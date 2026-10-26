//! The server in use, as the interface sees it: its projects, agents and
//! running instances, kept fresh by the server's event socket.
//!
//! The one owner of the [`ServerClient`]: screens read this entity and call
//! its methods; nothing else talks REST.

use std::collections::HashMap;
use std::time::Duration;

use aiddiction_api::{
    Agent, AgentInput, Instance, InstanceId, NewProject, PermissionAnswer, Project, ProjectId, ServerEvent, ServerInfo, Task,
    TaskId, TaskInput, TaskStatus,
};
use aiddiction_client::{Error, ServerClient};
use gpui_kit::{App, Context};

use crate::runtime;
use crate::settings::ServerEntry;

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Connecting,
    Online,
    Offline(String),
}

pub struct ServerConnection {
    pub entry: ServerEntry,
    client: Option<ServerClient>,
    pub status: Status,
    pub info: Option<ServerInfo>,
    pub projects: Vec<Project>,
    pub agents: Vec<Agent>,
    pub instances: HashMap<ProjectId, Vec<Instance>>,
    pub tasks: HashMap<ProjectId, Vec<Task>>,
    /// Bumped on every (re)connection, so a stale event loop stops.
    generation: u64,
}

impl ServerConnection {
    pub fn new(entry: ServerEntry, cx: &mut Context<Self>) -> Self {
        let (client, status) = match ServerClient::new(&entry.url, &entry.token) {
            Ok(client) => (Some(client), Status::Connecting),
            Err(error) => (None, Status::Offline(error.to_string())),
        };
        let mut this = Self {
            entry,
            client,
            status,
            info: None,
            projects: Vec::new(),
            agents: Vec::new(),
            instances: HashMap::new(),
            tasks: HashMap::new(),
            generation: 0,
        };
        this.connect(cx);
        this
    }

    pub fn client(&self) -> Option<&ServerClient> {
        self.client.as_ref()
    }

    pub fn project(&self, id: ProjectId) -> Option<&Project> {
        self.projects.iter().find(|project| project.id == id)
    }

    /// Checks the server, reads everything, and listens for changes.
    pub fn connect(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        self.generation += 1;
        let generation = self.generation;
        self.status = Status::Connecting;
        cx.notify();
        let this = cx.entity().downgrade();
        runtime::spawn(async move { client.check().await }, move |result, cx| {
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match result {
                    Ok(info) => {
                        this.info = Some(info);
                        this.status = Status::Online;
                        this.refresh_projects(cx);
                        this.refresh_agents(cx);
                        this.refresh_all_instances(cx);
                        this.listen(generation, cx);
                    }
                    Err(error) => this.went_offline(error, generation, cx),
                }
                cx.notify();
            });
        });
    }

    fn went_offline(&mut self, error: Error, generation: u64, cx: &mut Context<Self>) {
        self.status = Status::Offline(error.to_string());
        cx.notify();
        // A wrong token stays wrong; anything else is worth another try.
        if error.is_unauthorized() {
            return;
        }
        let this = cx.entity().downgrade();
        runtime::after(Duration::from_secs(5), move |cx| {
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.connect(cx);
                }
            });
        });
    }

    fn listen(&mut self, generation: u64, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let this = cx.entity().downgrade();
        runtime::spawn(async move { client.events().await }, move |result, cx| {
            let Ok(events) = result else {
                let _ = this.update(cx, |this, cx| {
                    if this.generation == generation {
                        this.went_offline(Error::Network("the event socket closed".into()), generation, cx);
                    }
                });
                return;
            };
            let on_event = this.clone();
            runtime::receive(
                events,
                move |event, cx| {
                    on_event
                        .update(cx, |this, cx| {
                            if this.generation != generation {
                                return false;
                            }
                            this.handle(event, cx);
                            true
                        })
                        .unwrap_or(false)
                },
                move |cx| {
                    let _ = this.update(cx, |this, cx| {
                        if this.generation == generation {
                            this.went_offline(Error::Network("lost the server".into()), generation, cx);
                        }
                    });
                },
            );
        });
    }

    fn handle(&mut self, event: ServerEvent, cx: &mut Context<Self>) {
        match event {
            // Also what a lagging event socket is told: everything is read again.
            ServerEvent::ProjectsChanged => {
                self.refresh_projects(cx);
                self.refresh_all_instances(cx);
            }
            ServerEvent::AgentsChanged => self.refresh_agents(cx),
            ServerEvent::InstancesChanged { project_id } => self.refresh_instances(project_id, cx),
            ServerEvent::TasksChanged { project_id } => self.refresh_tasks(project_id, cx),
        }
    }

    /// Runs a call on Tokio and hands its result to `on_done` on this entity.
    fn call<T, F>(
        &self,
        cx: &mut Context<Self>,
        call: impl FnOnce(ServerClient) -> F,
        on_done: impl FnOnce(&mut Self, Result<T, Error>, &mut Context<Self>) + 'static,
    ) where
        F: std::future::Future<Output = Result<T, Error>> + Send + 'static,
        T: Send + 'static,
    {
        let Some(client) = self.client.clone() else {
            return;
        };
        let future = call(client);
        let this = cx.entity().downgrade();
        runtime::spawn(future, move |result, cx| {
            let _ = this.update(cx, |this, cx| {
                on_done(this, result, cx);
                cx.notify();
            });
        });
    }

    pub fn refresh_projects(&mut self, cx: &mut Context<Self>) {
        self.call(cx, |client| async move { client.projects().await }, |this, result, cx| match result {
            Ok(mut projects) => {
                projects.sort_by_key(|project| std::cmp::Reverse(project.created_at_ms));
                this.projects = projects;
                this.refresh_all_tasks(cx);
            }
            Err(error) => tracing::warn!("could not read the projects: {error}"),
        });
    }

    pub fn refresh_agents(&mut self, cx: &mut Context<Self>) {
        self.call(cx, |client| async move { client.agents().await }, |this, result, _| match result {
            Ok(agents) => this.agents = agents,
            Err(error) => tracing::warn!("could not read the agents: {error}"),
        });
    }

    /// Every project's instances, as on (re)connecting: events only say
    /// what changes after that.
    pub fn refresh_all_instances(&mut self, cx: &mut Context<Self>) {
        self.call(cx, |client| async move { client.all_instances().await }, |this, result, _| match result {
            Ok(instances) => {
                this.instances.clear();
                for instance in instances {
                    this.instances.entry(instance.project_id).or_default().push(instance);
                }
            }
            Err(error) => tracing::warn!("could not read the instances: {error}"),
        });
    }

    /// Answers an agent's question from a list, without its conversation.
    pub fn answer_permission(&mut self, instance: InstanceId, answer: PermissionAnswer, cx: &mut Context<Self>) {
        // Shown answered at once; the server's event brings the truth.
        for instances in self.instances.values_mut() {
            for known in instances.iter_mut().filter(|known| known.id == instance) {
                for session in &mut known.sessions {
                    session.permissions.retain(|permission| permission.request_id != answer.request_id);
                }
            }
        }
        cx.notify();
        self.call(cx, move |client| async move { client.answer_permission(instance, &answer).await }, |_, result, cx| {
            if let Err(error) = result
                && !matches!(error, Error::Api { status: 409, .. })
            {
                crate::ui::toast_error(format!("Could not answer the agent: {error}"), cx);
            }
        });
    }

    pub fn refresh_instances(&mut self, project: ProjectId, cx: &mut Context<Self>) {
        self.call(cx, move |client| async move { client.instances(project).await }, move |this, result, _| {
            match result {
                Ok(instances) => {
                    this.instances.insert(project, instances);
                }
                Err(error) => tracing::warn!("could not read the instances: {error}"),
            }
        });
    }

    /// Every known project's tasks: called once the projects are read, since
    /// before that there is no project to ask about.
    pub fn refresh_all_tasks(&mut self, cx: &mut Context<Self>) {
        let projects = self.projects.iter().map(|p| p.id).collect::<Vec<_>>();
        self.tasks.retain(|project, _| projects.contains(project));
        for project in projects {
            self.refresh_tasks(project, cx);
        }
    }

    pub fn refresh_tasks(&mut self, project: ProjectId, cx: &mut Context<Self>) {
        self.call(cx, move |client| async move { client.tasks(project).await }, move |this, result, _| match result {
            Ok(mut tasks) => {
                tasks.sort_by_key(|task| task.created_at_ms);
                this.tasks.insert(project, tasks);
            }
            Err(error) => tracing::warn!("could not read the tasks: {error}"),
        });
    }

    /// Creates a card (`id` is `None`) or replaces one's title, text and status.
    pub fn save_task(
        &mut self,
        project: ProjectId,
        id: Option<TaskId>,
        input: TaskInput,
        cx: &mut Context<Self>,
        done: impl FnOnce(Result<Task, Error>, &mut App) + 'static,
    ) {
        self.call(
            cx,
            move |client| async move {
                match id {
                    Some(id) => client.update_task(project, id, &input).await,
                    None => client.create_task(project, &input).await,
                }
            },
            move |this, result, cx| {
                // Shown at once; the server's `TasksChanged` brings the rest.
                if let Ok(task) = &result {
                    let tasks = this.tasks.entry(project).or_default();
                    match tasks.iter_mut().find(|known| known.id == task.id) {
                        Some(known) => *known = task.clone(),
                        None => tasks.push(task.clone()),
                    }
                }
                // After this update, so `done` may update this entity again.
                cx.defer(move |cx| done(result, cx));
            },
        );
    }

    /// Moves a card to another column.
    pub fn move_task(&mut self, task: &Task, status: TaskStatus, cx: &mut Context<Self>) {
        let input = TaskInput { title: task.title.clone(), markdown: task.markdown.clone(), status: Some(status) };
        self.save_task(task.project_id, Some(task.id), input, cx, |result, cx| {
            if let Err(error) = result {
                crate::ui::toast_error(format!("Could not move the task: {error}"), cx);
            }
        });
    }

    pub fn delete_task(&mut self, project: ProjectId, id: TaskId, cx: &mut Context<Self>) {
        self.call(cx, move |client| async move { client.delete_task(project, id).await }, move |this, result, cx| {
            match result {
                Ok(()) => {
                    if let Some(tasks) = this.tasks.get_mut(&project) {
                        tasks.retain(|task| task.id != id);
                    }
                }
                Err(error) => crate::ui::toast_error(format!("Could not delete the task: {error}"), cx),
            }
        });
    }

    pub fn create_project(
        &mut self,
        input: NewProject,
        cx: &mut Context<Self>,
        done: impl FnOnce(Result<Project, Error>, &mut App) + 'static,
    ) {
        self.call(cx, move |client| async move { client.create_project(&input).await }, move |this, result, cx| {
            if let Ok(project) = &result
                && !this.projects.iter().any(|known| known.id == project.id) {
                    this.projects.insert(0, project.clone());
                }
            // After this update, so `done` may update this entity again.
            cx.defer(move |cx| done(result, cx));
        });
    }

    pub fn delete_project(&mut self, id: ProjectId, delete_files: bool, cx: &mut Context<Self>) {
        self.call(cx, move |client| async move { client.delete_project(id, delete_files).await }, move |this, result, cx| {
            match result {
                Ok(()) => this.projects.retain(|project| project.id != id),
                Err(error) => crate::ui::toast_error(format!("Could not delete the project: {error}"), cx),
            }
        });
    }

    pub fn save_agent(
        &mut self,
        id: Option<String>,
        input: AgentInput,
        cx: &mut Context<Self>,
        done: impl FnOnce(Result<Agent, Error>, &mut App) + 'static,
    ) {
        self.call(
            cx,
            move |client| async move {
                match id {
                    Some(id) => client.update_agent(&id, &input).await,
                    None => client.add_agent(&input).await,
                }
            },
            move |this, result, cx| {
                this.refresh_agents(cx);
                // After this update, so `done` may update this entity again.
            cx.defer(move |cx| done(result, cx));
            },
        );
    }

    pub fn remove_agent(&mut self, id: String, cx: &mut Context<Self>) {
        self.call(cx, move |client| async move { client.remove_agent(&id).await }, |this, result, cx| {
            if let Err(error) = result {
                crate::ui::toast_error(format!("Could not remove the agent: {error}"), cx);
            }
            this.refresh_agents(cx);
        });
    }

    pub fn start_instance(
        &mut self,
        project: ProjectId,
        agent_id: String,
        cx: &mut Context<Self>,
        done: impl FnOnce(Result<Instance, Error>, &mut App) + 'static,
    ) {
        self.call(
            cx,
            move |client| async move { client.start_instance(project, &agent_id).await },
            move |this, result, cx| {
                this.refresh_instances(project, cx);
                // After this update, so `done` may update this entity again.
            cx.defer(move |cx| done(result, cx));
            },
        );
    }

    pub fn stop_instance(&mut self, instance: &Instance, cx: &mut Context<Self>) {
        let (id, project) = (instance.id, instance.project_id);
        self.call(cx, move |client| async move { client.stop_instance(id).await }, move |this, result, cx| {
            if let Err(error) = result {
                crate::ui::toast_error(format!("Could not stop the agent: {error}"), cx);
            }
            this.refresh_instances(project, cx);
        });
    }
}
