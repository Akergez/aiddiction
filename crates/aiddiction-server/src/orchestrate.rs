//! Starting an agent from MCP: the server is the ACP client itself, the
//! session is an ordinary one in the common list of conversations, and the
//! child is given a token of its own for the board, limited to its project,
//! so what it finds and does goes back to the card.
//!
//! The caller gets the ids of the project, the card, the instance and the
//! thread once the prompt is sent, not when the turn ends: it follows the
//! thread with `list_subagents` and reads the result on the card.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aiddiction_api::{
    InstanceId, InstanceState, Project, ProjectId, ProjectState, SessionSummary, Task, TaskId,
    TaskSession, WorktreeId,
};
use serde_json::{Value, json};

use crate::agents::Catalog;
use crate::instances::{Instance, Supervisor};
use crate::projects::Projects;
use crate::proxy::now_ms;
use crate::worktrees::{WorktreeError, Worktrees};

/// `npx` may fetch an adapter on first use, as when a client starts one.
const READY_TIMEOUT: Duration = Duration::from_secs(180);

/// How long an agent is waited for to take a goal: an adapter without the
/// extension refuses at once, one that takes it may answer only when its
/// turn is over, and that is no reason to keep the caller waiting.
const GOAL_ANSWER_TIMEOUT: Duration = Duration::from_secs(15);

/// Who is asking: the person (the server's own token) or an agent the
/// server started, whose token is bound to one project.
#[derive(Debug, Clone)]
pub enum Caller {
    User,
    Agent(AgentScope),
}

/// Who an orchestrated agent is, as far as its token lets it be: one
/// project's board, and the card it was started on.
#[derive(Debug, Clone)]
pub struct AgentScope {
    pub token: String,
    pub project: ProjectId,
    pub task: Option<TaskId>,
    pub instance: InstanceId,
    pub session: Option<String>,
}

/// What the caller of `start_agent` asks for.
#[derive(Debug, Clone)]
pub struct StartRequest {
    pub project: ProjectId,
    pub agent_id: String,
    pub task_id: Option<TaskId>,
    /// The checkout to work in: one the server made, by id.
    pub worktree_id: Option<WorktreeId>,
    /// Or a checkout the person made, by folder: it is adopted on the way
    /// and the answer names the id it got.
    pub worktree_path: Option<String>,
    pub prompt: String,
    /// An objective for the agent to pursue, when its adapter has the goal
    /// extension (`_session/goal`); it is the turn, and the prompt, when
    /// there is one, is the first part of it.
    pub goal: Option<String>,
    /// ACP configuration option ids and values, applied in this order.
    pub settings: Vec<(String, Value)>,
    /// Ties a retry to the call it repeats, so a lost answer does not start
    /// a second agent.
    pub request_id: Option<String>,
}

/// What the caller of `send_message` asks for: another turn of a conversation
/// that is already running, instead of a new agent.
#[derive(Debug, Clone)]
pub struct SendRequest {
    /// The project named by the caller, when it named one.
    pub project: Option<ProjectId>,
    /// The instance named by the caller, when the session id is not enough.
    pub instance: Option<InstanceId>,
    pub session: String,
    pub prompt: String,
    /// The card to work on now; the conversation's own card when left out.
    pub task_id: Option<TaskId>,
    pub request_id: Option<String>,
}

/// What the caller of `set_goal` asks for: a new objective for a running
/// conversation, or the end of the one it pursues.
#[derive(Debug, Clone)]
pub struct GoalRequest {
    pub project: Option<ProjectId>,
    pub instance: Option<InstanceId>,
    pub session: String,
    /// `set` or `clear`.
    pub action: String,
    pub objective: Option<String>,
}

pub struct Orchestrator {
    /// Where the agents started here reach the board.
    mcp_url: String,
    mcp_urls: HashMap<String, String>,
    /// Tokens of the agents started here, by token.
    tokens: Mutex<HashMap<String, AgentScope>>,
    /// What an already started call answered, by caller and request id.
    started: Mutex<HashMap<String, Value>>,
}

impl Orchestrator {
    pub fn new(mcp_url: String, mcp_urls: HashMap<String, String>) -> Self {
        Self {
            mcp_url,
            mcp_urls,
            tokens: Mutex::new(HashMap::new()),
            started: Mutex::new(HashMap::new()),
        }
    }

    /// The scope of an agent's token, while that agent is one of ours.
    pub fn scope(&self, token: &str) -> Option<AgentScope> {
        self.tokens.lock().unwrap().get(token).cloned()
    }

    /// Starts an agent on a card and sends it the prompt. The answer is
    /// given once the session exists and the prompt is on its way; the turn
    /// itself is followed in the common list.
    pub async fn start(
        &self,
        projects: &Projects,
        catalog: &Catalog,
        supervisor: &Supervisor,
        worktrees: &Worktrees,
        caller: &str,
        request: StartRequest,
    ) -> Result<Value, String> {
        let repeat = request
            .request_id
            .as_ref()
            .map(|id| format!("{caller}:{id}"));
        if let Some(previous) = repeat
            .as_ref()
            .and_then(|key| self.started.lock().unwrap().get(key).cloned())
        {
            return Ok(previous);
        }

        let project = projects.get(request.project).ok_or("no such project")?;
        if project.state != ProjectState::Ready {
            return Err("the project is not ready yet".to_owned());
        }
        let task = match request.task_id {
            Some(id) => Some(
                projects
                    .tasks(project.id)
                    .map_err(|error| format!("{error:#}"))?
                    .into_iter()
                    .find(|task| task.id == id)
                    .ok_or("no such task in the project")?,
            ),
            None => None,
        };
        let agent = catalog
            .get(&request.agent_id)
            .ok_or_else(|| format!("unknown agent: {}", request.agent_id))?;
        let dir = projects.dir(&project);
        let worktree = match (request.worktree_id, request.worktree_path.as_deref()) {
            (Some(_), Some(_)) => {
                return Err("give either worktreeId or worktreePath, not both".to_owned());
            }
            (Some(id), None) => match worktrees.get(&project, &dir, id).await {
                Ok(worktree) => worktree,
                Err(WorktreeError::NotFound) => {
                    let known = worktrees
                        .list_for(&project, &dir)
                        .await
                        .iter()
                        .map(|worktree| format!("{} ({})", worktree.id, worktree.name))
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(format!(
                        "no such worktree in the project; its checkouts are: {known}"
                    ));
                }
                Err(error) => return Err(format!("{error}")),
            },
            (None, Some(path)) => worktrees
                .adopt(&project, &dir, path)
                .await
                .map_err(|error| match error {
                    WorktreeError::NotFound => "no such worktree in the project".to_owned(),
                    error => format!("{error}"),
                })?,
            (None, None) => worktrees
                .get(&project, &dir, project.id)
                .await
                .map_err(|error| format!("{error}"))?,
        };

        let instance = supervisor
            .start(
                project.id,
                worktree.id,
                worktree.name.clone(),
                std::path::PathBuf::from(&worktree.path),
                agent.clone(),
            )
            .map_err(|error| format!("could not start the agent: {error:#}"))?;
        let init = instance.proxy.wait_initialized(READY_TIMEOUT).await?;

        // A token of its own for the board, so the child writes to the card
        // and cannot use anything else the person's token opens.
        let token = new_token();
        let http = init
            .pointer("/agentCapabilities/mcpCapabilities/http")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let (mcp_servers, scoped) = if http {
            let scope = AgentScope {
                token: token.clone(),
                project: project.id,
                task: request.task_id,
                instance: instance.id,
                session: None,
            };
            self.tokens.lock().unwrap().insert(token.clone(), scope);
            (
                json!([{
                    "type": "http",
                    "name": "aiddiction",
                    "url": self.mcp_urls.get(&agent.id).unwrap_or(&self.mcp_url),
                    "headers": [{ "name": "Authorization", "value": format!("Bearer {token}") }],
                }]),
                true,
            )
        } else {
            (json!([]), false)
        };

        let session = match instance.proxy.open_session(mcp_servers).await {
            Ok(session) => session,
            Err(error) => {
                self.tokens.lock().unwrap().remove(&token);
                return Err(format!("could not open a conversation: {error}"));
            }
        };
        if let Some(scope) = self.tokens.lock().unwrap().get_mut(&token) {
            scope.session = Some(session.clone());
        }

        // Options go in the order they were given: a model may change what
        // effort is offered. A setting the agent refuses means no prompt.
        for (id, value) in &request.settings {
            if let Err(error) = instance.proxy.set_config_option(&session, id, value).await {
                instance.proxy.close_session(&session).await;
                self.tokens.lock().unwrap().remove(&token);
                return Err(format!("setting {id} to {value}: {error}"));
            }
        }

        // What the child is given: a goal, when asked for, is the turn itself;
        // otherwise the prompt is.
        let base = match request
            .goal
            .as_deref()
            .map(str::trim)
            .filter(|goal| !goal.is_empty())
        {
            Some(goal) if request.prompt.trim().is_empty() => goal.to_owned(),
            Some(goal) => format!("{goal}\n\n{}", request.prompt.trim()),
            None => request.prompt.trim().to_owned(),
        };
        let text = prompt_text(&base, &project, task.as_ref(), scoped);
        match request
            .goal
            .as_deref()
            .map(str::trim)
            .filter(|goal| !goal.is_empty())
        {
            Some(_) => {
                instance.proxy.begin(&session, &text, request.task_id);
                if let Err(error) = instance
                    .proxy
                    .control_goal(&session, "set", Some(&text), GOAL_ANSWER_TIMEOUT)
                    .await
                {
                    instance.proxy.close_session(&session).await;
                    self.tokens.lock().unwrap().remove(&token);
                    return Err(format!("the agent did not take a goal: {error}"));
                }
            }
            None => {
                instance
                    .proxy
                    .prompt(&session, &text, request.task_id)
                    .await;
            }
        }

        if let Some(task) = &task
            && let Err(error) = projects.link_session(
                project.id,
                task.id,
                TaskSession {
                    instance_id: instance.id,
                    session_id: session.clone(),
                    agent_id: agent.id.clone(),
                    started_at_ms: now_ms(),
                },
            )
        {
            tracing::warn!("could not link {} to {}: {error:#}", task.id, session);
        }

        let answer = json!({
            "projectId": project.id,
            "taskId": request.task_id,
            "agentId": agent.id,
            "instanceId": instance.id,
            "sessionId": session,
            "worktreeId": worktree.id,
            "board": scoped,
        });
        if let Some(key) = repeat {
            self.remember_answer(key, &answer);
        }
        Ok(answer)
    }

    /// Sends another message to a conversation that is already running, so
    /// the agent keeps its context instead of a new one being started.
    pub async fn send(
        &self,
        projects: &Projects,
        supervisor: &Supervisor,
        caller: &str,
        request: SendRequest,
    ) -> Result<Value, String> {
        let repeat = request
            .request_id
            .as_ref()
            .map(|id| format!("send:{caller}:{id}"));
        if let Some(previous) = repeat
            .as_ref()
            .and_then(|key| self.started.lock().unwrap().get(key).cloned())
        {
            return Ok(previous);
        }
        if request.prompt.trim().is_empty() {
            return Err("prompt is required".to_owned());
        }

        let (instance, session) = find_session(
            supervisor,
            request.project,
            request.instance,
            &request.session,
        )?;
        if session.busy {
            let doing = if session.preview.is_empty() {
                "a turn is running".to_owned()
            } else {
                format!("it is doing: {}", session.preview)
            };
            return Err(format!(
                "the conversation is still working ({doing}); send the message when its turn ends"
            ));
        }

        let project = projects.get(instance.project_id).ok_or("no such project")?;
        let tasks = projects
            .tasks(project.id)
            .map_err(|error| format!("{error:#}"))?;
        let asked = match request.task_id {
            Some(id) => Some(
                tasks
                    .iter()
                    .find(|task| task.id == id)
                    .cloned()
                    .ok_or("no such task in the project")?,
            ),
            None => None,
        };
        // The card the conversation works on: the one named now, or the one
        // it was started on.
        let task = asked.or_else(|| {
            session
                .task_id
                .and_then(|id| tasks.iter().find(|task| task.id == id).cloned())
        });
        let board = instance.proxy.has_board(&request.session);
        let text = prompt_text(&request.prompt, &project, task.as_ref(), board);

        if let Some(task) = &task
            && let Err(error) = projects.link_session(
                project.id,
                task.id,
                TaskSession {
                    instance_id: instance.id,
                    session_id: request.session.clone(),
                    agent_id: instance.agent.id.clone(),
                    started_at_ms: now_ms(),
                },
            )
        {
            tracing::warn!(
                "could not link {} to {}: {error:#}",
                task.id,
                request.session
            );
        }

        instance
            .proxy
            .prompt(&request.session, &text, task.as_ref().map(|task| task.id))
            .await;

        let answer = json!({
            "projectId": project.id,
            "taskId": task.as_ref().map(|task| task.id),
            "agentId": instance.agent.id,
            "instanceId": instance.id,
            "sessionId": request.session,
            "worktreeId": instance.worktree_id,
            "sent": true,
            // How full the context was before this message; the turn grows it.
            "usage": session.usage,
        });
        if let Some(key) = repeat {
            self.remember_answer(key, &answer);
        }
        Ok(answer)
    }

    /// Sets or clears the goal of a conversation that is already running.
    /// The agent may take it into the turn it is in or start a turn of its
    /// own for it, so the answer comes once the goal is sent, not when the
    /// turn ends.
    pub async fn goal(
        &self,
        supervisor: &Supervisor,
        request: GoalRequest,
    ) -> Result<Value, String> {
        let (instance, session) = find_session(
            supervisor,
            request.project,
            request.instance,
            &request.session,
        )?;
        instance
            .proxy
            .control_goal(
                &request.session,
                &request.action,
                request.objective.as_deref(),
                GOAL_ANSWER_TIMEOUT,
            )
            .await
            .map_err(|error| format!("the agent did not take the goal: {error}"))?;
        let goal = instance
            .info()
            .sessions
            .into_iter()
            .find(|session| session.session_id == request.session)
            .and_then(|session| session.goal);
        Ok(json!({
            "projectId": instance.project_id,
            "taskId": session.task_id,
            "agentId": instance.agent.id,
            "instanceId": instance.id,
            "sessionId": request.session,
            "worktreeId": instance.worktree_id,
            "goal": goal,
        }))
    }

    fn remember_answer(&self, key: String, answer: &Value) {
        let mut started = self.started.lock().unwrap();
        // A lifetime's worth of calls is not worth keeping; the newest
        // thousand answers are plenty to retry against.
        if started.len() > 1000 {
            started.clear();
        }
        started.insert(key, answer.clone());
    }
}

/// The running instance and the summary of a conversation, named by its
/// session id, optionally narrowed to a project or an instance.
fn find_session(
    supervisor: &Supervisor,
    project: Option<ProjectId>,
    wanted_instance: Option<InstanceId>,
    session: &str,
) -> Result<(Arc<Instance>, SessionSummary), String> {
    let found: Vec<_> = supervisor
        .all()
        .into_iter()
        .filter(|instance| {
            project.is_none_or(|project| instance.project_id == project)
                && wanted_instance.is_none_or(|wanted| instance.id == wanted)
        })
        .filter(|instance| {
            instance
                .info()
                .sessions
                .iter()
                .any(|shown| shown.session_id == session)
        })
        .collect();
    let instance = match found.as_slice() {
        [] => {
            return Err(format!(
                "no conversation {session} is running: its agent has gone, list_subagents shows the live ones, \
                 and start_agent starts a new one"
            ));
        }
        [only] => only.clone(),
        many => {
            let ids = many
                .iter()
                .map(|instance| format!("{} ({})", instance.id, instance.agent.id))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "the conversation is in more than one agent: {ids}; name its instanceId"
            ));
        }
    };
    let info = instance.info();
    if info.state != InstanceState::Ready {
        return Err(format!(
            "the agent of this conversation is {:?}, not running; start_agent starts a new one",
            info.state
        ));
    }
    let summary = info
        .sessions
        .into_iter()
        .find(|shown| shown.session_id == session)
        .ok_or("no such conversation")?;
    Ok((instance, summary))
}

/// The text the child is given: the caller's words, then where the card is
/// and what to do with it.
fn prompt_text(text: &str, project: &Project, task: Option<&Task>, board: bool) -> String {
    let mut text = text.trim().to_owned();
    let Some(task) = task else { return text };
    text.push_str(&format!(
        "\n\n---\nYou are working on task card {} (\"{}\") of project \"{}\" in Aiddiction.",
        task.id, task.title, project.name
    ));
    if board {
        text.push_str(
            " The board is the aiddiction MCP server: list_tasks shows the card, update_task writes to it. \
             Keep what the card already says, add what you did and checked and what is left, and say what the card \
             does not: paths and lines. Move it to inProgress while you work, and to done only when the work is \
             finished and checked.",
        );
    } else {
        text.push_str(
            " This agent was not given the board's tools; answer in the conversation instead.",
        );
    }
    text
}

fn new_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
