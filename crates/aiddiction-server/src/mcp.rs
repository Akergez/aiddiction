//! The MCP server agents reach at `POST /api/v1/mcp` (Streamable HTTP, JSON
//! responses only, no stream): the projects and their task boards, the
//! same store the UI shows, so an agent can list, create, edit, move and
//! delete cards.
//!
//! The person's token also starts agents (`start_agent`) and follows them
//! (`list_subagents`). An agent started that way gets a token of its own,
//! which reaches this server only: its project's board and nothing that
//! starts further agents.
//!
//! [`handle`] answers one JSON-RPC message; `None` is a notification, which
//! the route answers with 202 and no body. A tool that fails says so in its
//! result (`isError`), as MCP wants, rather than as a protocol error.

use aiddiction_api::{
    Agent, AgentProfile, InstanceId, InstanceState, NewWorktree, ProjectId, TaskId, TaskInput,
    TaskStatus, WorktreeId,
};
use serde_json::{Value, json};

use crate::agents::Catalog;
use crate::instances::Supervisor;
use crate::orchestrate::{Caller, Orchestrator, SendRequest, StartRequest};
use crate::projects::{ProjectError, Projects};
use crate::worktrees::Worktrees;

/// The protocol versions this server speaks, newest first.
const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

#[allow(clippy::too_many_arguments)]
pub async fn handle(
    projects: &Projects,
    catalog: &Catalog,
    supervisor: &Supervisor,
    orchestration: &Orchestrator,
    worktrees: &Worktrees,
    caller: &Caller,
    message: &Value,
) -> Option<Value> {
    // A notification has no id and gets no answer.
    let id = message.get("id")?.clone();
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
    let outcome = match method {
        "initialize" => Ok(initialize(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools(is_agent(caller)) })),
        "tools/call" => {
            call(
                projects,
                catalog,
                supervisor,
                orchestration,
                worktrees,
                caller,
                &params,
            )
            .await
        }
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    Some(match outcome {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        }
    })
}

fn is_agent(caller: &Caller) -> bool {
    matches!(caller, Caller::Agent(_))
}

fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = asked
        .filter(|asked| VERSIONS.contains(asked))
        .unwrap_or(VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "aiddiction", "version": env!("CARGO_PKG_VERSION") },
        "instructions": "Aiddiction's projects and their kanban boards of task cards. \
            list_projects gives the project ids the other tools take; a project's folder is its path. \
            start_agent gives an agent a card to work on, send_message continues a conversation it started, \
            and list_subagents follows them and tells how full a conversation's context is.",
    })
}

fn tools(for_agent: bool) -> Value {
    let status = json!({ "type": "string", "enum": ["todo", "inProgress", "done"], "description": "The board's column" });
    let project = json!({ "type": "string", "description": "A project id from list_projects" });
    let task = json!({ "type": "string", "description": "A task id from list_tasks" });
    let mut tools = vec![
        json!({
            "name": "list_projects",
            "description": "List the server's projects: id, name and folder.",
            "inputSchema": { "type": "object", "properties": {} },
        }),
        json!({
            "name": "list_tasks",
            "description": "List a project's task cards.",
            "inputSchema": { "type": "object", "required": ["projectId"], "properties": { "projectId": project } },
        }),
        json!({"name":"share_file","description":"Publish a file from the project and return a tokenized download URL.","inputSchema":{"type":"object","required":["projectId","path"],"properties":{"projectId":project,"path":{"type":"string","description":"Path to a file in the project"},"name":{"type":"string"}}}}),
        json!({
            "name": "create_task",
            "description": "Create a task card; it goes to Todo unless a status is given.",
            "inputSchema": {
                "type": "object",
                "required": ["projectId", "title"],
                "properties": {
                    "projectId": project,
                    "title": { "type": "string" },
                    "markdown": { "type": "string", "description": "The card's details, in Markdown" },
                    "status": status,
                },
            },
        }),
        json!({
            "name": "update_task",
            "description": "Edit a task card or move it to another column; what is left out stays as it is.",
            "inputSchema": {
                "type": "object",
                "required": ["projectId", "taskId"],
                "properties": {
                    "projectId": project,
                    "taskId": task,
                    "title": { "type": "string" },
                    "markdown": { "type": "string" },
                    "status": status,
                },
            },
        }),
        json!({
            "name": "delete_task",
            "description": "Delete a task card.",
            "inputSchema": { "type": "object", "required": ["projectId", "taskId"], "properties": { "projectId": project, "taskId": task } },
        }),
    ];
    if !for_agent {
        tools.insert(1, json!({
            "name": "list_agents",
            "description": "List the ACP agents that can be started, with what each said about itself when it last \
                initialized, and — for a project and its checkout — the model and settings it last offered there.",
            "inputSchema": { "type": "object", "properties": {
                "projectId": { "type": "string", "description": "A project id, for its last settings" },
                "worktreeId": { "type": "string", "description": "A checkout id from the project's worktrees; the main one when left out" },
            }},
        }));
        tools.insert(2, json!({
            "name": "list_subagents",
            "description": "See the agents running for a project (or one of its checkouts) and their conversations, \
                including the threads started by start_agent (a thread's taskId says which card it works on). \
                A thread says whether it is busy (`busy`), what it is doing (`preview`), why its last turn \
                failed (`error`), and how full its context window is (`usage`: `used` and `size` in tokens, \
                `cost` when the agent prices it).",
            "inputSchema": { "type": "object", "properties": {
                "projectId": { "type": "string" },
                "worktreeId": { "type": "string", "description": "Only agents working in this checkout" },
                "taskId": { "type": "string", "description": "Only threads of this card" },
            }},
        }));
        tools.insert(3, json!({
            "name": "start_agent",            "description": "Start an ACP agent on a task card. The agent gets a token for the project's board, is \
                prompted, and works in its own conversation, visible to the person in the common list. Answers \
                once the prompt is sent, not when the turn ends: follow it with list_subagents, read the result on \
                the card. It works in the project's own checkout unless a worktree is given: one the server made \
                (worktreeId) or one you made yourself with git worktree add (worktreePath), which is then kept \
                under an id of its own.",
            "inputSchema": {
                "type": "object",
                "required": ["projectId", "agentId"],
                "properties": {
                    "projectId": { "type": "string" },
                    "agentId": { "type": "string", "description": "An id from list_agents" },
                    "taskId": { "type": "string", "description": "The card to work on; it is linked to the thread" },
                    "worktreeId": { "type": "string", "description": "A checkout the server made, by id; the project's main one when neither is given" },
                    "worktreePath": { "type": "string", "description": "A git worktree of this project you made yourself, by folder (absolute, relative to the project, or ~/…); it is adopted and the answer gives its id. Its .git points into the project's own checkout, which the agent must reach at the same path" },
                    "prompt": { "type": "string", "description": "What to do, on top of the card; not needed when a goal is given" },
                    "goal": { "type": "string", "description": "An objective to pursue instead of a single prompt (Claude Code and Codex ACP have it); the agent works on it as a turn of its own, and the prompt, when given, opens it" },
                    "settings": {
                        "type": "array",
                        "description": "ACP configOptions, applied in this order (a model may change the effort offered)",
                        "items": {
                            "type": "object",
                            "required": ["configId", "value"],
                            "properties": { "configId": { "type": "string" }, "value": {} },
                        },
                    },
                    "requestId": { "type": "string", "description": "Repeat this call with the same id and it is the same start" },
                },
            },
        }));
        tools.insert(4, json!({
            "name": "send_message",
            "description": "Send another message to a conversation that is already running, so the agent keeps its \
                context instead of a new one being started: name the sessionId start_agent gave, or one from \
                list_subagents. The turn grows the context: list_subagents says how full it is. Refused while the \
                conversation is still working. Answers once the message is sent, not when the turn ends; the result \
                comes to the card and the conversation. The answer's `usage` is how full the context was before \
                this message.",
            "inputSchema": {
                "type": "object",
                "required": ["sessionId", "prompt"],
                "properties": {
                    "sessionId": { "type": "string", "description": "A conversation from start_agent or list_subagents" },
                    "prompt": { "type": "string", "description": "What to do next; the card's context is added when it has one" },
                    "projectId": { "type": "string", "description": "A project id, to look the conversation up in it" },
                    "instanceId": { "type": "string", "description": "The agent instance, when the session id is not enough" },
                    "taskId": { "type": "string", "description": "The card to work on now; the conversation's own card when left out" },
                    "requestId": { "type": "string", "description": "Repeat this call with the same id and it is the same message" },
                },
            },
        }));
        tools.push(json!({
            "name": "list_worktrees",
            "description": "The checkouts of a project: its own and the ones the server made or adopted, with the \
                branch each has. An agent can be started in any of them by worktreeId.",
            "inputSchema": { "type": "object", "required": ["projectId"], "properties": {
                "projectId": { "type": "string" },
            }},
        }));
        tools.push(json!({
            "name": "create_worktree",
            "description": "Make a clone of the project on a new branch, outside its own checkout, and answer \
                with its id: agents started there work in their own files and branch. Its .git is its own, so it \
                works where the project's checkout cannot be reached; its commits come back by sync_worktree. The \
                branch is kept when the clone goes.",
            "inputSchema": { "type": "object", "required": ["projectId"], "properties": {
                "projectId": { "type": "string" },
                "name": { "type": "string", "description": "Its name in a list; also the folder when there is no branch" },
                "branch": { "type": "string", "description": "The branch to make; aiddiction/<name> when left out" },
                "base": { "type": "string", "description": "The commit or branch to start from; the project's HEAD when left out" },
            }},
        }));
        tools.push(json!({
            "name": "sync_worktree",
            "description": "Bring the commits of a clone checkout into the project's repository, branch and all, so it \
                can be merged in the main checkout like any other branch. A worktree shares the repository already \
                and needs nothing.",
            "inputSchema": { "type": "object", "required": ["projectId", "worktreeId"], "properties": {
                "projectId": { "type": "string" },
                "worktreeId": { "type": "string", "description": "An id from list_worktrees" },
            }},
        }));
        tools.push(json!({
            "name": "remove_worktree",
            "description": "Remove a worktree the server made, or forget an adopted one. Refused while an agent is \
                working in it; the branch stays in the repository.",
            "inputSchema": { "type": "object", "required": ["projectId", "worktreeId"], "properties": {
                "projectId": { "type": "string" },
                "worktreeId": { "type": "string", "description": "An id from list_worktrees" },
            }},
        }));
    }
    json!(tools)
}

/// A tool's result: its output as text, or what went wrong, flagged.
async fn call(
    projects: &Projects,
    catalog: &Catalog,
    supervisor: &Supervisor,
    orchestration: &Orchestrator,
    worktrees: &Worktrees,
    caller: &Caller,
    params: &Value,
) -> Result<Value, (i64, String)> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if is_agent(caller)
        && matches!(
            name,
            "list_agents"
                | "list_subagents"
                | "start_agent"
                | "send_message"
                | "list_worktrees"
                | "create_worktree"
                | "remove_worktree"
        )
    {
        return Err((-32602, format!("{name} is not open to an agent")));
    }
    let output = match name {
        "list_projects" => Ok(json!(
            projects
                .list()
                .into_iter()
                .filter(|project| match caller {
                    Caller::User => true,
                    Caller::Agent(scope) => project.id == scope.project,
                })
                .map(|project| json!({ "id": project.id, "name": project.name, "path": projects.dir(&project), "state": project.state }))
                .collect::<Vec<_>>()
        )),
        "list_agents" => Ok(json!(
            catalog
                .public_list()
                .into_iter()
                .map(|agent| {
                    let profile = supervisor.profile(&agent.id);
                    let project = arguments.get("projectId").and_then(Value::as_str).and_then(|project| project.parse::<ProjectId>().ok());
                    // The main checkout when no other is named.
                    let worktree = match arguments.get("worktreeId").and_then(Value::as_str) {
                        Some(worktree) => worktree.parse::<WorktreeId>().ok(),
                        None => project,
                    };
                    let choices = project.and_then(|project| {
                        supervisor
                            .for_project(project)
                            .into_iter()
                            .filter(|instance| instance.agent.id == agent.id)
                            .filter(|instance| worktree.is_none_or(|worktree| instance.worktree_id == worktree))
                            .filter_map(|instance| instance.proxy.latest_choices())
                            .max_by_key(|(_, _, at)| *at)
                    });
                    agent_summary(agent, profile, choices)
                })
                .collect::<Vec<_>>()
        )),
        "list_subagents" => {
            let project = match arguments.get("projectId").and_then(Value::as_str) {
                Some(project) => Some(project.parse::<ProjectId>().map_err(|_| (-32602, "projectId is not an id".to_owned()))?),
                None => None,
            };
            let worktree = match arguments.get("worktreeId").and_then(Value::as_str) {
                Some(worktree) => Some(worktree.parse::<WorktreeId>().map_err(|_| (-32602, "worktreeId is not an id".to_owned()))?),
                None => None,
            };
            let task = match arguments.get("taskId").and_then(Value::as_str) {
                Some(task) => Some(task.parse::<TaskId>().map_err(|_| (-32602, "taskId is not an id".to_owned()))?),
                None => None,
            };
            let instances = project.map_or_else(|| supervisor.all(), |project| supervisor.for_project(project));
            let infos = instances
                .into_iter()
                .filter(|instance| worktree.is_none_or(|worktree| instance.worktree_id == worktree))
                .map(|instance| {
                    let mut info = instance.info();
                    if let Some(task) = task {
                        info.sessions.retain(|session| session.task_id == Some(task));
                    }
                    info
                })
                .filter(|info| task.is_none() || !info.sessions.is_empty())
                .collect::<Vec<_>>();
            Ok(json!(infos))
        }
        "start_agent" => start(projects, catalog, supervisor, orchestration, worktrees, caller, &arguments).await,
        "send_message" => send(projects, supervisor, orchestration, caller, &arguments).await,
        "list_worktrees" => list_worktrees(projects, worktrees, caller, &arguments).await,
        "create_worktree" => create_worktree(projects, worktrees, caller, &arguments).await,
        "remove_worktree" => remove_worktree(projects, supervisor, worktrees, caller, &arguments).await,
        "sync_worktree" => sync_worktree(projects, worktrees, caller, &arguments).await,
        "list_tasks" => project_of(&arguments, caller).and_then(|project| Ok(json!(projects.tasks(project)?))),
        "share_file" => (|| { let project=project_of(&arguments, caller)?; let path=text(&arguments,"path").ok_or_else(||ToolError("path is required".into()))?; let name=text(&arguments,"name"); let p=projects.list().into_iter().find(|x|x.id==project).ok_or_else(||ToolError("project not found".into()))?; let file=crate::files::share(&projects.dir(&p), &path, name.as_deref(), "").map_err(ToolError)?; Ok(json!(file)) })(),
        "create_task" => (|| {
            let project = project_of(&arguments, caller)?;
            let input = TaskInput {
                title: text(&arguments, "title").unwrap_or_default(),
                markdown: text(&arguments, "markdown").unwrap_or_default(),
                status: status(&arguments)?,
            };
            Ok(json!(projects.create_task(project, input)?))
        })(),
        "update_task" => (|| {
            let (project, task): (ProjectId, TaskId) = (project_of(&arguments, caller)?, id(&arguments, "taskId")?);
            let known = projects.tasks(project)?.into_iter().find(|known| known.id == task).ok_or("no such task")?;
            let input = TaskInput {
                title: text(&arguments, "title").unwrap_or(known.title),
                markdown: text(&arguments, "markdown").unwrap_or(known.markdown),
                status: status(&arguments)?,
            };
            Ok(json!(projects.update_task(project, task, input)?))
        })(),
        "delete_task" => (|| {
            let project = project_of(&arguments, caller)?;
            let task = id(&arguments, "taskId")?;
            if let Caller::Agent(scope) = caller
                && scope.task == Some(task)
            {
                return Err(ToolError("the card this agent was started on is not its to delete".into()));
            }
            projects.delete_task(project, task)?;
            Ok(json!({ "deleted": true }))
        })(),
        _ => return Err((-32602, format!("unknown tool: {name}"))),
    };
    Ok(match output {
        Ok(value) => json!({ "content": [{ "type": "text", "text": value.to_string() }] }),
        Err(ToolError(message)) => {
            json!({ "content": [{ "type": "text", "text": message }], "isError": true })
        }
    })
}

/// `start_agent`: a card, an agent and a prompt.
async fn start(
    projects: &Projects,
    catalog: &Catalog,
    supervisor: &Supervisor,
    orchestration: &Orchestrator,
    worktrees: &Worktrees,
    caller: &Caller,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let project = project_of(arguments, caller)?;
    let agent_id =
        text(arguments, "agentId").ok_or_else(|| ToolError("agentId is required".into()))?;
    let prompt = text(arguments, "prompt").unwrap_or_default();
    let goal = text(arguments, "goal").filter(|goal| !goal.trim().is_empty());
    if prompt.trim().is_empty() && goal.is_none() {
        return Err(ToolError("prompt or goal is required".into()));
    }
    let task_id = match arguments.get("taskId").and_then(Value::as_str) {
        Some(task) => Some(
            task.parse()
                .map_err(|_| ToolError("taskId is not an id".into()))?,
        ),
        None => None,
    };
    let worktree_id = match arguments.get("worktreeId").and_then(Value::as_str) {
        Some(worktree) => Some(
            worktree
                .parse()
                .map_err(|_| ToolError("worktreeId is not an id".into()))?,
        ),
        None => None,
    };
    let worktree_path = text(arguments, "worktreePath").filter(|path| !path.trim().is_empty());
    if worktree_id.is_some() && worktree_path.is_some() {
        return Err(ToolError(
            "give either worktreeId or worktreePath, not both".into(),
        ));
    }
    let request = StartRequest {
        project,
        agent_id,
        task_id,
        worktree_id,
        worktree_path,
        prompt,
        goal,
        settings: settings(arguments)?,
        request_id: text(arguments, "requestId"),
    };
    let caller_key = match caller {
        Caller::User => "user".to_owned(),
        Caller::Agent(scope) => scope.token.clone(),
    };
    orchestration
        .start(
            projects,
            catalog,
            supervisor,
            worktrees,
            &caller_key,
            request,
        )
        .await
        .map_err(ToolError)
}

/// `send_message`: another turn of a conversation already started, so the
/// agent keeps its context instead of a new one being spawned.
async fn send(
    projects: &Projects,
    supervisor: &Supervisor,
    orchestration: &Orchestrator,
    caller: &Caller,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let session =
        text(arguments, "sessionId").ok_or_else(|| ToolError("sessionId is required".into()))?;
    let prompt = text(arguments, "prompt").ok_or_else(|| ToolError("prompt is required".into()))?;
    let project = match arguments.get("projectId").and_then(Value::as_str) {
        Some(project) => Some(
            project
                .parse::<ProjectId>()
                .map_err(|_| ToolError("projectId is not an id".into()))?,
        ),
        None => None,
    };
    let instance = match arguments.get("instanceId").and_then(Value::as_str) {
        Some(instance) => Some(
            instance
                .parse::<InstanceId>()
                .map_err(|_| ToolError("instanceId is not an id".into()))?,
        ),
        None => None,
    };
    let task_id = match arguments.get("taskId").and_then(Value::as_str) {
        Some(task) => Some(
            task.parse::<TaskId>()
                .map_err(|_| ToolError("taskId is not an id".into()))?,
        ),
        None => None,
    };
    let request = SendRequest {
        project,
        instance,
        session,
        prompt,
        task_id,
        request_id: text(arguments, "requestId"),
    };
    let caller_key = match caller {
        Caller::User => "user".to_owned(),
        Caller::Agent(scope) => scope.token.clone(),
    };
    orchestration
        .send(projects, supervisor, &caller_key, request)
        .await
        .map_err(ToolError)
}

/// The project the caller names, with the folder it works in.
fn seen_project(
    projects: &Projects,
    caller: &Caller,
    arguments: &Value,
) -> Result<(aiddiction_api::Project, std::path::PathBuf), ToolError> {
    let id = project_of(arguments, caller)?;
    let project = projects
        .list()
        .into_iter()
        .find(|project| project.id == id)
        .ok_or_else(|| ToolError("no such project".into()))?;
    let dir = projects.dir(&project);
    Ok((project, dir))
}

/// `list_worktrees`: the project's own checkout and the ones made or adopted.
async fn list_worktrees(
    projects: &Projects,
    worktrees: &Worktrees,
    caller: &Caller,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let (project, dir) = seen_project(projects, caller, arguments)?;
    let checkouts = worktrees.list_for(&project, &dir).await;
    Ok(json!(
        checkouts
            .iter()
            .map(|worktree| json!({
                "worktreeId": worktree.id,
                "name": worktree.name,
                "branch": worktree.branch,
                "baseCommit": worktree.base_commit,
                "path": worktree.path,
                "main": worktree.main,
                "managed": worktree.managed,
                "clone": Worktrees::is_clone(worktree),
            }))
            .collect::<Vec<_>>()
    ))
}

/// `create_worktree`: a checkout of the project on a branch of its own.
async fn create_worktree(
    projects: &Projects,
    worktrees: &Worktrees,
    caller: &Caller,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let (project, dir) = seen_project(projects, caller, arguments)?;
    let input = NewWorktree {
        name: text(arguments, "name"),
        branch: text(arguments, "branch"),
        base: text(arguments, "base"),
    };
    let clone = match text(arguments, "mode").as_deref() {
        None | Some("clone") => true,
        Some("worktree") => false,
        Some(other) => {
            return Err(ToolError(format!("mode is clone or worktree, not {other}")));
        }
    };
    let made = match clone {
        true => worktrees.create_clone(&project, &dir, input).await,
        false => worktrees.create(&project, &dir, input).await,
    }
    .map_err(|error| ToolError(format!("{error}")))?;
    Ok(json!({
        "worktreeId": made.id,
        "name": made.name,
        "branch": made.branch,
        "baseCommit": made.base_commit,
        "path": made.path,
        "clone": Worktrees::is_clone(&made),
    }))
}

/// `remove_worktree`: takes a worktree the server made away, or forgets an
/// adopted one; a running agent stops it.
async fn remove_worktree(
    projects: &Projects,
    supervisor: &Supervisor,
    worktrees: &Worktrees,
    caller: &Caller,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let (project, dir) = seen_project(projects, caller, arguments)?;
    let worktree: WorktreeId = text(arguments, "worktreeId")
        .ok_or_else(|| ToolError("worktreeId is required".into()))?
        .parse()
        .map_err(|_| ToolError("worktreeId is not an id".into()))?;
    let busy = supervisor
        .for_worktree(project.id, worktree)
        .iter()
        .any(|instance| {
            matches!(
                instance.state(),
                InstanceState::Starting | InstanceState::Ready
            )
        });
    if busy {
        return Err(ToolError("the agent is still working in it".into()));
    }
    worktrees
        .remove(&project, &dir, worktree)
        .await
        .map_err(|error| ToolError(format!("{error}")))?;
    Ok(json!({ "worktreeId": worktree, "removed": true }))
}

/// `sync_worktree`: a clone's branch into the project's repository.
async fn sync_worktree(
    projects: &Projects,
    worktrees: &Worktrees,
    caller: &Caller,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let (project, dir) = seen_project(projects, caller, arguments)?;
    let worktree: WorktreeId = text(arguments, "worktreeId")
        .ok_or_else(|| ToolError("worktreeId is required".into()))?
        .parse()
        .map_err(|_| ToolError("worktreeId is not an id".into()))?;
    let (branch, commit) = worktrees
        .sync(&project, &dir, worktree)
        .await
        .map_err(|error| ToolError(format!("{error}")))?;
    Ok(json!({ "worktreeId": worktree, "branch": branch, "commit": commit }))
}

fn agent_summary(
    agent: Agent,
    profile: Option<AgentProfile>,
    choices: Option<(Value, Value, u64)>,
) -> Value {
    let mut summary = json!({ "id": agent.id, "name": agent.name, "source": agent.source, "command": agent.command.command });
    if let Some(profile) = profile {
        summary["profile"] = json!({
            "agentInfo": profile.agent_info,
            "capabilities": profile.capabilities,
            "authMethods": profile.auth_methods,
            "learnedAtMs": profile.learned_at_ms,
            // The agent now runs something else than when it was learned.
            "stale": !profile.is_for(&agent.command),
        });
    }
    if let Some((modes, config_options, at)) = choices {
        summary["lastChoices"] =
            json!({ "modes": modes, "configOptions": config_options, "learnedAtMs": at });
    }
    summary
}

struct ToolError(String);

impl From<&str> for ToolError {
    fn from(message: &str) -> Self {
        ToolError(message.into())
    }
}

impl From<String> for ToolError {
    fn from(message: String) -> Self {
        ToolError(message)
    }
}

impl From<ProjectError> for ToolError {
    fn from(error: ProjectError) -> Self {
        ToolError(format!("{error:#}"))
    }
}

fn text(arguments: &Value, key: &str) -> Option<String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn id(arguments: &Value, key: &str) -> Result<uuid::Uuid, ToolError> {
    let value = arguments
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError(format!("{key} is required")))?;
    value
        .parse()
        .map_err(|_| ToolError(format!("{key} is not an id: {value}")))
}

/// The project a tool works in: the one asked for, which for an agent's
/// token must be its own.
fn project_of(arguments: &Value, caller: &Caller) -> Result<ProjectId, ToolError> {
    let project = id(arguments, "projectId")?;
    if let Caller::Agent(scope) = caller
        && scope.project != project
    {
        return Err(ToolError(
            "this agent's token is for another project".into(),
        ));
    }
    Ok(project)
}

fn status(arguments: &Value) -> Result<Option<TaskStatus>, ToolError> {
    match arguments.get("status") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|_| {
                ToolError(format!(
                    "status is one of todo, inProgress, done, not {value}"
                ))
            }),
    }
}

/// Configuration options in the order they are applied: a list of
/// `{ configId, value }`, or an object for a single call.
fn settings(arguments: &Value) -> Result<Vec<(String, Value)>, ToolError> {
    match arguments.get("settings") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                let id = item
                    .get("configId")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| ToolError("each setting needs a configId".into()))?;
                Ok((
                    id.to_owned(),
                    item.get("value").cloned().unwrap_or(Value::Null),
                ))
            })
            .collect(),
        Some(Value::Object(map)) => Ok(map
            .iter()
            .map(|(id, value)| (id.clone(), value.clone()))
            .collect()),
        Some(_) => Err(ToolError(
            "settings is a list of {configId, value}, or an object".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aiddiction_api::{NewProject, ProjectSource, TaskStatus};
    use tokio::sync::broadcast;

    use super::*;
    use crate::orchestrate::{AgentScope, Caller, Orchestrator};
    use crate::store::Store;
    use crate::worktrees::Worktrees;

    fn projects() -> (Arc<Projects>, ProjectId, Arc<Store>, [tempfile::TempDir; 2]) {
        let (root, data) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let store = Arc::new(Store::open(data.path()).unwrap());
        let projects = Arc::new(
            Projects::open(
                root.path(),
                data.path(),
                store.clone(),
                broadcast::channel(8).0,
            )
            .unwrap(),
        );
        let project = projects
            .create(NewProject {
                name: "web".into(),
                source: ProjectSource::Empty,
            })
            .unwrap();
        (projects, project.id, store, [root, data])
    }

    async fn request(projects: &Projects, caller: &Caller, method: &str, params: Value) -> Value {
        let catalog = Catalog::new(&[], vec![]);
        let data = tempfile::tempdir().unwrap().keep();
        let store = Arc::new(Store::open(&data).unwrap());
        let supervisor = Supervisor::new(broadcast::channel(1).0, store.clone());
        let orchestration =
            Orchestrator::new("http://127.0.0.1:1/api/v1/mcp".into(), Default::default());
        let worktrees = Worktrees::open(
            &tempfile::tempdir().unwrap().keep(),
            store,
            broadcast::channel(1).0,
        )
        .unwrap();
        handle(
            projects,
            &catalog,
            &supervisor,
            &orchestration,
            &worktrees,
            caller,
            &json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params }),
        )
        .await
        .unwrap()
    }

    /// A tool's output, parsed, or its error text.
    async fn tool(projects: &Projects, name: &str, arguments: Value) -> Result<Value, String> {
        let result = request(
            projects,
            &Caller::User,
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
        .await["result"]
            .clone();
        let text = result["content"][0]["text"].as_str().unwrap().to_owned();
        if result["isError"] == true {
            Err(text)
        } else {
            Ok(serde_json::from_str(&text).unwrap())
        }
    }

    #[tokio::test]
    async fn a_notification_gets_no_answer_and_versions_are_agreed() {
        let (projects, _, _, _dirs) = projects();
        let catalog = Catalog::new(&[], vec![]);
        let data = tempfile::tempdir().unwrap().keep();
        let store = Arc::new(Store::open(&data).unwrap());
        let supervisor = Supervisor::new(broadcast::channel(1).0, store.clone());
        let orchestration =
            Orchestrator::new("http://127.0.0.1:1/api/v1/mcp".into(), Default::default());
        let worktrees = Worktrees::open(
            &tempfile::tempdir().unwrap().keep(),
            store,
            broadcast::channel(1).0,
        )
        .unwrap();
        assert_eq!(
            handle(
                &projects,
                &catalog,
                &supervisor,
                &orchestration,
                &worktrees,
                &Caller::User,
                &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
            )
            .await,
            None
        );
        let version = |asked: &'static str| {
            let projects = &projects;
            async move {
                request(
                    projects,
                    &Caller::User,
                    "initialize",
                    json!({ "protocolVersion": asked }),
                )
                .await["result"]["protocolVersion"]
                    .clone()
            }
        };
        assert_eq!(version("2025-03-26").await, "2025-03-26");
        assert_eq!(version("1999-01-01").await, VERSIONS[0]);
        assert_eq!(
            request(&projects, &Caller::User, "ping", json!({})).await["result"],
            json!({})
        );
        assert_eq!(
            request(&projects, &Caller::User, "resources/list", json!({})).await["error"]["code"],
            -32601
        );
    }

    #[tokio::test]
    async fn an_agent_finds_the_project_and_works_its_board() {
        let (projects, project, _, _dirs) = projects();
        let listed = tool(&projects, "list_projects", json!({})).await.unwrap();
        assert_eq!(listed[0]["id"], json!(project));
        let card = tool(
            &projects,
            "create_task",
            json!({ "projectId": project, "title": "Docs", "markdown": "all of them" }),
        )
        .await
        .unwrap();
        assert_eq!(card["status"], "todo");
        // Moving a card is its status alone: the rest stays.
        let moved = tool(
            &projects,
            "update_task",
            json!({ "projectId": project, "taskId": card["id"], "status": "inProgress" }),
        )
        .await
        .unwrap();
        assert_eq!(
            (
                moved["title"].as_str(),
                moved["markdown"].as_str(),
                moved["status"].as_str()
            ),
            (Some("Docs"), Some("all of them"), Some("inProgress"))
        );
        tool(
            &projects,
            "delete_task",
            json!({ "projectId": project, "taskId": card["id"] }),
        )
        .await
        .unwrap();
        assert_eq!(
            tool(&projects, "list_tasks", json!({ "projectId": project }))
                .await
                .unwrap(),
            json!([])
        );
    }

    #[tokio::test]
    async fn what_goes_wrong_in_a_tool_is_its_result_not_a_protocol_error() {
        let (projects, project, _, _dirs) = projects();
        assert!(
            tool(&projects, "list_tasks", json!({}))
                .await
                .unwrap_err()
                .contains("projectId is required")
        );
        assert!(
            tool(
                &projects,
                "list_tasks",
                json!({ "projectId": uuid::Uuid::nil() })
            )
            .await
            .unwrap_err()
            .contains("no such project")
        );
        assert!(
            tool(
                &projects,
                "create_task",
                json!({ "projectId": project, "title": " " })
            )
            .await
            .unwrap_err()
            .contains("title")
        );
        assert!(
            tool(
                &projects,
                "create_task",
                json!({ "projectId": project, "title": "x", "status": "later" })
            )
            .await
            .unwrap_err()
            .contains("status")
        );
        assert_eq!(
            request(
                &projects,
                &Caller::User,
                "tools/call",
                json!({ "name": "nope" })
            )
            .await["error"]["code"],
            -32602
        );
    }

    #[tokio::test]
    async fn an_agents_token_reaches_its_own_board_and_no_further() {
        let (projects, project, _, _dirs) = projects();
        let other = projects
            .create(NewProject {
                name: "other".into(),
                source: ProjectSource::Empty,
            })
            .unwrap();
        let scope = AgentScope {
            token: "t".into(),
            project,
            task: None,
            instance: uuid::Uuid::now_v7(),
            session: None,
        };
        let agent = Caller::Agent(scope);

        let listed = request(&projects, &agent, "tools/list", json!({})).await;
        let names: Vec<_> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert!(names.contains(&"list_tasks"));
        assert!(
            !names.contains(&"start_agent"),
            "an agent cannot start further agents: {names:?}"
        );
        assert!(!names.contains(&"list_subagents"));
        assert!(!names.contains(&"send_message"));

        let seen = request(
            &projects,
            &agent,
            "tools/call",
            json!({ "name": "list_projects", "arguments": {} }),
        )
        .await;
        let seen: Value =
            serde_json::from_str(seen["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(seen.as_array().unwrap().len(), 1);
        assert_eq!(seen[0]["id"], json!(project));

        let refused = request(
            &projects,
            &agent,
            "tools/call",
            json!({ "name": "list_tasks", "arguments": { "projectId": other.id } }),
        )
        .await;
        assert_eq!(refused["result"]["isError"], true);
        assert!(
            refused["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("another project")
        );

        // The card on its own board is open to it.
        let card = json!({ "projectId": project, "title": "Child", "status": "inProgress" });
        let made = request(
            &projects,
            &agent,
            "tools/call",
            json!({ "name": "create_task", "arguments": card }),
        )
        .await;
        assert!(made["result"]["isError"].is_null(), "{made}");
        assert_eq!(
            task_status(&projects, project).await,
            TaskStatus::InProgress
        );
    }

    async fn task_status(projects: &Projects, project: ProjectId) -> TaskStatus {
        projects.tasks(project).unwrap()[0].status
    }
}
