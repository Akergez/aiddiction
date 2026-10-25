//! A new conversation in one step: the project, the agent, and what to ask
//! it. Both lists are searchable, so a server with many projects and agents
//! is as quick as one with two.

use aiddiction_api::{Agent, Project, ProjectId};
use gpui_kit::component::form::{field, v_form};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::searchable_list::SearchableListItem;
use gpui_kit::component::select::{SearchableVec, Select, SelectState};
use gpui_kit::component::{ActiveTheme, IndexPath, h_flex};
use gpui_kit::Focusable as _;
use gpui_kit::prelude::*;
use gpui_kit::{App, AppContext, Context, Entity, FocusHandle, Render, SharedString, Window, div};

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

pub struct NewSessionForm {
    project: Entity<SelectState<SearchableVec<ProjectItem>>>,
    agent: Entity<SelectState<SearchableVec<AgentItem>>>,
    prompt: Entity<TextareaState>,
    pub error: Option<SharedString>,
}

impl NewSessionForm {
    fn new(projects: &[Project], agents: &[Agent], project: Option<ProjectId>, agent: Option<&str>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let project_items: Vec<_> = projects
            .iter()
            .map(|project| ProjectItem { id: project.id, name: project.name.clone().into(), origin: super::inbox::origin(project) })
            .collect();
        let chosen_project = project.and_then(|id| project_items.iter().position(|item| item.id == id)).unwrap_or(0);
        let agent_items: Vec<_> = agents.iter().map(|agent| AgentItem { id: agent.id.clone(), name: agent.name.clone().into() }).collect();
        let chosen_agent = agent.and_then(|id| agent_items.iter().position(|item| item.id == id)).unwrap_or(0);
        let has_projects = !project_items.is_empty();
        let has_agents = !agent_items.is_empty();
        let project = cx.new(|cx| {
            SelectState::new(SearchableVec::new(project_items), has_projects.then(|| IndexPath::default().row(chosen_project)), window, cx)
                .searchable(true)
        });
        let agent = cx.new(|cx| {
            SelectState::new(SearchableVec::new(agent_items), has_agents.then(|| IndexPath::default().row(chosen_agent)), window, cx)
                .searchable(true)
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
        Self { project, agent, prompt, error: None }
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
    /// conversation. The project on screen is offered first, and the agent
    /// used last.
    pub(super) fn new_conversation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(server) = self.server.clone() else { return };
        let (projects, agents) = {
            let server = server.read(cx);
            let ready = server.projects.iter().filter(|project| project.state == aiddiction_api::ProjectState::Ready).cloned().collect::<Vec<_>>();
            (ready, server.agents.clone())
        };
        let current = self.current_project(cx);
        let last_agent = Settings::get(cx).last_agent.clone();
        let form = cx.new(|cx| NewSessionForm::new(&projects, &agents, current, last_agent.as_deref(), window, cx));
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
