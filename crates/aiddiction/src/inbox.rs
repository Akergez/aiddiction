//! Every conversation on every server this device knows, in one list in the
//! order a person wants to see them: the ones waiting on them first, then the
//! ones at work, then what finished unread, then the rest; in each of these,
//! a project's conversations together. Pure, so the rules are tested here;
//! the inbox page (`ui/inbox.rs`) only draws the result.

use std::collections::HashMap;

use aiddiction_api::{Instance, InstanceState, Project, ProjectId, SessionSummary};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    /// The agent asks something, or its turn failed and nobody has looked.
    NeedsYou,
    Working,
    /// A turn ended after the person last looked.
    Unread,
    Recent,
    /// The agent is no longer running.
    Stopped,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Group::NeedsYou => "Needs you",
            Group::Working => "Working",
            Group::Unread => "Done",
            Group::Recent => "Recent",
            Group::Stopped => "Stopped",
        }
    }
}

/// One conversation, with what a row needs to show it.
#[derive(Debug, Clone)]
pub struct Listed {
    /// The server's id in the settings.
    pub server: String,
    pub project: Project,
    pub instance: Instance,
    pub session: SessionSummary,
    pub group: Group,
}

impl Listed {
    pub fn title(&self) -> &str {
        if self.session.title.is_empty() { "Untitled conversation" } else { &self.session.title }
    }
}

/// The conversations of one server's `instances`, grouped, a project's
/// together in each group. `seen` says how far a conversation was read;
/// `showing` is the one on screen, which is never unread.
pub fn list(
    server: &str,
    projects: &[Project],
    instances: &HashMap<ProjectId, Vec<Instance>>,
    seen: impl Fn(&str) -> u64,
    showing: Option<&str>,
) -> Vec<Listed> {
    let mut listed = Vec::new();
    for project in projects {
        for instance in instances.get(&project.id).into_iter().flatten() {
            for session in &instance.sessions {
                let read = showing == Some(session.session_id.as_str()) || session.last_seq <= seen(&session.session_id);
                listed.push(Listed {
                    server: server.to_owned(),
                    project: project.clone(),
                    instance: instance.clone(),
                    session: session.clone(),
                    group: group(instance.state, session, read),
                });
            }
        }
    }
    sort(&mut listed);
    listed
}

/// Several servers' lists as one: what needs the person first, whichever
/// server it is on.
pub fn merge(lists: impl IntoIterator<Item = Vec<Listed>>) -> Vec<Listed> {
    let mut listed: Vec<_> = lists.into_iter().flatten().collect();
    sort(&mut listed);
    listed
}

/// By group; in a group, a project's conversations together, the project
/// with the latest first, and newest first within it.
fn sort(listed: &mut [Listed]) {
    let mut latest: HashMap<(Group, ProjectId), u64> = HashMap::new();
    for item in listed.iter() {
        let latest = latest.entry((item.group, item.project.id)).or_default();
        *latest = (*latest).max(item.session.updated_at_ms);
    }
    listed.sort_by(|a, b| {
        a.group.cmp(&b.group)
            .then(latest[&(b.group, b.project.id)].cmp(&latest[&(a.group, a.project.id)]))
            .then(a.project.id.cmp(&b.project.id))
            .then(b.session.updated_at_ms.cmp(&a.session.updated_at_ms))
    });
}

pub fn group(state: InstanceState, session: &SessionSummary, read: bool) -> Group {
    if matches!(state, InstanceState::Exited | InstanceState::Failed) {
        Group::Stopped
    } else if !session.permissions.is_empty() || (session.error.is_some() && !read) {
        Group::NeedsYou
    } else if session.busy {
        Group::Working
    } else if !read {
        Group::Unread
    } else {
        Group::Recent
    }
}

/// How long ago `then` was, the way a list says it: `now`, `5m`, `3h`,
/// `2d`, then weeks.
pub fn ago(now_ms: u64, then_ms: u64) -> String {
    let seconds = now_ms.saturating_sub(then_ms) / 1000;
    match seconds {
        0..60 => "now".into(),
        60..3600 => format!("{}m", seconds / 60),
        3600..86400 => format!("{}h", seconds / 3600),
        86400..604800 => format!("{}d", seconds / 86400),
        _ => format!("{}w", seconds / 604800),
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The hue of a project's avatar, the same on every device: the project is
/// told apart by it at a glance.
pub fn hue(project: ProjectId) -> f32 {
    const HUES: [f32; 8] = [0.0, 0.08, 0.14, 0.33, 0.47, 0.55, 0.68, 0.82];
    HUES[(project.as_u128() % HUES.len() as u128) as usize]
}

#[cfg(test)]
mod tests {
    use aiddiction_api::{PendingPermission, ProjectState};
    use serde_json::json;

    use super::*;

    fn session(id: &str, updated: u64, last_seq: u64) -> SessionSummary {
        SessionSummary {
            session_id: id.into(),
            title: id.into(),
            created_at_ms: 0,
            updated_at_ms: updated,
            last_seq,
            busy: false,
            permissions: Vec::new(),
            preview: String::new(),
            error: None,
        }
    }

    fn project(name: &str) -> Project {
        Project {
            id: uuid::Uuid::now_v7(),
            name: name.into(),
            dir_name: name.into(),
            path: None,
            state: ProjectState::Ready,
            git_url: None,
            status_message: None,
            created_at_ms: 0,
        }
    }

    fn instance(project: &Project, state: InstanceState, sessions: Vec<SessionSummary>) -> Instance {
        Instance {
            id: uuid::Uuid::now_v7(),
            project_id: project.id,
            agent_id: "fake".into(),
            agent_name: "Fake".into(),
            state,
            message: None,
            sessions,
            stderr_tail: Vec::new(),
        }
    }

    #[test]
    fn conversations_from_every_project_come_in_one_list_by_what_they_need() {
        let (web, api) = (project("web"), project("api"));
        let mut asking = session("asking", 1, 5);
        asking.permissions.push(PendingPermission { request_id: json!(1), title: "Run tests".into(), options: Vec::new() });
        let mut working = session("working", 9, 5);
        working.busy = true;
        let mut failed = session("failed", 2, 5);
        failed.error = Some("boom".into());
        let instances = HashMap::from([
            (web.id, vec![instance(&web, InstanceState::Ready, vec![session("old", 3, 5), asking, working])]),
            (api.id, vec![
                instance(&api, InstanceState::Ready, vec![session("fresh", 8, 9), failed]),
                instance(&api, InstanceState::Exited, vec![session("gone", 7, 1)]),
            ]),
        ]);
        let seen = |id: &str| if id == "fresh" { 4 } else { 5 };
        let order: Vec<_> = list("home", &[web, api], &instances, seen, None)
            .into_iter()
            .map(|listed| (listed.session.session_id, listed.group))
            .collect();
        assert_eq!(
            order,
            [
                ("asking".to_owned(), Group::NeedsYou),
                ("working".to_owned(), Group::Working),
                ("fresh".to_owned(), Group::Unread),
                ("old".to_owned(), Group::Recent),
                ("failed".to_owned(), Group::Recent),
                ("gone".to_owned(), Group::Stopped),
            ]
        );
    }

    #[test]
    fn a_failed_turn_needs_you_until_it_is_read_and_the_one_on_screen_is_read() {
        let mut failed = session("s", 1, 7);
        failed.error = Some("boom".into());
        assert_eq!(group(InstanceState::Ready, &failed, false), Group::NeedsYou);
        assert_eq!(group(InstanceState::Ready, &failed, true), Group::Recent);
        let web = project("web");
        let instances = HashMap::from([(web.id, vec![instance(&web, InstanceState::Ready, vec![session("s", 1, 7)])])]);
        assert_eq!(list("home", &[web], &instances, |_| 0, Some("s"))[0].group, Group::Recent);
    }

    #[test]
    fn in_a_group_a_projects_conversations_come_together() {
        let (web, api) = (project("web"), project("api"));
        let instances = HashMap::from([
            (web.id, vec![instance(&web, InstanceState::Ready, vec![session("web-old", 1, 5), session("web-new", 5, 5)])]),
            (api.id, vec![instance(&api, InstanceState::Ready, vec![session("api", 3, 5)])]),
        ]);
        let order: Vec<_> = list("home", &[api, web], &instances, |_| 5, None).into_iter().map(|listed| listed.session.session_id).collect();
        assert_eq!(order, ["web-new", "web-old", "api"]);
    }

    #[test]
    fn every_server_comes_in_one_list_by_what_it_needs() {
        let (web, api) = (project("web"), project("api"));
        let mut asking = session("asking", 1, 5);
        asking.permissions.push(PendingPermission { request_id: json!(1), title: "Run tests".into(), options: Vec::new() });
        let home = HashMap::from([(web.id, vec![instance(&web, InstanceState::Ready, vec![session("old", 3, 5)])])]);
        let work = HashMap::from([(api.id, vec![instance(&api, InstanceState::Ready, vec![asking, session("new", 9, 5)])])]);
        let order: Vec<_> = merge([
            list("home", std::slice::from_ref(&web), &home, |_| 5, None),
            list("work", std::slice::from_ref(&api), &work, |_| 5, None),
        ])
        .into_iter()
        .map(|listed| (listed.server, listed.session.session_id))
        .collect();
        let pair = |server: &str, session: &str| (server.to_owned(), session.to_owned());
        assert_eq!(order, [pair("work", "asking"), pair("work", "new"), pair("home", "old")]);
    }

    #[test]
    fn time_ago_is_short() {
        let minute = 60_000;
        assert_eq!(ago(10 * minute, 10 * minute - 5000), "now");
        assert_eq!(ago(100 * minute, 95 * minute), "5m");
        assert_eq!(ago(1000 * minute, 820 * minute), "3h");
        assert_eq!(ago(10_000 * minute, 10_000 * minute - 3 * 1440 * minute), "3d");
    }
}
