//! Starting an agent from MCP: the server is the ACP client itself, the
//! session is an ordinary one in the common list of conversations, and the
//! child is given a token of its own for the board, limited to its project,
//! so what it finds and does goes back to the card.
//!
//! The caller gets the ids of the project, the card, the instance and the
//! thread once the prompt is sent, not when the turn ends: it follows the
//! thread with `list_subagents` and reads the result on the card.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use aiddiction_api::{
    InstanceId, Project, ProjectId, ProjectState, Task, TaskId, TaskSession, WorktreeId,
};
use serde_json::{Value, json};

use crate::agents::Catalog;
use crate::instances::Supervisor;
use crate::projects::Projects;
use crate::proxy::now_ms;
use crate::worktrees::{WorktreeError, Worktrees};

/// `npx` may fetch an adapter on first use, as when a client starts one.
const READY_TIMEOUT: Duration = Duration::from_secs(180);

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
    /// ACP configuration option ids and values, applied in this order.
    pub settings: Vec<(String, Value)>,
    /// Ties a retry to the call it repeats, so a lost answer does not start
    /// a second agent.
    pub request_id: Option<String>,
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

        let text = prompt_text(&request, &project, task.as_ref(), scoped);
        instance
            .proxy
            .prompt(&session, &text, request.task_id)
            .await;

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
            let mut started = self.started.lock().unwrap();
            // A lifetime's worth of calls is not worth keeping; the newest
            // thousand answers are plenty to retry against.
            if started.len() > 1000 {
                started.clear();
            }
            started.insert(key, answer.clone());
        }
        Ok(answer)
    }
}

/// The prompt the child is given: the caller's words, then where the card
/// is and what to do with it.
fn prompt_text(
    request: &StartRequest,
    project: &Project,
    task: Option<&Task>,
    board: bool,
) -> String {
    let mut text = request.prompt.trim().to_owned();
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
