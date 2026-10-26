//! The ACP proxy between one agent process and any number of client sockets.
//!
//! The server is the agent's ACP client in its own right: it sends
//! `initialize` once, answers every client's `initialize` from that, serves
//! the agent's `fs/*` requests itself inside the project, and pins every
//! session to the project directory. Everything else passes through as raw
//! JSON, so methods this file has never heard of keep working.
//!
//! Lessons taken from multiagent-ui's `acp-ws-bridge.mjs`:
//!
//! - one JSON-RPC message per socket frame, never a fragment of one;
//! - every client numbers its requests from zero, so request ids are rewritten
//!   to proxy-unique ones on the way in and restored on the way out, and a
//!   response goes only to the socket that asked;
//! - a session belongs to the socket that last drove it (`session/new`,
//!   `session/load`, `session/prompt`, attach); its notifications go there,
//!   and only when no owner is connected to everyone.
//!
//! On top of that, every notification about a session is kept in a bounded
//! journal with a sequence number, so a client that lost its socket (a phone
//! that slept) attaches again and is given what it missed.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use aiddiction_api::{AttachParams, AttachResult, PendingPermission, PermissionChoice, PromptFinished, SessionSummary, ext};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

pub type ConnId = u64;

/// How many notifications a session's journal keeps.
const JOURNAL_LIMIT: usize = 5000;
/// How much of the agent's reply a session keeps for its preview.
const REPLY_LIMIT: usize = 2000;
/// How long a preview is.
const PREVIEW_LENGTH: usize = 160;

/// JSON-RPC error codes the proxy answers with.
const INTERNAL_ERROR: i64 = -32603;
const INVALID_PARAMS: i64 = -32602;
const METHOD_NOT_FOUND: i64 = -32601;

pub struct Proxy {
    root: PathBuf,
    to_agent: mpsc::UnboundedSender<String>,
    on_sessions_changed: Box<dyn Fn() + Send + Sync>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    conns: HashMap<ConnId, mpsc::UnboundedSender<String>>,
    next_conn: ConnId,
    next_upstream_id: u64,
    /// client → agent requests awaiting the agent's answer, by upstream id.
    pending_client: HashMap<u64, PendingClient>,
    /// agent → client requests awaiting a person, by the agent's id (as JSON
    /// text, since an id may be a number or a string).
    pending_agent: HashMap<String, PendingAgent>,
    owners: HashMap<String, ConnId>,
    sessions: HashMap<String, Session>,
    /// Session ids in the order they were created, for the summary list.
    order: Vec<String>,
    init: InitState,
    /// The agent's last `_auth/status_update`, for clients that come later.
    auth_status: Option<String>,
    exited: Option<String>,
}

#[derive(Default)]
enum InitState {
    #[default]
    Pending,
    /// Client `initialize` requests that arrived before the agent answered
    /// the server's: (connection, the client's id).
    Waiting(Vec<(ConnId, Value)>),
    Done(Value),
    Failed(String),
}

enum PendingClient {
    Client { conn: ConnId, id: Value, method: String, session: Option<String> },
    Internal(oneshot::Sender<Result<Value, Value>>),
}

struct PendingAgent {
    session: Option<String>,
    method: String,
    line: String,
}

struct Session {
    journal: VecDeque<(u64, String)>,
    next_seq: u64,
    /// Whether anything was ever pushed out of the journal.
    dropped: bool,
    prompts_running: u32,
    title: String,
    created_at_ms: u64,
    updated_at_ms: u64,
    listed: bool,
    /// What the agent has said since its last tool call this turn.
    reply: String,
    /// The title of the tool call the agent started last this turn.
    activity: String,
    /// The start of the agent's last reply, from the last finished turn.
    preview: String,
    error: Option<String>,
    /// Tool call titles by id, for naming a permission request that only
    /// gives the id.
    tools: HashMap<String, String>,
}

impl Session {
    fn new() -> Self {
        let now = now_ms();
        Self {
            journal: VecDeque::new(),
            next_seq: 1,
            dropped: false,
            prompts_running: 0,
            title: String::new(),
            created_at_ms: now,
            updated_at_ms: now,
            listed: false,
            reply: String::new(),
            activity: String::new(),
            preview: String::new(),
            error: None,
            tools: HashMap::new(),
        }
    }
}

/// What a message from either side is.
enum Kind<'a> {
    Request { id: &'a Value, method: &'a str },
    Notification { method: &'a str },
    Response { id: &'a Value },
}

fn kind(message: &Value) -> Option<Kind<'_>> {
    let method = message.get("method").and_then(Value::as_str);
    let id = message.get("id").filter(|id| !id.is_null());
    match (method, id) {
        (Some(method), Some(id)) => Some(Kind::Request { id, method }),
        (Some(method), None) => Some(Kind::Notification { method }),
        (None, Some(id)) => Some(Kind::Response { id }),
        (None, None) => None,
    }
}

fn session_of(message: &Value) -> Option<String> {
    message.pointer("/params/sessionId").and_then(Value::as_str).map(str::to_owned)
}

fn response(id: &Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

fn error_response(id: &Value, code: i64, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }).to_string()
}

fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl Proxy {
    pub fn new(
        root: PathBuf,
        to_agent: mpsc::UnboundedSender<String>,
        on_sessions_changed: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self {
            root,
            to_agent,
            on_sessions_changed: Box::new(on_sessions_changed),
            state: Mutex::new(State::default()),
        }
    }

    /// Sends the server's own `initialize`. The answer is what every client
    /// is told when it initializes.
    pub fn initialize(&self) -> oneshot::Receiver<Result<Value, Value>> {
        let params = json!({
            "protocolVersion": 1,
            "clientCapabilities": {
                "fs": { "readTextFile": true, "writeTextFile": true },
                "terminal": false,
                // A terminal sign-in is run by the server for a client
                // (`login.rs`); a sign-in page is shown by the client.
                "auth": { "terminal": true },
                "elicitation": { "url": {} },
            },
            "clientInfo": { "name": "aiddiction-server", "version": env!("CARGO_PKG_VERSION") },
        });
        let (tx, rx) = oneshot::channel();
        let mut state = self.state.lock().unwrap();
        state.init = InitState::Waiting(Vec::new());
        let id = state.next_upstream_id;
        state.next_upstream_id += 1;
        state.pending_client.insert(id, PendingClient::Internal(tx));
        drop(state);
        let _ = self.to_agent.send(json!({ "jsonrpc": "2.0", "id": id, "method": "initialize", "params": params }).to_string());
        rx
    }

    /// Records how the server's `initialize` ended and answers the clients
    /// that were waiting for it.
    pub fn initialized(&self, outcome: Result<Value, String>) {
        let mut state = self.state.lock().unwrap();
        let waiting = match std::mem::take(&mut state.init) {
            InitState::Waiting(waiting) => waiting,
            _ => Vec::new(),
        };
        for (conn, id) in waiting {
            let line = match &outcome {
                Ok(result) => response(&id, result.clone()),
                Err(message) => error_response(&id, INTERNAL_ERROR, message),
            };
            state.send(conn, line);
            state.send_auth_status(conn);
        }
        state.init = match outcome {
            Ok(result) => InitState::Done(result),
            Err(message) => InitState::Failed(message),
        };
    }

    pub fn connect(&self) -> Option<(ConnId, mpsc::UnboundedReceiver<String>)> {
        let mut state = self.state.lock().unwrap();
        if state.exited.is_some() {
            return None;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        state.next_conn += 1;
        let conn = state.next_conn;
        state.conns.insert(conn, tx);
        Some((conn, rx))
    }

    pub fn disconnect(&self, conn: ConnId) {
        let mut state = self.state.lock().unwrap();
        state.conns.remove(&conn);
        state.owners.retain(|_, owner| *owner != conn);
        if let InitState::Waiting(waiting) = &mut state.init {
            waiting.retain(|(waiting_conn, _)| *waiting_conn != conn);
        }
    }

    /// The agent was signed in some way the proxy did not see (a terminal
    /// sign-in): what it said before is no longer so.
    pub fn forget_auth_status(&self) {
        self.state.lock().unwrap().auth_status = None;
    }

    /// The agent's answer to the server's `initialize`, once it came.
    pub fn init_result(&self) -> Option<Value> {
        match &self.state.lock().unwrap().init {
            InitState::Done(result) => Some(result.clone()),
            _ => None,
        }
    }

    pub fn sessions(&self) -> Vec<SessionSummary> {
        self.state.lock().unwrap().summaries()
    }

    /// Answers a permission request on behalf of a person who has no
    /// conversation open: as a client's answer would, it settles the request
    /// for every client. False when it was already settled.
    pub fn answer_permission(&self, request_id: &Value, option_id: &str) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.pending_agent.remove(&request_id.to_string()).is_none() {
            return false;
        }
        let outcome = json!({ "outcome": { "outcome": "selected", "optionId": option_id } });
        let _ = self.to_agent.send(response(request_id, outcome));
        let resolved = notification(ext::REQUEST_RESOLVED, json!({ "requestId": request_id })).to_string();
        let conns: Vec<_> = state.conns.keys().copied().collect();
        for conn in conns {
            state.send(conn, resolved.clone());
        }
        drop(state);
        (self.on_sessions_changed)();
        true
    }

    /// The agent is gone: every open request is answered with an error and
    /// every socket is closed.
    pub fn agent_exited(&self, message: &str) {
        let mut state = self.state.lock().unwrap();
        state.exited = Some(message.to_owned());
        for (_, pending) in state.pending_client.drain().collect::<Vec<_>>() {
            match pending {
                PendingClient::Client { conn, id, .. } => {
                    let line = error_response(&id, INTERNAL_ERROR, message);
                    state.send(conn, line);
                }
                PendingClient::Internal(tx) => {
                    let _ = tx.send(Err(json!({ "message": message })));
                }
            }
        }
        if let InitState::Waiting(waiting) = std::mem::take(&mut state.init) {
            for (conn, id) in waiting {
                state.send(conn, error_response(&id, INTERNAL_ERROR, message));
            }
        }
        state.init = InitState::Failed(message.to_owned());
        // Nobody can answer a question of an agent that is gone.
        state.pending_agent.clear();
        for session in state.sessions.values_mut() {
            session.prompts_running = 0;
            session.activity.clear();
        }
        // Dropping the senders ends each socket's writer, which closes it.
        state.conns.clear();
    }

    pub fn on_client_line(&self, conn: ConnId, line: &str) {
        let Ok(mut message) = serde_json::from_str::<Value>(line) else {
            tracing::warn!("a client sent something that is not JSON");
            return;
        };
        match kind(&message) {
            Some(Kind::Request { id, method }) => {
                let id = id.clone();
                let method = method.to_owned();
                self.client_request(conn, id, &method, &mut message);
            }
            Some(Kind::Response { id }) => {
                let key = id.to_string();
                let mut state = self.state.lock().unwrap();
                if state.pending_agent.remove(&key).is_some() {
                    let _ = self.to_agent.send(message.to_string());
                    let resolved = notification(ext::REQUEST_RESOLVED, json!({ "requestId": message["id"] })).to_string();
                    let others: Vec<_> = state.conns.keys().copied().filter(|other| *other != conn).collect();
                    for other in others {
                        state.send(other, resolved.clone());
                    }
                    drop(state);
                    (self.on_sessions_changed)();
                }
                // Otherwise another client answered first.
            }
            Some(Kind::Notification { .. }) => {
                let _ = self.to_agent.send(message.to_string());
            }
            None => {}
        }
    }

    fn client_request(&self, conn: ConnId, id: Value, method: &str, message: &mut Value) {
        let mut state = self.state.lock().unwrap();
        match method {
            "initialize" => {
                let line = match &mut state.init {
                    InitState::Done(result) => Some(response(&id, result.clone())),
                    InitState::Failed(error) => Some(error_response(&id, INTERNAL_ERROR, error)),
                    InitState::Waiting(waiting) => {
                        waiting.push((conn, id.clone()));
                        None
                    }
                    InitState::Pending => Some(error_response(&id, INTERNAL_ERROR, "the agent is not running")),
                };
                if let Some(line) = line {
                    state.send(conn, line);
                    state.send_auth_status(conn);
                }
                return;
            }
            ext::ATTACH => {
                // The answer goes first, so a client told `truncated` clears
                // its view before the replay arrives.
                match serde_json::from_value::<AttachParams>(message["params"].clone()) {
                    Ok(params) => {
                        let (result, replay) = state.attach(conn, &params);
                        state.send(conn, response(&id, serde_json::to_value(result).unwrap()));
                        for line in replay {
                            state.send(conn, line);
                        }
                    }
                    Err(error) => state.send(conn, error_response(&id, INVALID_PARAMS, &error.to_string())),
                }
                return;
            }
            "session/new" | "session/load" | "session/resume" => {
                // Whatever the client thinks the directory is, the session
                // lives in the project.
                if let Some(params) = message.get_mut("params").and_then(Value::as_object_mut) {
                    params.insert("cwd".into(), json!(self.root.to_string_lossy()));
                    params.remove("additionalDirectories");
                    params.entry("mcpServers").or_insert_with(|| json!([]));
                }
            }
            _ => {}
        }

        let session = session_of(message);
        if let Some(session) = &session {
            match method {
                "session/load" => {
                    // The agent replays the whole conversation: the journal
                    // starts over with it.
                    let entry = state.sessions.entry(session.clone()).or_insert_with(Session::new);
                    entry.journal.clear();
                    entry.dropped = true;
                    state.list_session(session);
                    state.owners.insert(session.clone(), conn);
                }
                "session/resume" => {
                    state.sessions.entry(session.clone()).or_insert_with(Session::new);
                    state.list_session(session);
                    state.owners.insert(session.clone(), conn);
                }
                "session/prompt" => {
                    state.owners.insert(session.clone(), conn);
                    let text = prompt_text(&message["params"]["prompt"]);
                    let entry = state.sessions.entry(session.clone()).or_insert_with(Session::new);
                    entry.prompts_running += 1;
                    entry.reply.clear();
                    entry.activity.clear();
                    entry.error = None;
                    if entry.title.is_empty() && !text.trim().is_empty() {
                        entry.title = title_from(&text);
                    }
                    // Agents do not echo the prompt back, so the journal gets
                    // it from here: every client of the session, the sender
                    // included, shows the user's message from this one place.
                    let echo = notification(
                        "session/update",
                        json!({
                            "sessionId": session,
                            "update": {
                                "sessionUpdate": "user_message_chunk",
                                "content": { "type": "text", "text": text },
                            },
                        }),
                    );
                    state.publish(session, echo);
                    // The session is busy now, which a list shows.
                    drop(state);
                    (self.on_sessions_changed)();
                    state = self.state.lock().unwrap();
                }
                _ => {}
            }
        }

        let upstream = state.next_upstream_id;
        state.next_upstream_id += 1;
        state.pending_client.insert(
            upstream,
            PendingClient::Client { conn, id, method: method.to_owned(), session },
        );
        message["id"] = json!(upstream);
        drop(state);
        let _ = self.to_agent.send(message.to_string());
    }

    pub fn on_agent_line(&self, line: &str) {
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            tracing::warn!(line = %truncate(line, 200), "the agent wrote something that is not JSON");
            return;
        };
        match kind(&message) {
            Some(Kind::Response { id }) => self.agent_response(id.as_u64(), message.clone()),
            Some(Kind::Request { id, method }) if method.starts_with("fs/") => {
                let id = id.clone();
                let method = method.to_owned();
                let root = self.root.clone();
                let to_agent = self.to_agent.clone();
                tokio::task::spawn_blocking(move || {
                    let line = match serve_fs(&root, &method, &message["params"]) {
                        Ok(result) => response(&id, result),
                        Err((code, error)) => error_response(&id, code, &error),
                    };
                    let _ = to_agent.send(line);
                });
            }
            Some(Kind::Request { id, method }) if method.starts_with("terminal/") => {
                // Not offered in `initialize`; an agent that asks anyway is told no.
                let _ = self.to_agent.send(error_response(id, METHOD_NOT_FOUND, "terminals are not supported"));
            }
            Some(Kind::Request { id, method }) => {
                let session = session_of(&message);
                let key = id.to_string();
                let method = method.to_owned();
                let mut state = self.state.lock().unwrap();
                // A question about a request of a client (the sign-in page
                // of an `authenticate`) goes to that client, in its ids.
                let (audience, line) = match state.asker_of(&message) {
                    Some((conn, client_id)) => {
                        let mut message = message;
                        message["params"]["requestId"] = client_id;
                        (vec![conn], message.to_string())
                    }
                    None => (state.audience(session.as_deref()), line.to_owned()),
                };
                state.pending_agent.insert(key, PendingAgent { session, method, line: line.clone() });
                for conn in audience {
                    state.send(conn, line.clone());
                }
                drop(state);
                (self.on_sessions_changed)();
            }
            Some(Kind::Notification { method }) => {
                let mut state = self.state.lock().unwrap();
                match session_of(&message) {
                    Some(session) => {
                        let changed = method == "session/update" && state.follow(&session, &message);
                        state.publish(&session, message);
                        if changed {
                            drop(state);
                            (self.on_sessions_changed)();
                        }
                    }
                    None => {
                        if method == ext::AUTH_STATUS {
                            state.auth_status = Some(line.to_owned());
                        }
                        for conn in state.audience(None) {
                            state.send(conn, line.to_owned());
                        }
                    }
                }
            }
            None => {}
        }
    }

    fn agent_response(&self, upstream: Option<u64>, mut message: Value) {
        let mut state = self.state.lock().unwrap();
        let Some(pending) = upstream.and_then(|id| state.pending_client.remove(&id)) else {
            return;
        };
        match pending {
            PendingClient::Internal(tx) => {
                let outcome = match message.get("error") {
                    Some(error) => Err(error.clone()),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = tx.send(outcome);
            }
            PendingClient::Client { conn, id, method, session } => {
                let mut sessions_changed = false;
                if message.get("error").is_none() && method == "session/new"
                    && let Some(new) = message.pointer("/result/sessionId").and_then(Value::as_str) {
                        let new = new.to_owned();
                        state.sessions.entry(new.clone()).or_insert_with(Session::new);
                        state.list_session(&new);
                        state.owners.insert(new, conn);
                        sessions_changed = true;
                    }
                if method == "authenticate" && message.get("error").is_none() {
                    state.auth_status = None;
                }
                if method == "session/prompt"
                    && let Some(session) = &session {
                        let finished = PromptFinished {
                            session_id: session.clone(),
                            stop_reason: message.pointer("/result/stopReason").and_then(Value::as_str).map(str::to_owned),
                            error: message.pointer("/error/message").and_then(Value::as_str).map(str::to_owned),
                        };
                        if let Some(entry) = state.sessions.get_mut(session) {
                            entry.prompts_running = entry.prompts_running.saturating_sub(1);
                            entry.activity.clear();
                            let preview = preview_of(&entry.reply);
                            if !preview.is_empty() {
                                entry.preview = preview;
                            }
                            entry.error = finished.error.clone();
                        }
                        sessions_changed = true;
                        let note = notification(
                            ext::PROMPT_FINISHED,
                            json!({ "sessionId": session, "stopReason": finished.stop_reason, "error": finished.error }),
                        );
                        state.publish(session, note);
                    }
                message["id"] = id;
                state.send(conn, message.to_string());
                if sessions_changed {
                    drop(state);
                    (self.on_sessions_changed)();
                }
            }
        }
    }
}

impl State {
    fn send(&mut self, conn: ConnId, line: String) {
        if let Some(tx) = self.conns.get(&conn)
            && tx.send(line).is_err() {
                self.conns.remove(&conn);
            }
    }

    fn send_auth_status(&mut self, conn: ConnId) {
        if let Some(line) = self.auth_status.clone() {
            self.send(conn, line);
        }
    }

    /// The connection, and its own id, of the client request an agent
    /// request names in `params.requestId`, while it is still open.
    fn asker_of(&self, message: &Value) -> Option<(ConnId, Value)> {
        let upstream = message.pointer("/params/requestId")?.as_u64()?;
        match self.pending_client.get(&upstream)? {
            PendingClient::Client { conn, id, .. } => Some((*conn, id.clone())),
            PendingClient::Internal(_) => None,
        }
    }

    /// Who hears about a session: its owner while connected, else everyone.
    fn audience(&self, session: Option<&str>) -> Vec<ConnId> {
        if let Some(owner) = session.and_then(|session| self.owners.get(session))
            && self.conns.contains_key(owner) {
                return vec![*owner];
            }
        self.conns.keys().copied().collect()
    }

    fn list_session(&mut self, session: &str) {
        if let Some(entry) = self.sessions.get_mut(session)
            && !entry.listed {
                entry.listed = true;
                self.order.push(session.to_owned());
            }
    }

    /// Numbers a notification, keeps it, and sends it to the session's audience.
    fn publish(&mut self, session: &str, mut message: Value) {
        let entry = self.sessions.entry(session.to_owned()).or_insert_with(Session::new);
        let seq = entry.next_seq;
        entry.next_seq += 1;
        entry.updated_at_ms = now_ms();
        if let Some(params) = message.get_mut("params").and_then(Value::as_object_mut) {
            let meta = params.entry("_meta").or_insert_with(|| json!({}));
            if let Some(meta) = meta.as_object_mut() {
                meta.insert(ext::SEQ_META.into(), json!(seq));
            }
        }
        let line = message.to_string();
        entry.journal.push_back((seq, line.clone()));
        if entry.journal.len() > JOURNAL_LIMIT {
            entry.journal.pop_front();
            entry.dropped = true;
        }
        for conn in self.audience(Some(session)) {
            self.send(conn, line.clone());
        }
    }

    /// Makes `conn` the session's owner. Returns the answer and what to send
    /// after it: the journal past `after_seq`, then the agent's open questions.
    fn attach(&mut self, conn: ConnId, params: &AttachParams) -> (AttachResult, Vec<String>) {
        let Some(entry) = self.sessions.get(&params.session_id) else {
            return (AttachResult { known: false, truncated: false, busy: false, last_seq: 0 }, Vec::new());
        };
        let first = entry.journal.front().map(|(seq, _)| *seq).unwrap_or(entry.next_seq);
        let truncated = entry.dropped && first > params.after_seq + 1;
        let mut replay: Vec<String> = entry
            .journal
            .iter()
            .filter(|(seq, _)| *seq > params.after_seq)
            .map(|(_, line)| line.clone())
            .collect();
        let result = AttachResult {
            known: true,
            truncated,
            busy: entry.prompts_running > 0,
            last_seq: entry.next_seq - 1,
        };
        self.owners.insert(params.session_id.clone(), conn);
        // Questions the agent is still waiting on go to the new owner too.
        replay.extend(
            self.pending_agent
                .values()
                .filter(|pending| pending.session.as_deref() == Some(params.session_id.as_str()))
                .map(|pending| pending.line.clone()),
        );
        (result, replay)
    }

    /// Keeps what a list of sessions shows up to date with an update from
    /// the agent: the title it gives the session, its reply, the tool it
    /// runs. True when a list should be read again.
    fn follow(&mut self, session: &str, message: &Value) -> bool {
        let update = &message["params"]["update"];
        let entry = self.sessions.entry(session.to_owned()).or_insert_with(Session::new);
        match update["sessionUpdate"].as_str().unwrap_or_default() {
            "session_info_update" => match update.get("title").and_then(Value::as_str) {
                Some(title) if entry.title != title => {
                    entry.title = title.to_owned();
                    true
                }
                _ => false,
            },
            "agent_message_chunk" => {
                if let Some(text) = update.pointer("/content/text").and_then(Value::as_str) {
                    let room = REPLY_LIMIT.saturating_sub(entry.reply.len());
                    entry.reply.push_str(&text[..floor_char_boundary(text, room)]);
                }
                false
            }
            "tool_call" => {
                let id = update["toolCallId"].as_str().unwrap_or_default().to_owned();
                let title = update["title"].as_str().unwrap_or_default().to_owned();
                // What the agent says after its last tool is what sums the
                // turn up.
                entry.reply.clear();
                let fresh = !entry.tools.contains_key(&id);
                entry.tools.insert(id, title.clone());
                if fresh && !title.is_empty() {
                    entry.activity = title;
                    return true;
                }
                false
            }
            "tool_call_update" => {
                if let (Some(id), Some(title)) = (update["toolCallId"].as_str(), update["title"].as_str()) {
                    entry.tools.insert(id.to_owned(), title.to_owned());
                }
                false
            }
            _ => false,
        }
    }

    fn summaries(&self) -> Vec<SessionSummary> {
        self.order
            .iter()
            .filter_map(|id| {
                let session = self.sessions.get(id)?;
                let busy = session.prompts_running > 0;
                let mut asked: Vec<_> = self
                    .pending_agent
                    .iter()
                    .filter(|(_, pending)| pending.session.as_deref() == Some(id.as_str()) && pending.method == "session/request_permission")
                    .collect();
                asked.sort_by(|a, b| a.0.cmp(b.0));
                let permissions = asked.into_iter().filter_map(|(_, pending)| permission_of(&pending.line, &session.tools)).collect();
                Some(SessionSummary {
                    session_id: id.clone(),
                    title: session.title.clone(),
                    created_at_ms: session.created_at_ms,
                    updated_at_ms: session.updated_at_ms,
                    last_seq: session.next_seq - 1,
                    busy,
                    permissions,
                    preview: if busy && !session.activity.is_empty() { session.activity.clone() } else { session.preview.clone() },
                    error: session.error.clone(),
                })
            })
            .collect()
    }
}

fn prompt_text(prompt: &Value) -> String {
    let mut text = String::new();
    for block in prompt.as_array().into_iter().flatten() {
        match block["type"].as_str() {
            Some("text") => text.push_str(block["text"].as_str().unwrap_or_default()),
            Some("resource_link") => {
                text.push_str(&format!("[{}]", block["name"].as_str().unwrap_or("file")));
            }
            _ => {}
        }
    }
    text
}

/// A `session/request_permission` line as a list shows it.
fn permission_of(line: &str, tools: &HashMap<String, String>) -> Option<PendingPermission> {
    let message: Value = serde_json::from_str(line).ok()?;
    let params = &message["params"];
    let title = params
        .pointer("/toolCall/title")
        .and_then(Value::as_str)
        .or_else(|| params.pointer("/toolCall/toolCallId").and_then(Value::as_str).and_then(|id| tools.get(id)).map(String::as_str))
        .filter(|title| !title.is_empty())
        .unwrap_or("The agent asks for permission")
        .to_owned();
    let options = params["options"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|option| PermissionChoice {
            id: option["optionId"].as_str().unwrap_or_default().to_owned(),
            name: option["name"].as_str().unwrap_or_default().to_owned(),
            kind: option["kind"].as_str().unwrap_or_default().to_owned(),
        })
        .collect();
    Some(PendingPermission { request_id: message["id"].clone(), title, options })
}

/// The start of a reply on one line, without the markdown that would show
/// as stray characters.
fn preview_of(reply: &str) -> String {
    let plain: String = reply.chars().filter(|c| !matches!(c, '*' | '`' | '#')).collect();
    let line = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate(&line, PREVIEW_LENGTH)
}

/// The largest index at most `at` that starts a character of `text`.
fn floor_char_boundary(text: &str, at: usize) -> usize {
    if at >= text.len() {
        return text.len();
    }
    (0..=at).rev().find(|index| text.is_char_boundary(*index)).unwrap_or(0)
}

fn title_from(text: &str) -> String {
    let line = text.lines().find(|line| !line.trim().is_empty()).unwrap_or_default().trim();
    truncate(line, 60)
}

fn truncate(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

/// `fs/read_text_file` and `fs/write_text_file`, confined to the project.
fn serve_fs(root: &Path, method: &str, params: &Value) -> Result<Value, (i64, String)> {
    let invalid = |message: String| (INVALID_PARAMS, message);
    let path = params["path"].as_str().ok_or_else(|| invalid("path is required".into()))?;
    let root = root.canonicalize().map_err(|error| (INTERNAL_ERROR, error.to_string()))?;
    let path = if Path::new(path).is_absolute() { PathBuf::from(path) } else { root.join(path) };
    match method {
        "fs/read_text_file" => {
            let path = path.canonicalize().map_err(|error| invalid(format!("{}: {error}", path.display())))?;
            if !path.starts_with(&root) {
                return Err(invalid(format!("{} is outside the project", path.display())));
            }
            let text = std::fs::read_to_string(&path).map_err(|error| invalid(format!("{}: {error}", path.display())))?;
            let line = params["line"].as_u64().unwrap_or(1).max(1) as usize;
            let limit = params["limit"].as_u64().map(|limit| limit as usize);
            let content = if line == 1 && limit.is_none() {
                text
            } else {
                let lines = text.split_inclusive('\n').skip(line - 1);
                match limit {
                    Some(limit) => lines.take(limit).collect(),
                    None => lines.collect(),
                }
            };
            Ok(json!({ "content": content }))
        }
        "fs/write_text_file" => {
            let content = params["content"].as_str().ok_or_else(|| invalid("content is required".into()))?;
            // Checked before anything is created, then again once symlinks
            // can be resolved.
            let path = normalize(&path);
            if !path.starts_with(&root) {
                return Err(invalid(format!("{} is outside the project", path.display())));
            }
            let name = path.file_name().ok_or_else(|| invalid("not a file path".into()))?.to_owned();
            let parent = path.parent().ok_or_else(|| invalid("not a file path".into()))?;
            std::fs::create_dir_all(parent).map_err(|error| (INTERNAL_ERROR, format!("{}: {error}", parent.display())))?;
            let parent = parent.canonicalize().map_err(|error| invalid(format!("{}: {error}", parent.display())))?;
            if !parent.starts_with(&root) {
                return Err(invalid(format!("{} is outside the project", path.display())));
            }
            let target = parent.join(name);
            if let Ok(resolved) = target.canonicalize()
                && !resolved.starts_with(&root) {
                    return Err(invalid(format!("{} is outside the project", target.display())));
                }
            std::fs::write(&target, content).map_err(|error| (INTERNAL_ERROR, format!("{}: {error}", target.display())))?;
            Ok(Value::Null)
        }
        _ => Err((METHOD_NOT_FOUND, format!("{method} is not supported"))),
    }
}

/// Resolves `.` and `..` without touching the filesystem.
fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normal.pop();
            }
            Component::CurDir => {}
            other => normal.push(other),
        }
    }
    normal
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Harness {
        proxy: Proxy,
        agent: mpsc::UnboundedReceiver<String>,
        _dir: tempfile::TempDir,
    }

    fn harness() -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let (tx, agent) = mpsc::unbounded_channel();
        let proxy = Proxy::new(dir.path().to_path_buf(), tx, || {});
        Harness { proxy, agent, _dir: dir }
    }

    fn json_of(line: String) -> Value {
        serde_json::from_str(&line).unwrap()
    }

    impl Harness {
        fn ready(&mut self) {
            drop(self.proxy.initialize());
            let init = json_of(self.agent.try_recv().unwrap());
            assert_eq!(init["method"], "initialize");
            self.proxy.on_agent_line(&json!({ "jsonrpc": "2.0", "id": init["id"], "result": { "protocolVersion": 1 } }).to_string());
            self.proxy.initialized(Ok(json!({ "protocolVersion": 1 })));
        }
    }

    #[test]
    fn request_ids_are_rewritten_and_answers_go_back_to_the_asker() {
        let mut h = harness();
        h.ready();
        let (a, mut a_rx) = h.proxy.connect().unwrap();
        let (b, mut b_rx) = h.proxy.connect().unwrap();
        h.proxy.on_client_line(a, r#"{"jsonrpc":"2.0","id":0,"method":"session/new","params":{"cwd":"/elsewhere"}}"#);
        h.proxy.on_client_line(b, r#"{"jsonrpc":"2.0","id":0,"method":"session/new","params":{"cwd":"."}}"#);
        let first = json_of(h.agent.try_recv().unwrap());
        let second = json_of(h.agent.try_recv().unwrap());
        assert_ne!(first["id"], second["id"]);
        assert_eq!(first["params"]["cwd"], json!(h._dir.path().to_string_lossy()));

        h.proxy.on_agent_line(&json!({ "jsonrpc": "2.0", "id": second["id"], "result": { "sessionId": "s-b" } }).to_string());
        let answer = json_of(b_rx.try_recv().unwrap());
        assert_eq!(answer["id"], 0);
        assert_eq!(answer["result"]["sessionId"], "s-b");
        assert!(a_rx.try_recv().is_err());
    }

    #[test]
    fn notifications_go_to_the_owner_and_are_replayed_on_attach() {
        let mut h = harness();
        h.ready();
        let (a, mut a_rx) = h.proxy.connect().unwrap();
        let (_b, mut b_rx) = h.proxy.connect().unwrap();
        h.proxy.on_client_line(a, r#"{"jsonrpc":"2.0","id":1,"method":"session/new","params":{}}"#);
        let new = json_of(h.agent.try_recv().unwrap());
        h.proxy.on_agent_line(&json!({ "jsonrpc": "2.0", "id": new["id"], "result": { "sessionId": "s" } }).to_string());
        a_rx.try_recv().unwrap();

        let chunk = json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": "s", "update": { "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "hi" } } } });
        h.proxy.on_agent_line(&chunk.to_string());
        let got = json_of(a_rx.try_recv().unwrap());
        assert_eq!(got["params"]["_meta"][ext::SEQ_META], 1);
        assert!(b_rx.try_recv().is_err(), "a session's updates go to its owner only");

        h.proxy.disconnect(a);
        let (c, mut c_rx) = h.proxy.connect().unwrap();
        h.proxy.on_client_line(c, r#"{"jsonrpc":"2.0","id":5,"method":"_aiddiction/attach","params":{"sessionId":"s","afterSeq":0}}"#);
        let attached = json_of(c_rx.try_recv().unwrap());
        assert_eq!(attached["result"]["known"], true);
        assert_eq!(attached["result"]["lastSeq"], 1);
        let replayed = json_of(c_rx.try_recv().unwrap());
        assert_eq!(replayed["params"]["update"]["content"]["text"], "hi");
    }

    #[test]
    fn a_prompt_is_echoed_into_the_journal_and_its_end_announced() {
        let mut h = harness();
        h.ready();
        let (a, mut a_rx) = h.proxy.connect().unwrap();
        h.proxy.on_client_line(a, r#"{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"s","prompt":[{"type":"text","text":"do it"}]}}"#);
        let echo = json_of(a_rx.try_recv().unwrap());
        assert_eq!(echo["params"]["update"]["sessionUpdate"], "user_message_chunk");
        let prompt = json_of(h.agent.try_recv().unwrap());
        h.proxy.on_agent_line(&json!({ "jsonrpc": "2.0", "id": prompt["id"], "result": { "stopReason": "end_turn" } }).to_string());
        let finished = json_of(a_rx.try_recv().unwrap());
        assert_eq!(finished["method"], ext::PROMPT_FINISHED);
        let answer = json_of(a_rx.try_recv().unwrap());
        assert_eq!(answer["id"], 3);
        assert_eq!(h.proxy.sessions().len(), 0, "a prompt alone does not list a session");
    }

    #[test]
    fn the_first_answer_to_a_permission_request_wins() {
        let mut h = harness();
        h.ready();
        let (a, mut a_rx) = h.proxy.connect().unwrap();
        let (b, mut b_rx) = h.proxy.connect().unwrap();
        h.proxy.on_agent_line(r#"{"jsonrpc":"2.0","id":"p1","method":"session/request_permission","params":{"sessionId":"s","options":[]}}"#);
        a_rx.try_recv().unwrap();
        b_rx.try_recv().unwrap();
        h.proxy.on_client_line(a, r#"{"jsonrpc":"2.0","id":"p1","result":{"outcome":{"outcome":"cancelled"}}}"#);
        h.proxy.on_client_line(b, r#"{"jsonrpc":"2.0","id":"p1","result":{"outcome":{"outcome":"cancelled"}}}"#);
        assert!(h.agent.try_recv().is_ok());
        assert!(h.agent.try_recv().is_err());
        assert_eq!(json_of(b_rx.try_recv().unwrap())["method"], ext::REQUEST_RESOLVED);
    }

    #[test]
    fn the_list_shows_a_turn_its_questions_and_its_reply() {
        let mut h = harness();
        h.ready();
        let (a, mut a_rx) = h.proxy.connect().unwrap();
        h.proxy.on_client_line(a, r#"{"jsonrpc":"2.0","id":1,"method":"session/new","params":{}}"#);
        let new = json_of(h.agent.try_recv().unwrap());
        h.proxy.on_agent_line(&json!({ "jsonrpc": "2.0", "id": new["id"], "result": { "sessionId": "s" } }).to_string());
        h.proxy.on_client_line(a, r#"{"jsonrpc":"2.0","id":2,"method":"session/prompt","params":{"sessionId":"s","prompt":[{"type":"text","text":"fix the login\nplease"}]}}"#);
        let prompt = json_of(h.agent.try_recv().unwrap());

        h.proxy.on_agent_line(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Looking."}}}}"#);
        h.proxy.on_agent_line(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call","toolCallId":"t","title":"Run npm test"}}}"#);
        h.proxy.on_agent_line(r#"{"jsonrpc":"2.0","id":"p","method":"session/request_permission","params":{"sessionId":"s","toolCall":{"toolCallId":"t"},"options":[{"optionId":"y","name":"Allow","kind":"allow_once"}]}}"#);
        let listed = &h.proxy.sessions()[0];
        assert_eq!(listed.title, "fix the login");
        assert!(listed.busy);
        assert_eq!(listed.preview, "Run npm test", "while a turn runs, its tool");
        assert_eq!(listed.permissions[0].title, "Run npm test");
        assert_eq!(listed.permissions[0].request_id, json!("p"));
        assert_eq!(listed.permissions[0].options[0].kind, "allow_once");

        assert!(h.proxy.answer_permission(&json!("p"), "y"));
        assert!(!h.proxy.answer_permission(&json!("p"), "y"), "answered once");
        let answer = json_of(h.agent.try_recv().unwrap());
        assert_eq!(answer["id"], "p");
        assert_eq!(answer["result"]["outcome"]["optionId"], "y");
        assert!(h.proxy.sessions()[0].permissions.is_empty());

        h.proxy.on_agent_line(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"**Fixed**: the\n`returnTo` check."}}}}"#);
        h.proxy.on_agent_line(&json!({ "jsonrpc": "2.0", "id": prompt["id"], "result": { "stopReason": "end_turn" } }).to_string());
        let listed = &h.proxy.sessions()[0];
        assert!(!listed.busy);
        assert_eq!(listed.preview, "Fixed: the returnTo check.", "after a turn, what the agent said after its last tool");
        assert!(listed.last_seq >= 4);
        while a_rx.try_recv().is_ok() {}
    }

    #[test]
    fn client_initialize_waits_for_the_agent() {
        let mut h = harness();
        drop(h.proxy.initialize());
        h.agent.try_recv().unwrap();
        let (a, mut a_rx) = h.proxy.connect().unwrap();
        h.proxy.on_client_line(a, r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}"#);
        assert!(a_rx.try_recv().is_err());
        h.proxy.initialized(Ok(json!({ "protocolVersion": 1, "agentInfo": { "name": "x" } })));
        let answer = json_of(a_rx.try_recv().unwrap());
        assert_eq!(answer["result"]["agentInfo"]["name"], "x");
    }

    #[test]
    fn a_client_that_initializes_is_told_the_last_auth_status() {
        let mut h = harness();
        h.ready();
        h.proxy.on_agent_line(r#"{"jsonrpc":"2.0","method":"_auth/status_update","params":{"authStatus":{"kind":"none"}}}"#);
        let (a, mut a_rx) = h.proxy.connect().unwrap();
        h.proxy.on_client_line(a, r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}"#);
        assert_eq!(json_of(a_rx.try_recv().unwrap())["id"], 0);
        let status = json_of(a_rx.try_recv().unwrap());
        assert_eq!(status["method"], ext::AUTH_STATUS);
        assert_eq!(status["params"]["authStatus"]["kind"], "none");
    }

    #[test]
    fn a_sign_in_page_goes_to_the_client_that_asked_to_sign_in() {
        let mut h = harness();
        h.ready();
        let (a, mut a_rx) = h.proxy.connect().unwrap();
        let (_b, mut b_rx) = h.proxy.connect().unwrap();
        h.proxy.on_client_line(a, r#"{"jsonrpc":"2.0","id":7,"method":"authenticate","params":{"methodId":"chat-gpt-device-code"}}"#);
        let authenticate = json_of(h.agent.try_recv().unwrap());
        let page = json!({ "jsonrpc": "2.0", "id": 0, "method": "elicitation/create", "params": { "mode": "url", "requestId": authenticate["id"], "url": "https://x/device", "message": "code", "elicitationId": "e" } });
        h.proxy.on_agent_line(&page.to_string());
        let got = json_of(a_rx.try_recv().unwrap());
        assert_eq!(got["params"]["requestId"], 7, "in the client's own ids");
        assert!(b_rx.try_recv().is_err());

        h.proxy.on_client_line(a, r#"{"jsonrpc":"2.0","id":0,"result":{"action":"accept"}}"#);
        assert_eq!(json_of(h.agent.try_recv().unwrap())["result"]["action"], "accept");
    }

    #[test]
    fn files_outside_the_project_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let read = serve_fs(dir.path(), "fs/read_text_file", &json!({ "path": "a.txt", "line": 2, "limit": 1 })).unwrap();
        assert_eq!(read["content"], "two\n");
        assert!(serve_fs(dir.path(), "fs/read_text_file", &json!({ "path": "/etc/hostname" })).is_err());
        assert!(serve_fs(dir.path(), "fs/write_text_file", &json!({ "path": "../escape.txt", "content": "x" })).is_err());
        serve_fs(dir.path(), "fs/write_text_file", &json!({ "path": "sub/b.txt", "content": "x" })).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("sub/b.txt")).unwrap(), "x");
    }
}
