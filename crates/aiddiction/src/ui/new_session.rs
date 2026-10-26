//! A new conversation in one step: the project, the agent, and what to ask
//! it. The projects are every server's; the agents, the chosen project's
//! server's. Both lists are searchable, so many projects and agents are as
//! quick as two.

use std::collections::HashMap;

use aiddiction_api::{Agent, Project, ProjectId};
use gpui_kit::component::form::{field, v_form};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::searchable_list::SearchableListItem;
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_kit::component::{ActiveTheme, IndexPath, h_flex};
use gpui_kit::Focusable as _;
use gpui_kit::prelude::*;
use gpui_kit::{App, AppContext, Context, Entity, FocusHandle, Render, SharedString, Subscription, Window, div};

use super::forms::open_form;
use super::shell::Shell;
use crate::settings::Settings;

#[derive(Clone)]
struct ProjectItem {
    id: ProjectId,
    name: SharedString,
    origin: SharedString,
}

impl SearchableListItem for ProjectItem {
    type Value = ProjectId;

    fn title(&self) -> SharedString {
        self.name.clone()
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        h_flex()
            .w_full()
            .gap_2()
            .child(div().flex_none().child(self.name.clone()))
            .child(div().flex_1().min_w_0().truncate().text_xs().text_color(cx.theme().muted_foreground).child(self.origin.clone()))
    }

    fn value(&self) -> &ProjectId {
        &self.id
    }
}

#[derive(Clone)]
struct AgentItem {
    id: String,
    name: SharedString,
}

impl SearchableListItem for AgentItem {
    type Value = String;

    fn title(&self) -> SharedString {
        self.name.clone()
    }

    fn value(&self) -> &String {
        &self.id
    }
}

/// A project to offer: which server it is on (by name, when there are
/// several) and that server's agents.
pub struct Offered {
    pub project: Project,
    pub server: Option<String>,
    pub agents: Vec<Agent>,
}

pub struct NewSessionForm {
    project: Entity<SelectState<SearchableVec<ProjectItem>>>,
    agent: Entity<SelectState<SearchableVec<AgentItem>>>,
    /// Each project's server's agents, for when another project is picked.
    agents: HashMap<ProjectId, Vec<Agent>>,
    /// The agent to choose when there is a choice to make again.
    last_agent: Option<String>,
    prompt: Entity<TextareaState>,
    pub error: Option<SharedString>,
    _project_chosen: Subscription,
}

impl NewSessionForm {
    fn new(offered: Vec<Offered>, project: Option<ProjectId>, last_agent: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let project_items: Vec<_> = offered
            .iter()
            .map(|offered| {
                let origin = super::inbox::origin(&offered.project);
                ProjectItem {
                    id: offered.project.id,
                    name: offered.project.name.clone().into(),
                    origin: match &offered.server {
                        Some(server) => format!("{server} · {origin}").into(),
                        None => origin,
                    },
                }
            })
            .collect();
        let chosen_project = project.and_then(|id| project_items.iter().position(|item| item.id == id)).unwrap_or(0);
        let agents: HashMap<_, _> = offered.into_iter().map(|offered| (offered.project.id, offered.agents)).collect();
        let first_agents = project_items.get(chosen_project).and_then(|item| agents.get(&item.id)).cloned().unwrap_or_default();
        let (first_items, chosen_agent) = agent_items(&first_agents, last_agent.as_deref());
        let has_projects = !project_items.is_empty();
        let project = cx.new(|cx| {
            SelectState::new(SearchableVec::new(project_items), has_projects.then(|| IndexPath::default().row(chosen_project)), window, cx)
                .searchable(true)
        });
        let agent = cx.new(|cx| SelectState::new(SearchableVec::new(first_items), chosen_agent, window, cx).searchable(true));
        let project_chosen = cx.subscribe_in(&project, window, |form, _, event: &SelectEvent<SearchableVec<ProjectItem>>, window, cx| {
            let SelectEvent::Confirm(Some(project)) = event else { return };
            let agents = form.agents.get(project).cloned().unwrap_or_default();
            let (items, chosen) = agent_items(&agents, form.last_agent.as_deref());
            form.agent.update(cx, |agent, cx| {
                agent.set_items(SearchableVec::new(items), window, cx);
                if let Some(chosen) = chosen {
                    agent.set_selected_index(Some(chosen), window, cx);
                }
            });
        });
        let prompt = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 10)
                // Enter starts; Shift+Enter is a new line, as in a conversation.
                .submit_on_enter(true)
                .placeholder("What should the agent do?")
        });
        // Enter reaches the dialog, which starts the conversation.
        super::keyboard_for(&prompt, gpui_android_glue::keyboard::Purpose::Message, cx);
        Self { project, agent, agents, last_agent, prompt, error: None, _project_chosen: project_chosen }
    }

    fn first(&self, cx: &App) -> FocusHandle {
        self.prompt.read(cx).focus_handle(cx)
    }

    /// The project, the agent and the first message, or why there are none.
    fn choice(&self, cx: &App) -> Result<(ProjectId, String, String), String> {
        let project = self.project.read(cx).selected_value().copied().ok_or("Pick a project.")?;
        let agent = self.agent.read(cx).selected_value().cloned().ok_or("Pick an agent.")?;
        Ok((project, agent, self.prompt.read(cx).value().trim().to_owned()))
    }
}

/// A server's agents as a list, and which of them to choose: `last` if it
/// is there, else the first.
fn agent_items(agents: &[Agent], last: Option<&str>) -> (Vec<AgentItem>, Option<IndexPath>) {
    let items: Vec<_> = agents.iter().map(|agent| AgentItem { id: agent.id.clone(), name: agent.name.clone().into() }).collect();
    let chosen = (!items.is_empty()).then(|| IndexPath::default().row(last.and_then(|id| items.iter().position(|item| item.id == id)).unwrap_or(0)));
    (items, chosen)
}

impl Render for NewSessionForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_form()
            .child(field().label("Project").child(Select::new(&self.project).search_placeholder("Find a project").placeholder("No projects yet")))
            .child(field().label("Agent").child(Select::new(&self.agent).search_placeholder("Find an agent").placeholder("No agents")))
            .child(field().label("Message").child(gpui_android_glue::keyboard::on_tap(Textarea::new(&self.prompt), &self.prompt, cx)))
            .child(field().label_indent(false).child(
                div().when_some(self.error.clone(), |line, error| line.text_sm().text_color(cx.theme().danger).child(error)),
            ))
    }
}

impl Shell {
    /// Asks for a project, an agent and a first message, and starts the
    /// conversation. Every server's projects are offered: the one on screen
    /// first, else the server in use's; and the agent used last.
    pub(super) fn new_conversation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.servers.is_empty() {
            return;
        }
        let several = self.servers.len() > 1;
        // The server in use's projects come first.
        let mut servers = self.servers.clone();
        if let Some(active) = &self.server {
            servers.sort_by_key(|server| server != active);
        }
        let mut offered = Vec::new();
        for server in &servers {
            let server = server.read(cx);
            for project in server.projects.iter().filter(|project| project.state == aiddiction_api::ProjectState::Ready) {
                offered.push(Offered {
                    project: project.clone(),
                    server: several.then(|| server.entry.name.clone()),
                    agents: server.agents.clone(),
                });
            }
        }
        let current = self.current_project(cx);
        let last_agent = Settings::get(cx).last_agent.clone();
        let form = cx.new(|cx| NewSessionForm::new(offered, current, last_agent, window, cx));
        let first = form.read(cx).first(cx);
        let shell = cx.entity().downgrade();
        open_form("New conversation", "Start", form, Some(first), move |form, window, cx| {
            match form.choice(cx) {
                Ok((project, agent, prompt)) => {
                    let _ = shell.update(cx, |shell, cx| shell.start_chat(project, agent, Some(prompt), window, cx));
                    true
                }
                Err(error) => {
                    form.error = Some(error.into());
                    cx.notify();
                    false
                }
            }
        }, window, cx);
    }
}
