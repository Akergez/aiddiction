//! A new conversation in one step: the project, the agent, and what to ask
//! it. The projects are every server's; the agents, the chosen project's
//! server's. Both lists are searchable, so many projects and agents are as
//! quick as two.

use std::collections::HashMap;

use aiddiction_api::{Agent, Project, ProjectId};
use gpui_adaptive::{Choice, ChoiceEvent, ChoiceOption, ChoiceState, Fields, Note, TextField, TextFieldState, Tone};
use gpui_kit::Focusable as _;
use gpui_kit::prelude::*;
use gpui_kit::{App, AppContext, Context, Entity, FocusHandle, Render, SharedString, Subscription, Window};

use super::forms::open_form;
use super::shell::Shell;
use crate::settings::Settings;

/// A project to offer: which server it is on (by name, when there are
/// several) and that server's agents.
pub struct Offered {
    pub project: Project,
    pub server: Option<String>,
    pub agents: Vec<Agent>,
}

pub struct NewSessionForm {
    project: Entity<ChoiceState>,
    agent: Entity<ChoiceState>,
    /// Each project's id as its option's key, and its server's agents, for
    /// when another project is picked.
    projects: HashMap<SharedString, (ProjectId, Vec<Agent>)>,
    /// The agent to choose when there is a choice to make again.
    last_agent: Option<String>,
    prompt: Entity<TextFieldState>,
    pub error: Option<SharedString>,
    _project_chosen: Subscription,
}

impl NewSessionForm {
    fn new(offered: Vec<Offered>, project: Option<ProjectId>, last_agent: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let options: Vec<ChoiceOption> = offered
            .iter()
            .map(|offered| {
                let origin = super::inbox::origin(&offered.project);
                ChoiceOption::new(offered.project.id.to_string(), offered.project.name.clone()).detail(match &offered.server {
                    Some(server) => SharedString::from(format!("{server} · {origin}")),
                    None => origin,
                })
            })
            .collect();
        let chosen_project = project
            .map(|id| SharedString::from(id.to_string()))
            .filter(|key| options.iter().any(|option| option.key() == key))
            .or_else(|| options.first().map(|option| option.key().clone()));
        let projects: HashMap<_, _> =
            offered.into_iter().map(|offered| (SharedString::from(offered.project.id.to_string()), (offered.project.id, offered.agents))).collect();
        let first_agents = chosen_project.as_ref().and_then(|key| projects.get(key)).map(|(_, agents)| agents.clone()).unwrap_or_default();
        let (first_options, chosen_agent) = agent_options(&first_agents, last_agent.as_deref());
        let project = cx.new(|cx| ChoiceState::searchable(options, chosen_project, window, cx));
        let agent = cx.new(|cx| ChoiceState::searchable(first_options, chosen_agent, window, cx));
        let project_chosen = cx.subscribe_in(&project, window, |form, _, event: &ChoiceEvent, window, cx| {
            let ChoiceEvent::Change(key) = event;
            let agents = form.projects.get(key).map(|(_, agents)| agents.clone()).unwrap_or_default();
            let (options, chosen) = agent_options(&agents, form.last_agent.as_deref());
            form.agent.update(cx, |agent, cx| agent.set_options(options, chosen, window, cx));
        });
        // On a pointer Enter starts, as in a conversation.
        let prompt = cx.new(|cx| {
            let mut prompt = TextFieldState::prompt(3, 10, window, cx);
            prompt.set_placeholder("What should the agent do?", window, cx);
            prompt
        });
        Self { project, agent, projects, last_agent, prompt, error: None, _project_chosen: project_chosen }
    }

    fn first(&self, cx: &App) -> FocusHandle {
        self.prompt.read(cx).focus_handle(cx)
    }

    /// The project, the agent and the first message, or why there are none.
    fn choice(&self, cx: &App) -> Result<(ProjectId, String, String), String> {
        let key = self.project.read(cx).selected().ok_or("Pick a project.")?;
        let project = self.projects.get(key).map(|(id, _)| *id).ok_or("Pick a project.")?;
        let agent = self.agent.read(cx).selected().map(|key| key.to_string()).ok_or("Pick an agent.")?;
        Ok((project, agent, self.prompt.read(cx).value(cx).trim().to_owned()))
    }
}

/// A server's agents as options, and which of them to choose: `last` if it
/// is there, else the first.
fn agent_options(agents: &[Agent], last: Option<&str>) -> (Vec<ChoiceOption>, Option<SharedString>) {
    let options: Vec<_> = agents.iter().map(|agent| ChoiceOption::new(agent.id.clone(), agent.name.clone())).collect();
    let chosen = last
        .filter(|last| agents.iter().any(|agent| agent.id == *last))
        .map(|last| SharedString::from(last.to_owned()))
        .or_else(|| options.first().map(|option| option.key().clone()));
    (options, chosen)
}

impl Render for NewSessionForm {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Fields::new()
            .child(Choice::new(&self.project).label("Project").placeholder("No projects yet"))
            .child(Choice::new(&self.agent).label("Agent").placeholder("No agents"))
            .child(TextField::new(&self.prompt).label("Message"))
            .children(self.error.clone().map(|error| Note::new(error).tone(Tone::Danger)))
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
