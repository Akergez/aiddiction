//! One conversation with an agent instance on the server: an ACP client over
//! the server's socket, a JSON-RPC layer of its own, and the [`Thread`] the
//! chat draws.
//!
//! Losing the socket is ordinary (a phone that slept): the session connects
//! again and attaches with the last sequence number it applied, and the
//! server replays what it missed.
//!
//! An agent that is not signed in refuses with "authentication required";
//! the chat then offers the agent's ways of signing in ([`SignIn`]).

use std::collections::HashMap;
use std::time::Duration;

use aiddiction_api::{AttachResult, InstanceId, InstanceState, Login, ext};
use aiddiction_client::ServerClient;
use gpui_kit::Context;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::login::{self, AUTH_REQUIRED, AuthMethod, MethodKind};
use crate::runtime;
use crate::thread::Thread;

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Connecting,
    Ready,
    Reconnecting(String),
    /// The agent will not open the conversation until it is signed in.
    SignIn,
    Closed(String),
}

/// A JSON-RPC error, or a request that never reached the agent (code 0).
#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    fn local(message: impl Into<String>) -> Self {
        Self {
            code: 0,
            message: message.into(),
        }
    }

    fn auth_required(&self) -> bool {
        self.code == AUTH_REQUIRED
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Signing the agent in from the chat.
#[derive(Default)]
pub struct SignIn {
    /// The agent says it is signed out, or refused something until it is.
    pub needed: bool,
    /// What the agent offers.
    pub methods: Vec<AuthMethod>,
    /// The way being tried: an API key waits for the key here.
    pub method: Option<AuthMethod>,
    /// Waiting for the agent or the server.
    pub busy: bool,
    /// The terminal sign-in on the server, as it was last read.
    pub terminal: Option<Login>,
    /// A page the agent asks the person to open.
    pub page: Option<SignInPage>,
    pub error: Option<String>,
    /// A message was refused for want of it.
    refused: bool,
    polling: u64,
}

impl SignIn {
    /// Whether the chat shows it at all.
    pub fn shown(&self, phase: &Phase) -> bool {
        self.needed || self.method.is_some() || *phase == Phase::SignIn
    }
}

/// An `elicitation/create` in URL mode: open this, do what it says.
#[derive(Debug, Clone)]
pub struct SignInPage {
    request_id: Value,
    elicitation_id: Option<String>,
    pub url: String,
    pub message: String,
    opened: bool,
}

type Callback =
    Box<dyn FnOnce(&mut AgentSession, Result<Value, RpcError>, &mut Context<AgentSession>)>;

pub struct AgentSession {
    client: ServerClient,
    pub instance_id: InstanceId,
    pub agent_name: String,
    pub project_name: String,
    pub session_id: Option<String>,
    pub phase: Phase,
    pub busy: bool,
    pub thread: Thread,
    pub sign_in: SignIn,
    load_session: bool,
    /// Sent as soon as a new conversation is ready.
    queued: Option<String>,
    out: Option<UnboundedSender<String>>,
    next_id: u64,
    pending: HashMap<u64, Callback>,
    last_seq: u64,
    retries: u32,
    connection: u64,
}

impl AgentSession {
    pub fn new(
        client: ServerClient,
        instance_id: InstanceId,
        agent_name: String,
        project_name: String,
        session_id: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            client,
            instance_id,
            agent_name,
            project_name,
            session_id,
            phase: Phase::Connecting,
            busy: false,
            thread: Thread::default(),
            sign_in: SignIn::default(),
            load_session: false,
            queued: None,
            out: None,
            next_id: 0,
            pending: HashMap::new(),
            last_seq: 0,
            retries: 0,
            connection: 0,
        };
        this.connect(cx);
        this
    }

    pub fn is_ready(&self) -> bool {
        self.phase == Phase::Ready
    }

    /// The last journal sequence number applied: how far it has been read.
    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// Whether a first message waits for the conversation to be ready.
    pub fn has_queued(&self) -> bool {
        self.queued.is_some()
    }

    /// Sends `prompt` once the conversation is ready (a new one is not yet).
    pub fn queue_prompt(&mut self, prompt: Option<String>) {
        self.queued = prompt.filter(|prompt| !prompt.trim().is_empty());
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        self.connection += 1;
        let connection = self.connection;
        let client = self.client.clone();
        let instance = self.instance_id;
        let this = cx.entity().downgrade();
        runtime::spawn(
            async move { client.acp(instance).await },
            move |result, cx| {
                let _ = this.update(cx, |session, cx| {
                    if session.connection != connection {
                        return;
                    }
                    match result {
                        Ok(socket) => {
                            session.out = Some(socket.outgoing);
                            let lines = this.clone();
                            let closed = this.clone();
                            runtime::receive(
                                socket.incoming,
                                move |line, cx| {
                                    lines
                                        .update(cx, |session, cx| {
                                            if session.connection != connection {
                                                return false;
                                            }
                                            session.on_line(&line, cx);
                                            true
                                        })
                                        .unwrap_or(false)
                                },
                                move |cx| {
                                    let _ = closed.update(cx, |session, cx| {
                                        session.on_closed(connection, cx)
                                    });
                                },
                            );
                            session.initialize(cx);
                        }
                        Err(error) => {
                            session.out = None;
                            session.lost(error.to_string(), cx);
                        }
                    }
                });
            },
        );
    }

    fn initialize(&mut self, cx: &mut Context<Self>) {
        let params = json!({
            "protocolVersion": 1,
            "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
            "clientInfo": { "name": "aiddiction", "version": env!("CARGO_PKG_VERSION") },
        });
        self.request(
            "initialize",
            params,
            cx,
            |session, result, cx| match result {
                Ok(result) => {
                    session.load_session = result
                        .pointer("/agentCapabilities/loadSession")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    session.sign_in.methods = login::methods(&result);
                    session.open(cx);
                }
                Err(error) => session.close(format!("The agent did not start: {error}"), cx),
            },
        );
    }

    /// A new session, or the one this view had: attached, else loaded.
    fn open(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            self.request(
                "session/new",
                json!({ "cwd": ".", "mcpServers": [] }),
                cx,
                |session, result, cx| match result {
                    Ok(result) => {
                        session.session_id = result["sessionId"].as_str().map(str::to_owned);
                        session.thread.set_modes(&result);
                        session.ready(cx);
                    }
                    Err(error) if error.auth_required() => session.needs_sign_in(cx),
                    Err(error) => {
                        session.close(format!("Could not start a conversation: {error}"), cx)
                    }
                },
            );
            return;
        };
        let params = json!({ "sessionId": session_id, "afterSeq": self.last_seq });
        self.request(ext::ATTACH, params, cx, move |session, result, cx| {
            let attach = result
                .ok()
                .and_then(|result| serde_json::from_value::<AttachResult>(result).ok());
            match attach {
                Some(attach) if attach.known => {
                    if attach.truncated {
                        session.thread.clear();
                        session.last_seq = 0;
                    }
                    session.busy = attach.busy;
                    session.ready(cx);
                }
                _ if session.load_session => {
                    session.thread.clear();
                    session.last_seq = 0;
                    let params = json!({ "sessionId": session_id, "cwd": ".", "mcpServers": [] });
                    session.request(
                        "session/load",
                        params,
                        cx,
                        |session, result, cx| match result {
                            Ok(result) => {
                                session.thread.set_modes(&result);
                                session.ready(cx);
                            }
                            Err(error) if error.auth_required() => session.needs_sign_in(cx),
                            Err(error) => session
                                .close(format!("Could not reopen the conversation: {error}"), cx),
                        },
                    );
                }
                _ => session.close("The agent no longer has this conversation.".into(), cx),
            }
        });
    }

    fn ready(&mut self, cx: &mut Context<Self>) {
        self.phase = Phase::Ready;
        self.retries = 0;
        if let Some(prompt) = self.queued.take() {
            self.prompt(prompt, cx);
        }
        cx.notify();
    }

    fn needs_sign_in(&mut self, cx: &mut Context<Self>) {
        self.phase = Phase::SignIn;
        self.sign_in.needed = true;
        cx.notify();
    }

    fn close(&mut self, reason: String, cx: &mut Context<Self>) {
        self.thread.notice(reason.clone(), true);
        self.phase = Phase::Closed(reason);
        self.busy = false;
        self.out = None;
        cx.notify();
    }

    fn on_closed(&mut self, connection: u64, cx: &mut Context<Self>) {
        if self.connection != connection {
            return;
        }
        self.out = None;
        self.lost("The connection was lost".into(), cx);
    }

    /// The socket is gone. Unless the agent itself is, connect again.
    fn lost(&mut self, reason: String, cx: &mut Context<Self>) {
        for (_, callback) in std::mem::take(&mut self.pending) {
            callback(self, Err(RpcError::local(reason.clone())), cx);
        }
        if matches!(self.phase, Phase::Closed(_)) {
            return;
        }
        self.phase = Phase::Reconnecting(reason);
        cx.notify();
        let delay =
            Duration::from_millis(500 * 2u64.pow(self.retries.min(5))).min(Duration::from_secs(15));
        self.retries += 1;
        let client = self.client.clone();
        let instance = self.instance_id;
        let connection = self.connection;
        let this = cx.entity().downgrade();
        runtime::spawn(
            async move {
                tokio::time::sleep(delay).await;
                client.instance(instance).await
            },
            move |result, cx| {
                let _ = this.update(cx, |session, cx| {
                    if session.connection != connection || matches!(session.phase, Phase::Closed(_))
                    {
                        return;
                    }
                    match result {
                        Ok(instance)
                            if matches!(
                                instance.state,
                                InstanceState::Exited | InstanceState::Failed
                            ) =>
                        {
                            let mut reason = instance
                                .message
                                .unwrap_or_else(|| "The agent stopped.".into());
                            if let Some(last) = instance.stderr_tail.last() {
                                reason = format!("{reason}\n{last}");
                            }
                            session.close(reason, cx);
                        }
                        Err(aiddiction_client::Error::Api { status: 404, .. }) => {
                            session.close("The agent is no longer running.".into(), cx);
                        }
                        _ => session.connect(cx),
                    }
                });
            },
        );
    }

    fn send(&mut self, message: Value) -> bool {
        match &self.out {
            Some(out) => out.send(message.to_string()).is_ok(),
            None => false,
        }
    }

    fn request(
        &mut self,
        method: &str,
        params: Value,
        cx: &mut Context<Self>,
        callback: impl FnOnce(&mut Self, Result<Value, RpcError>, &mut Context<Self>) + 'static,
    ) {
        self.next_id += 1;
        let id = self.next_id;
        if self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })) {
            self.pending.insert(id, Box::new(callback));
        } else {
            callback(self, Err(RpcError::local("not connected")), cx);
        }
    }

    fn on_line(&mut self, line: &str, cx: &mut Context<Self>) {
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let method = message["method"].as_str();
        let id = message.get("id").filter(|id| !id.is_null());
        match (method, id) {
            (None, Some(id)) => {
                let Some(callback) = id.as_u64().and_then(|id| self.pending.remove(&id)) else {
                    return;
                };
                let result = match message.get("error") {
                    Some(error) => Err(RpcError {
                        code: error["code"].as_i64().unwrap_or_default(),
                        message: error["message"]
                            .as_str()
                            .unwrap_or("the agent refused")
                            .to_owned(),
                    }),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                callback(self, result, cx);
            }
            (Some("session/request_permission"), Some(id)) => {
                self.thread.add_permission(id.clone(), &message["params"]);
            }
            (Some("elicitation/create"), Some(id)) if message["params"]["mode"] == "url" => {
                let params = &message["params"];
                self.sign_in.page = Some(SignInPage {
                    request_id: id.clone(),
                    elicitation_id: params["elicitationId"].as_str().map(str::to_owned),
                    url: params["url"].as_str().unwrap_or_default().to_owned(),
                    message: params["message"].as_str().unwrap_or_default().to_owned(),
                    opened: false,
                });
            }
            (Some(_), Some(id)) => {
                let id = id.clone();
                self.send(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "not supported by this client" } }));
            }
            (Some(method), None) => self.on_notification(method, &message["params"]),
            (None, None) => {}
        }
        cx.notify();
    }

    fn on_notification(&mut self, method: &str, params: &Value) {
        match method {
            ext::REQUEST_RESOLVED => {
                self.thread.remove_permission(&params["requestId"]);
                return;
            }
            ext::AUTH_STATUS => {
                self.sign_in.needed = login::signed_out(params);
                return;
            }
            "elicitation/complete" => {
                if self.sign_in.page.as_ref().is_some_and(|page| {
                    page.elicitation_id.as_deref() == params["elicitationId"].as_str()
                }) {
                    self.sign_in.page = None;
                }
                return;
            }
            _ => {}
        }
        if params["sessionId"].as_str() != self.session_id.as_deref() {
            return;
        }
        // Something the server replays twice is applied once.
        if let Some(seq) = params
            .pointer(&format!("/_meta/{}", ext::SEQ_META))
            .and_then(Value::as_u64)
        {
            if seq <= self.last_seq {
                return;
            }
            self.last_seq = seq;
        }
        match method {
            "session/update" => self.thread.apply(&params["update"]),
            ext::PROMPT_FINISHED => {
                self.busy = false;
                self.thread.end_turn();
                if let Some(error) = params["error"].as_str() {
                    self.thread.notice(error, true);
                } else if params["stopReason"] == "cancelled" {
                    self.thread.notice("Stopped", false);
                }
            }
            _ => {}
        }
    }

    pub fn prompt(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        if !self.is_ready() || self.busy || text.trim().is_empty() {
            return;
        }
        self.busy = true;
        let params =
            json!({ "sessionId": session_id, "prompt": [{ "type": "text", "text": text }] });
        self.request("session/prompt", params, cx, |session, result, cx| {
            session.busy = false;
            match result {
                Err(error) if error.auth_required() => {
                    session.sign_in.needed = true;
                    session.sign_in.refused = true;
                }
                // A lost socket is not the turn failing: the replay says how it ended.
                Err(error) if session.is_ready() => session.thread.notice(error.message, true),
                _ => {}
            }
            cx.notify();
        });
        cx.notify();
    }

    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        self.send(json!({ "jsonrpc": "2.0", "method": "session/cancel", "params": { "sessionId": session_id } }));
        // Questions the agent is waiting on are answered "cancelled", as the
        // protocol asks of a client that cancels.
        for permission in std::mem::take(&mut self.thread.permissions) {
            self.send(json!({ "jsonrpc": "2.0", "id": permission.request_id, "result": { "outcome": { "outcome": "cancelled" } } }));
        }
        cx.notify();
    }

    pub fn answer_permission(
        &mut self,
        request_id: Value,
        option_id: String,
        cx: &mut Context<Self>,
    ) {
        let outcome = json!({ "outcome": "selected", "optionId": option_id });
        self.send(json!({ "jsonrpc": "2.0", "id": request_id, "result": { "outcome": outcome } }));
        self.thread.remove_permission(&request_id);
        cx.notify();
    }

    pub fn set_mode(&mut self, mode_id: String, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let params = json!({ "sessionId": session_id, "modeId": mode_id });
        let chosen = mode_id.clone();
        self.request(
            "session/set_mode",
            params,
            cx,
            move |session, result, cx| {
                match result {
                    Ok(_) => session.thread.current_mode = Some(chosen),
                    Err(error) => session
                        .thread
                        .notice(format!("Could not change the mode: {error}"), true),
                }
                cx.notify();
            },
        );
    }

    pub fn set_config_option(&mut self, config_id: String, value: String, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let params =
            json!({ "sessionId": session_id, "configId": config_id, "type": "id", "value": value });
        self.request(
            "session/set_config_option",
            params,
            cx,
            |session, result, cx| {
                match result {
                    Ok(result) => session.thread.set_config_options(&result["configOptions"]),
                    Err(error) => session
                        .thread
                        .notice(format!("Could not change the setting: {error}"), true),
                }
                cx.notify();
            },
        );
    }

    /// Starts signing the agent in one of the ways it offers. An API key is
    /// sent by [`Self::submit_key`].
    pub fn sign_in_with(&mut self, method: AuthMethod, cx: &mut Context<Self>) {
        self.sign_in.error = None;
        self.sign_in.terminal = None;
        self.sign_in.method = Some(method.clone());
        match method.kind {
            MethodKind::ApiKey => {}
            MethodKind::Agent => self.authenticate(json!({ "methodId": method.id }), cx),
            MethodKind::Terminal => self.start_terminal(method.id, cx),
        }
        cx.notify();
    }

    pub fn submit_key(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(method) = self.sign_in.method.clone() else {
            return;
        };
        if key.trim().is_empty() || self.sign_in.busy {
            return;
        }
        self.authenticate(
            json!({ "methodId": method.id, "_meta": { "api-key": { "apiKey": key.trim() } } }),
            cx,
        );
    }

    fn authenticate(&mut self, params: Value, cx: &mut Context<Self>) {
        self.sign_in.busy = true;
        self.request("authenticate", params, cx, |session, result, cx| {
            session.sign_in.busy = false;
            session.sign_in.page = None;
            // Cancelled by the person: nothing to say.
            let Some(method) = session.sign_in.method.clone() else {
                return;
            };
            match result {
                Ok(_) => session.signed_in(cx),
                Err(error) => {
                    session.sign_in.error = Some(error.message);
                    if method.kind != MethodKind::ApiKey {
                        session.sign_in.method = None;
                    }
                }
            }
            cx.notify();
        });
    }

    fn start_terminal(&mut self, method_id: String, cx: &mut Context<Self>) {
        self.sign_in.busy = true;
        let client = self.client.clone();
        let instance = self.instance_id;
        let this = cx.entity().downgrade();
        runtime::spawn(
            async move { client.start_login(instance, &method_id).await },
            move |result, cx| {
                let _ = this.update(cx, |session, cx| {
                    session.sign_in.busy = false;
                    session.terminal_read(result, cx);
                });
            },
        );
    }

    /// What the terminal sign-in says now: read again while it runs.
    fn terminal_read(&mut self, result: aiddiction_client::Result<Login>, cx: &mut Context<Self>) {
        if self
            .sign_in
            .method
            .as_ref()
            .is_none_or(|method| method.kind != MethodKind::Terminal)
        {
            return;
        }
        match result {
            Ok(login) if login.running => {
                self.sign_in.terminal = Some(login);
                self.sign_in.polling += 1;
                let polling = self.sign_in.polling;
                let client = self.client.clone();
                let instance = self.instance_id;
                let this = cx.entity().downgrade();
                runtime::spawn(
                    async move {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        client.login(instance).await
                    },
                    move |result, cx| {
                        let _ = this.update(cx, |session, cx| {
                            if session.sign_in.polling == polling {
                                session.terminal_read(result, cx);
                            }
                        });
                    },
                );
            }
            Ok(login) if login.success == Some(true) => self.signed_in(cx),
            Ok(login) => {
                let last = login
                    .output
                    .lines()
                    .rev()
                    .find(|line| !line.trim().is_empty())
                    .map(str::trim)
                    .map(str::to_owned);
                self.sign_in.error =
                    Some(last.unwrap_or_else(|| "The sign-in did not succeed.".into()));
                self.sign_in.method = None;
                self.sign_in.terminal = None;
            }
            Err(error) => {
                self.sign_in.error = Some(error.to_string());
                self.sign_in.method = None;
                self.sign_in.terminal = None;
            }
        }
        cx.notify();
    }

    /// A line for the terminal sign-in: the code its page gave.
    pub fn type_into_sign_in(&mut self, text: &str, cx: &mut Context<Self>) {
        let text = text.trim().to_owned();
        if text.is_empty() {
            return;
        }
        let client = self.client.clone();
        let instance = self.instance_id;
        let this = cx.entity().downgrade();
        runtime::spawn(
            async move { client.login_input(instance, &text).await },
            move |result, cx| {
                if let Err(error) = result {
                    let _ = this.update(cx, |session, cx| {
                        session.sign_in.error = Some(error.to_string());
                        cx.notify();
                    });
                }
            },
        );
    }

    /// The page the agent asks to open, told that the person went there.
    pub fn open_sign_in_page(&mut self) -> Option<String> {
        let page = self.sign_in.page.as_mut()?;
        let url = page.url.clone();
        if !page.opened {
            page.opened = true;
            let id = page.request_id.clone();
            self.send(json!({ "jsonrpc": "2.0", "id": id, "result": { "action": "accept" } }));
        }
        Some(url)
    }

    pub fn cancel_sign_in(&mut self, cx: &mut Context<Self>) {
        let Some(method) = self.sign_in.method.take() else {
            return;
        };
        self.sign_in.busy = false;
        self.sign_in.terminal = None;
        self.sign_in.error = None;
        if let Some(page) = self.sign_in.page.take() {
            self.send(json!({ "jsonrpc": "2.0", "id": page.request_id, "result": { "action": "cancel" } }));
        }
        if method.kind == MethodKind::Terminal {
            let client = self.client.clone();
            let instance = self.instance_id;
            runtime::spawn(async move { client.stop_login(instance).await }, |_, _| {});
        }
        cx.notify();
    }

    fn signed_in(&mut self, cx: &mut Context<Self>) {
        let refused = self.sign_in.refused;
        self.sign_in = SignIn {
            methods: std::mem::take(&mut self.sign_in.methods),
            ..SignIn::default()
        };
        if self.phase == Phase::SignIn {
            self.phase = Phase::Connecting;
            self.open(cx);
        } else if refused {
            self.thread.notice(
                format!("Signed in to {}. Send the message again.", self.agent_name),
                false,
            );
        }
        cx.notify();
    }

    /// Connects again now, after the session closed or while it waits.
    pub fn retry(&mut self, cx: &mut Context<Self>) {
        self.phase = Phase::Connecting;
        self.retries = 0;
        cx.notify();
        self.connect(cx);
    }
}
