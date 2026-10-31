//! The inbox — every conversation on every server, grouped by project
//! (`crate::inbox` decides) — and the list of projects. The conversations'
//! rows and the projects' avatars are shared with the project page.

use aiddiction_api::{PermissionAnswer, Project, ProjectState};
use gpui_adaptive::{Ask, Avatar, Banner, Collection, Command, Empty, ListRow, Screen, SectionHeader, Signal, Tone};
use gpui_adaptive::IconName;
use gpui_adaptive::prim::column;
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, SharedString};

use super::shell::{Page, Shell};
use crate::inbox::{self, Group, Listed};
use crate::settings::Settings;

impl Shell {
    /// Every conversation on every server, in the inbox's order.
    pub(crate) fn listed(&self, cx: &App) -> Vec<Listed> {
        let settings = Settings::get(cx);
        let showing = self.showing_session(cx);
        inbox::merge(self.servers.iter().map(|server| {
            let server = server.read(cx);
            let id = &server.entry.id;
            inbox::list(id, &server.projects, &server.instances, |session| settings.seen(id, session), showing.as_deref())
        }))
    }

    /// The inbox: every conversation, a section for each project.
    pub(super) fn render_inbox(&mut self, cx: &mut Context<Self>) -> Screen {
        let listed = self.listed(cx);
        // A server that does not answer has its conversations missing here:
        // say so rather than let them quietly vanish.
        let offline: Vec<Banner> = self
            .servers
            .iter()
            .filter_map(|server| {
                let server = server.read(cx);
                match &server.status {
                    crate::server::Status::Offline(error) => Some(
                        Banner::new(SharedString::from(format!("offline-{}", server.entry.id)), format!("{}: {error}", server.entry.name))
                            .tone(Tone::Danger)
                            .icon(IconName::CircleX),
                    ),
                    _ => None,
                }
            })
            .collect();
        let screen = Screen::new("inbox", "Inbox")
            .root(true)
            .command(self.new_conversation_command(cx))
            .banner((!offline.is_empty()).then(|| column().children(offline)));
        if listed.is_empty() {
            let empty = if self.servers.is_empty() {
                Empty::new(IconName::Inbox, "No server yet").body("Add the server your projects live on.")
            } else {
                Empty::new(IconName::Inbox, "No conversations yet")
                    .body("Start one with an agent in any of your projects: they all come here.")
            };
            return screen.scrolls(false).content(empty);
        }
        let mut list = Collection::new("inbox-list");
        let mut rest = listed.as_slice();
        while let Some(first) = rest.first() {
            let project = first.project.id;
            let count = rest.iter().take_while(|item| item.project.id == project).count();
            let (run, after) = rest.split_at(count);
            list = list.section(
                SectionHeader::new(first.project.name.clone()),
                run.iter().map(|item| self.session_row(item, true, cx)),
            );
            rest = after;
        }
        screen.content(list)
    }

    /// One conversation: its agent, its title, what it is doing, and the
    /// agent's question with its answers when it asks one.
    pub(super) fn session_row(&self, item: &Listed, show_server: bool, cx: &mut Context<Self>) -> ListRow {
        let selected = self.showing_session(cx).as_deref() == Some(item.session.session_id.as_str());
        let signal = match item.group {
            Group::NeedsYou if item.session.permissions.is_empty() => Some(Signal::Waiting(Tone::Danger)),
            Group::NeedsYou => Some(Signal::Waiting(Tone::Warning)),
            Group::Working => Some(Signal::Busy),
            Group::Unread => Some(Signal::New),
            Group::Recent | Group::Stopped => None,
        };
        let server = self.connection(&item.server, cx);
        let mut meta = item.instance.agent_name.clone();
        // With more than one server, which one it is on.
        if show_server && self.servers.len() > 1
            && let Some(server) = &server
        {
            meta = format!("{} · {meta}", server.read(cx).entry.name);
        }

        let project = item.project.clone();
        let instance = item.instance.clone();
        let session = item.session.session_id.clone();
        let stopped = item.group == Group::Stopped;
        let project_id = item.project.id;
        let shell = cx.entity().downgrade();
        let row = ListRow::new(SharedString::from(format!("session-{}", item.session.session_id)), item.title().to_owned())
            .leading(avatar(&item.project))
            .subtitle(meta)
            .trailing(inbox::ago(inbox::now_ms(), item.session.updated_at_ms))
            .signal(signal)
            .selected(selected)
            .unread(matches!(item.group, Group::Unread | Group::NeedsYou))
            .muted(stopped)
            .on_activate(move |window, cx| {
                shell
                    .update(cx, |shell, cx| {
                        if stopped {
                            shell.go(Page::Project(project_id), cx);
                        } else {
                            shell.open_chat(&project, &instance, Some(session.clone()), None, window, cx);
                        }
                    })
                    .ok();
            });

        if let Some(permission) = item.session.permissions.first() {
            let mut ask = Ask::new(SharedString::from(format!("ask-{}", item.session.session_id)), permission.title.clone());
            for option in permission.options.iter().take(3) {
                let server = server.clone();
                let answer = PermissionAnswer { request_id: permission.request_id.clone(), option_id: option.id.clone() };
                let instance = item.instance.id;
                let command = Command::new(
                    SharedString::from(format!("answer-{}-{}", item.session.session_id, option.id)),
                    option.name.clone(),
                )
                .on_trigger(move |_, cx| {
                    if let Some(server) = &server {
                        server.update(cx, |server, cx| server.answer_permission(instance, answer.clone(), cx));
                    }
                });
                ask = ask.answer(if option.kind.starts_with("allow") { command.primary() } else { command });
            }
            row.body(ask)
        } else if let (Group::NeedsYou, Some(error)) = (item.group, &item.session.error) {
            row.supporting(error.clone()).supporting_tone(Tone::Danger)
        } else if !item.session.preview.is_empty() {
            row.supporting(item.session.preview.clone())
        } else if stopped {
            row.supporting("The agent stopped")
        } else {
            row
        }
    }

    pub(super) fn render_projects(&mut self, cx: &mut Context<Self>) -> Screen {
        let shell = cx.entity().downgrade();
        let screen = Screen::new("projects", "Projects").root(true).command(
            Command::new("new-project", "New project").icon(IconName::Plus).primary().disabled(self.server.is_none()).on_trigger(
                move |window, cx| {
                    shell.update(cx, |shell, cx| shell.new_project(window, cx)).ok();
                },
            ),
        );
        let Some(server) = self.server.clone() else {
            return screen
                .scrolls(false)
                .content(Empty::new(IconName::Folder, "No server yet").body("Add the server your projects live on."));
        };
        let projects = server.read(cx).projects.clone();
        if projects.is_empty() {
            return screen.scrolls(false).content(
                Empty::new(IconName::Folder, "No projects yet")
                    .body("Make one: an empty folder, a git clone, or a folder already on the server."),
            );
        }
        let listed = self.listed(cx);
        let page = self.page(cx);
        let rows = projects.into_iter().map(|project| {
            let id = project.id;
            let of_project = || listed.iter().filter(move |item| item.project.id == id);
            let waiting = of_project().filter(|item| item.group == Group::NeedsYou).count();
            let working = of_project().filter(|item| item.group == Group::Working).count();
            let mut counts = Vec::new();
            if waiting > 0 {
                counts.push(if waiting == 1 { "1 needs you".to_owned() } else { format!("{waiting} need you") });
            }
            if working > 0 {
                counts.push(format!("{working} working"));
            }
            let total = of_project().count();
            if counts.is_empty() && total > 0 {
                counts.push(format!("{total} conversation{}", if total == 1 { "" } else { "s" }));
            }
            let status: SharedString = match project.state {
                ProjectState::Cloning => project.status_message.clone().unwrap_or_else(|| "Cloning…".into()).into(),
                ProjectState::Failed => "Could not be made".into(),
                ProjectState::Ready => counts.join(" · ").into(),
            };
            let selected = matches!(page, Page::Project(open) | Page::Review(open) if open == id);
            let shell = cx.entity().downgrade();
            ListRow::new(("project", id.as_u128() as u64), project.name.clone())
                .leading(avatar(&project))
                .subtitle(origin(&project))
                .when(!status.is_empty(), |row| {
                    let row = row.supporting(status);
                    if waiting > 0 { row.supporting_tone(Tone::Warning) } else { row }
                })
                .signal((project.state == ProjectState::Cloning).then_some(Signal::Busy))
                .selected(selected)
                .on_activate(move |_, cx| {
                    shell.update(cx, |shell, cx| shell.go(Page::Project(id), cx)).ok();
                })
        });
        screen.content(Collection::new("projects-list").children(rows))
    }
}

/// Where a project came from, in a line.
pub fn origin(project: &Project) -> SharedString {
    match (&project.path, &project.git_url) {
        (Some(path), _) => path.clone().into(),
        (None, Some(url)) => url.trim_start_matches("https://").trim_end_matches(".git").to_owned().into(),
        (None, None) => "Empty folder".into(),
    }
}

/// A project's letter on its own colour.
pub fn avatar(project: &Project) -> Avatar {
    let letter = project.name.chars().find(|c| c.is_alphanumeric()).unwrap_or('?').to_uppercase().collect::<String>();
    Avatar::new(("project-avatar", project.id.as_u128() as u64), letter).square().hue(inbox::hue(project.id))
}
