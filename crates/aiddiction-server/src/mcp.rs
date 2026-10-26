//! The MCP server agents reach at `POST /api/v1/mcp` (Streamable HTTP, JSON
//! responses only, no stream): the projects and their task boards, the
//! same store the UI shows, so an agent can list, create, edit, move and
//! delete cards.
//!
//! [`handle`] answers one JSON-RPC message; `None` is a notification, which
//! the route answers with 202 and no body. A tool that fails says so in its
//! result (`isError`), as MCP wants, rather than as a protocol error.

use aiddiction_api::{ProjectId, TaskId, TaskInput, TaskStatus};
use serde_json::{Value, json};

use crate::projects::{ProjectError, Projects};

/// The protocol versions this server speaks, newest first.
const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

pub fn handle(projects: &Projects, message: &Value) -> Option<Value> {
    // A notification has no id and gets no answer.
    let id = message.get("id")?.clone();
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
    let outcome = match method {
        "initialize" => Ok(initialize(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => call(projects, &params),
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    Some(match outcome {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
    })
}

fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = asked.filter(|asked| VERSIONS.contains(asked)).unwrap_or(VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "aiddiction", "version": env!("CARGO_PKG_VERSION") },
        "instructions": "Aiddiction's projects and their kanban boards of task cards. \
            list_projects gives the project ids the other tools take; a project's folder is its path.",
    })
}

fn tools() -> Value {
    let status = json!({ "type": "string", "enum": ["todo", "inProgress", "done"], "description": "The board's column" });
    let project = json!({ "type": "string", "description": "A project id from list_projects" });
    let task = json!({ "type": "string", "description": "A task id from list_tasks" });
    json!([
        {
            "name": "list_projects",
            "description": "List the server's projects: id, name and folder.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "list_tasks",
            "description": "List a project's task cards.",
            "inputSchema": { "type": "object", "required": ["projectId"], "properties": { "projectId": project } },
        },
        {
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
        },
        {
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
        },
        {
            "name": "delete_task",
            "description": "Delete a task card.",
            "inputSchema": { "type": "object", "required": ["projectId", "taskId"], "properties": { "projectId": project, "taskId": task } },
        },
    ])
}

/// A tool's result: its output as text, or what went wrong, flagged.
fn call(projects: &Projects, params: &Value) -> Result<Value, (i64, String)> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let output = match name {
        "list_projects" => Ok(json!(
            projects
                .list()
                .into_iter()
                .map(|project| json!({ "id": project.id, "name": project.name, "path": projects.dir(&project), "state": project.state }))
                .collect::<Vec<_>>()
        )),
        "list_tasks" => id(&arguments, "projectId").and_then(|project| Ok(json!(projects.tasks(project)?))),
        "create_task" => (|| {
            let project = id(&arguments, "projectId")?;
            let input = TaskInput {
                title: text(&arguments, "title").unwrap_or_default(),
                markdown: text(&arguments, "markdown").unwrap_or_default(),
                status: status(&arguments)?,
            };
            Ok(json!(projects.create_task(project, input)?))
        })(),
        "update_task" => (|| {
            let (project, task): (ProjectId, TaskId) = (id(&arguments, "projectId")?, id(&arguments, "taskId")?);
            let known = projects.tasks(project)?.into_iter().find(|known| known.id == task).ok_or("no such task")?;
            let input = TaskInput {
                title: text(&arguments, "title").unwrap_or(known.title),
                markdown: text(&arguments, "markdown").unwrap_or(known.markdown),
                status: status(&arguments)?,
            };
            Ok(json!(projects.update_task(project, task, input)?))
        })(),
        "delete_task" => (|| {
            projects.delete_task(id(&arguments, "projectId")?, id(&arguments, "taskId")?)?;
            Ok(json!({ "deleted": true }))
        })(),
        _ => return Err((-32602, format!("unknown tool: {name}"))),
    };
    Ok(match output {
        Ok(value) => json!({ "content": [{ "type": "text", "text": value.to_string() }] }),
        Err(ToolError(message)) => json!({ "content": [{ "type": "text", "text": message }], "isError": true }),
    })
}

struct ToolError(String);

impl From<&str> for ToolError {
    fn from(message: &str) -> Self {
        ToolError(message.into())
    }
}

impl From<ProjectError> for ToolError {
    fn from(error: ProjectError) -> Self {
        ToolError(format!("{error:#}"))
    }
}

fn text(arguments: &Value, key: &str) -> Option<String> {
    arguments.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn id(arguments: &Value, key: &str) -> Result<uuid::Uuid, ToolError> {
    let value = arguments.get(key).and_then(Value::as_str).ok_or_else(|| ToolError(format!("{key} is required")))?;
    value.parse().map_err(|_| ToolError(format!("{key} is not an id: {value}")))
}

fn status(arguments: &Value) -> Result<Option<TaskStatus>, ToolError> {
    match arguments.get("status") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|_| ToolError(format!("status is one of todo, inProgress, done, not {value}"))),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aiddiction_api::{NewProject, ProjectSource};
    use tokio::sync::broadcast;

    use super::*;
    use crate::store::Store;

    fn projects() -> (Arc<Projects>, ProjectId, [tempfile::TempDir; 2]) {
        let (root, data) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let store = Arc::new(Store::open(data.path()).unwrap());
        let projects = Arc::new(Projects::open(root.path(), data.path(), store, broadcast::channel(8).0).unwrap());
        let project = projects.create(NewProject { name: "web".into(), source: ProjectSource::Empty }).unwrap();
        (projects, project.id, [root, data])
    }

    fn request(projects: &Projects, method: &str, params: Value) -> Value {
        handle(projects, &json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params })).unwrap()
    }

    /// A tool's output, parsed, or its error text.
    fn tool(projects: &Projects, name: &str, arguments: Value) -> Result<Value, String> {
        let result = request(projects, "tools/call", json!({ "name": name, "arguments": arguments }))["result"].clone();
        let text = result["content"][0]["text"].as_str().unwrap().to_owned();
        if result["isError"] == true { Err(text) } else { Ok(serde_json::from_str(&text).unwrap()) }
    }

    #[test]
    fn a_notification_gets_no_answer_and_versions_are_agreed() {
        let (projects, _, _dirs) = projects();
        assert_eq!(handle(&projects, &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })), None);
        let version = |asked: &str| request(&projects, "initialize", json!({ "protocolVersion": asked }))["result"]["protocolVersion"].clone();
        assert_eq!(version("2025-03-26"), "2025-03-26");
        assert_eq!(version("1999-01-01"), VERSIONS[0]);
        assert_eq!(request(&projects, "ping", json!({}))["result"], json!({}));
        assert_eq!(request(&projects, "resources/list", json!({}))["error"]["code"], -32601);
    }

    #[test]
    fn an_agent_finds_the_project_and_works_its_board() {
        let (projects, project, _dirs) = projects();
        let listed = tool(&projects, "list_projects", json!({})).unwrap();
        assert_eq!(listed[0]["id"], json!(project));
        let card = tool(&projects, "create_task", json!({ "projectId": project, "title": "Docs", "markdown": "all of them" })).unwrap();
        assert_eq!(card["status"], "todo");
        // Moving a card is its status alone: the rest stays.
        let moved = tool(&projects, "update_task", json!({ "projectId": project, "taskId": card["id"], "status": "inProgress" })).unwrap();
        assert_eq!((moved["title"].as_str(), moved["markdown"].as_str(), moved["status"].as_str()), (Some("Docs"), Some("all of them"), Some("inProgress")));
        tool(&projects, "delete_task", json!({ "projectId": project, "taskId": card["id"] })).unwrap();
        assert_eq!(tool(&projects, "list_tasks", json!({ "projectId": project })).unwrap(), json!([]));
    }

    #[test]
    fn what_goes_wrong_in_a_tool_is_its_result_not_a_protocol_error() {
        let (projects, project, _dirs) = projects();
        assert!(tool(&projects, "list_tasks", json!({})).unwrap_err().contains("projectId is required"));
        assert!(tool(&projects, "list_tasks", json!({ "projectId": uuid::Uuid::nil() })).unwrap_err().contains("no such project"));
        assert!(tool(&projects, "create_task", json!({ "projectId": project, "title": " " })).unwrap_err().contains("title"));
        assert!(tool(&projects, "create_task", json!({ "projectId": project, "title": "x", "status": "later" })).unwrap_err().contains("status"));
        assert_eq!(request(&projects, "tools/call", json!({ "name": "nope" }))["error"]["code"], -32602);
    }
}
