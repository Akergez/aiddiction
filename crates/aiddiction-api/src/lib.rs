//! What the server and its clients say to each other over REST and the event
//! socket, and the names of the ACP extensions the server's proxy adds.
//!
//! Nothing here knows about Tokio, HTTP libraries or the interface: the
//! server and the client crate both build on these types.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Bumped when a change to the REST or socket shapes breaks an older peer.
pub const API_VERSION: u32 = 1;

/// The ACP extensions the server's proxy speaks besides the protocol itself.
pub mod ext {
    /// Client → server request: make this connection the owner of a session
    /// and replay what it missed. Params [`super::AttachParams`], result
    /// [`super::AttachResult`].
    pub const ATTACH: &str = "_aiddiction/attach";
    /// Server → client notification sent when the agent answers a
    /// `session/prompt`, so a client that lost the request still learns that
    /// the turn ended. Params [`super::PromptFinished`].
    pub const PROMPT_FINISHED: &str = "_aiddiction/prompt_finished";
    /// Server → client notification: an agent request one client answered is
    /// settled, the others should drop it. Params `{ "requestId": … }`.
    pub const REQUEST_RESOLVED: &str = "_aiddiction/request_resolved";
    /// Agent → client notification of who the agent is signed in as
    /// (`{ "authStatus": { "kind": "none", … } }` when it is not), from the
    /// Claude and Codex adapters. The server keeps the last one and sends it
    /// to every client that initializes.
    pub const AUTH_STATUS: &str = "_auth/status_update";
    /// The key under `_meta` that carries the journal sequence number of a
    /// replayable notification.
    pub const SEQ_META: &str = "aiddictionSeq";
}

pub type ProjectId = Uuid;
pub type InstanceId = Uuid;
pub type TaskId = Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    pub ok: bool,
    pub version: String,
    pub api_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub api_version: u32,
    pub projects_root: String,
}

/// How an agent is started, in the shape Zed's `agent_servers` settings use:
/// a command, its arguments and extra environment. The command is a shell
/// command line, so `npx -y some-acp` and `FOO=1 my-agent --acp` both work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCommand {
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentSource {
    /// Shipped with the server (Claude, Codex).
    Builtin,
    /// From the server's configuration file; read-only over the API.
    Config,
    /// Added through the API; can be edited and removed by clients.
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub source: AgentSource,
    /// Shown to clients so a person can tell what will run on the server.
    pub command: AgentCommand,
}

/// Body of `POST /agents` and `PUT /agents/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInput {
    pub name: String,
    #[serde(flatten)]
    pub command: AgentCommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectState {
    Ready,
    Cloning,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    /// The folder's name: under the projects root, or the last part of `path`.
    pub dir_name: String,
    /// A folder elsewhere on the server, taken as it is; `None` for one the
    /// server made under its projects root. The server never deletes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub state: ProjectState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_url: Option<String>,
    /// The last line `git clone` printed, or why it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum ProjectSource {
    Empty,
    /// A folder that already exists on the server (`~` is its user's home).
    Existing { path: String },
    Git {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        /// An HTTPS token; handed to git through `GIT_ASKPASS`, never stored.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<String>,
    },
}

/// `POST /projects/{id}/review/diff` answers with the unified diff the
/// request (a `diffreview::DiffRequest`) asks for, as git wrote it: the
/// client parses it with the same code a standalone review tool uses.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDiff {
    pub diff: String,
}

/// `GET /fs/dirs?path=…`: the folders in one folder on the server, for
/// picking an existing project folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirListing {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub dirs: Vec<DirEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewProject {
    pub name: String,
    pub source: ProjectSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskStatus { Todo, InProgress, Done }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: TaskId,
    pub project_id: ProjectId,
    pub title: String,
    #[serde(default)]
    pub markdown: String,
    pub status: TaskStatus,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskInput {
    pub title: String,
    #[serde(default)]
    pub markdown: String,
    #[serde(default)]
    pub status: Option<TaskStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InstanceState {
    Starting,
    Ready,
    Exited,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub session_id: String,
    pub title: String,
    pub created_at_ms: u64,
    /// When the session's journal last grew.
    #[serde(default)]
    pub updated_at_ms: u64,
    /// The journal's last sequence number: a client that showed less has
    /// something unread.
    #[serde(default)]
    pub last_seq: u64,
    /// A `session/prompt` is running.
    #[serde(default)]
    pub busy: bool,
    /// The agent's questions nobody has answered yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<PendingPermission>,
    /// What the agent is doing (the running tool call) or, after a turn, the
    /// start of the last thing it said.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub preview: String,
    /// Why the last turn failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A `session/request_permission` the agent is waiting on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingPermission {
    /// The agent's JSON-RPC id, a number or a string.
    pub request_id: serde_json::Value,
    pub title: String,
    pub options: Vec<PermissionChoice>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionChoice {
    pub id: String,
    pub name: String,
    /// ACP's `allow_once`, `allow_always`, `reject_once`, `reject_always`.
    pub kind: String,
}

/// Body of `POST /instances/{id}/permission`: an answer given without a
/// conversation open, as if from one.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionAnswer {
    pub request_id: serde_json::Value,
    pub option_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Instance {
    pub id: InstanceId,
    pub project_id: ProjectId,
    pub agent_id: String,
    pub agent_name: String,
    pub state: InstanceState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub sessions: Vec<SessionSummary>,
    /// The last lines the agent wrote to stderr.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stderr_tail: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartInstance {
    pub agent_id: String,
}

/// Body of `POST /instances/{id}/login`: one of the agent's `authMethods`
/// of type `terminal`, which the server runs with pipes for a terminal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartLogin {
    pub method_id: String,
}

/// Body of `POST /instances/{id}/login/input`: a line for the login to read.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginInput {
    pub text: String,
}

/// An agent's terminal sign-in running on the server, as a client shows it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Login {
    pub method_id: String,
    /// What it wrote to its terminal, without escape sequences: the tail
    /// when there was a lot.
    pub output: String,
    pub running: bool,
    /// Whether it ended well, once it ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
}

/// What the event socket carries. Clients re-read the affected list; the
/// events say what changed, not how.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum ServerEvent {
    ProjectsChanged,
    AgentsChanged,
    InstancesChanged { project_id: ProjectId },
    TasksChanged { project_id: ProjectId },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ApiError,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachParams {
    pub session_id: String,
    /// The last journal sequence number this client has applied; 0 for none.
    #[serde(default)]
    pub after_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachResult {
    /// Whether the server knows the session at all. It does not after a
    /// restart of the agent; the client then asks the agent with
    /// `session/load`.
    pub known: bool,
    /// The journal no longer holds everything after `after_seq`; what was
    /// replayed starts later, and the client should start its view afresh.
    pub truncated: bool,
    /// Whether a `session/prompt` for the session is still running.
    pub busy: bool,
    pub last_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptFinished {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_input_reads_zed_shaped_settings() {
        let input: AgentInput = serde_json::from_str(
            r#"{"name":"Mine","command":"npx -y my-acp","args":["--acp"],"env":{"A":"1"}}"#,
        )
        .unwrap();
        assert_eq!(input.command.command, "npx -y my-acp");
        assert_eq!(input.command.args, vec!["--acp"]);
        assert_eq!(input.command.env["A"], "1");
    }

    #[test]
    fn project_source_is_tagged_by_kind() {
        let source: ProjectSource =
            serde_json::from_str(r#"{"kind":"git","url":"https://x/y.git"}"#).unwrap();
        assert!(matches!(source, ProjectSource::Git { branch: None, .. }));
    }
}
