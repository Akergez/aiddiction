//! One conversation with an agent instance on the server: an ACP client over
//! the server's socket, a JSON-RPC layer of its own, and the [`Thread`] the
//! chat draws.
//!
//! Losing the socket is ordinary (a phone that slept): the session connects
//! again and attaches with the last sequence number it applied, and the
//! server replays what it missed.

use std::collections::HashMap;
use std::time::Duration;

use aiddiction_api::{AttachResult, InstanceId, InstanceState, ext};
use aiddiction_client::ServerClient;
use gpui_kit::Context;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::runtime;
use crate::thread::Thread;

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Connecting,
    Ready,
    Reconnecting(String),
    Closed(String),
}

type Callback = Box<dyn FnOnce(&mut AgentSession, Result<Value, String>, &mut Context<AgentSession>)>;

pub struct AgentSession {
    client: ServerClient,
    pub instance_id: InstanceId,
    pub agent_name: String,
    pub project_name: String,
    pub session_id: Option<String>,
    pub phase: Phase,
    pub busy: bool,
    pub thread: Thread,
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
        runtime::spawn(async move { client.acp(instance).await }, move |result, cx| {
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
                                let _ = closed.update(cx, |session, cx| session.on_closed(connection, cx));
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
        });
    }

    fn initialize(&mut self, cx: &mut Context<Self>) {
        let params = json!({
            "protocolVersion": 1,
            "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
            "clientInfo": { "name": "aiddiction", "version": env!("CARGO_PKG_VERSION") },
        });
        self.request("initialize", params, cx, |session, result, cx| match result {
            Ok(result) => {
                session.load_session = result.pointer("/agentCapabilities/loadSession").and_then(Value::as_bool).unwrap_or(false);
                session.open(cx);
            }
            Err(error) => session.close(format!("The agent did not start: {error}"), cx),
        });
    }

    /// A new session, or the one this view had: attached, else loaded.
    fn open(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            self.request("session/new", json!({ "cwd": ".", "mcpServers": [] }), cx, |session, result, cx| {
                match result {
                    Ok(result) => {
                        session.session_id = result["sessionId"].as_str().map(str::to_owned);
                        session.thread.set_modes(&result);
                        session.ready(cx);
                    }
                    Err(error) => session.close(format!("Could not start a conversation: {error}"), cx),
                }
            });
            return;
        };
        let params = json!({ "sessionId": session_id, "afterSeq": self.last_seq });
        self.request(ext::ATTACH, params, cx, move |session, result, cx| {
            let attach = result.ok().and_then(|result| serde_json::from_value::<AttachResult>(result).ok());
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
                    session.request("session/load", params, cx, |session, result, cx| match result {
                        Ok(result) => {
                            session.thread.set_modes(&result);
                            session.ready(cx);
                        }
                        Err(error) => session.close(format!("Could not reopen the conversation: {error}"), cx),
                    });
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
            callback(self, Err(reason.clone()), cx);
        }
        if matches!(self.phase, Phase::Closed(_)) {
            return;
        }
        self.phase = Phase::Reconnecting(reason);
        cx.notify();
        let delay = Duration::from_millis(500 * 2u64.pow(self.retries.min(5))).min(Duration::from_secs(15));
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
                    if session.connection != connection || matches!(session.phase, Phase::Closed(_)) {
                        return;
                    }
                    match result {
                        Ok(instance) if matches!(instance.state, InstanceState::Exited | InstanceState::Failed) => {
                            let mut reason = instance.message.unwrap_or_else(|| "The agent stopped.".into());
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
        callback: impl FnOnce(&mut Self, Result<Value, String>, &mut Context<Self>) + 'static,
    ) {
        self.next_id += 1;
        let id = self.next_id;
        if self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })) {
            self.pending.insert(id, Box::new(callback));
        } else {
            callback(self, Err("not connected".into()), cx);
        }
    }

    fn on_line(&mut self, line: &str, cx: &mut Context<Self>) {
        let Ok(message) = serde_json::from_str::<Value>(line) else { return };
        let method = message["method"].as_str();
        let id = message.get("id").filter(|id| !id.is_null());
        match (method, id) {
            (None, Some(id)) => {
                let Some(callback) = id.as_u64().and_then(|id| self.pending.remove(&id)) else { return };
                let result = match message.get("error") {
                    Some(error) => Err(error["message"].as_str().unwrap_or("the agent refused").to_owned()),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                callback(self, result, cx);
            }
            (Some("session/request_permission"), Some(id)) => {
                self.thread.add_permission(id.clone(), &message["params"]);
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
        if method == ext::REQUEST_RESOLVED {
            self.thread.remove_permission(&params["requestId"]);
            return;
        }
        if params["sessionId"].as_str() != self.session_id.as_deref() {
            return;
        }
        // Something the server replays twice is applied once.
        if let Some(seq) = params.pointer(&format!("/_meta/{}", ext::SEQ_META)).and_then(Value::as_u64) {
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
        let Some(session_id) = self.session_id.clone() else { return };
        if !self.is_ready() || self.busy || text.trim().is_empty() {
            return;
        }
        self.busy = true;
        let params = json!({ "sessionId": session_id, "prompt": [{ "type": "text", "text": text }] });
        self.request("session/prompt", params, cx, |session, result, cx| {
            session.busy = false;
            if let Err(error) = result {
                // A lost socket is not the turn failing: the replay says how it ended.
                if session.is_ready() {
                    session.thread.notice(error, true);
                }
            }
            cx.notify();
        });
        cx.notify();
    }

    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else { return };
        self.send(json!({ "jsonrpc": "2.0", "method": "session/cancel", "params": { "sessionId": session_id } }));
        // Questions the agent is waiting on are answered "cancelled", as the
        // protocol asks of a client that cancels.
        for permission in std::mem::take(&mut self.thread.permissions) {
            self.send(json!({ "jsonrpc": "2.0", "id": permission.request_id, "result": { "outcome": { "outcome": "cancelled" } } }));
        }
        cx.notify();
    }

    pub fn answer_permission(&mut self, request_id: Value, option_id: String, cx: &mut Context<Self>) {
        let outcome = json!({ "outcome": "selected", "optionId": option_id });
        self.send(json!({ "jsonrpc": "2.0", "id": request_id, "result": { "outcome": outcome } }));
        self.thread.remove_permission(&request_id);
        cx.notify();
    }

    pub fn set_mode(&mut self, mode_id: String, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else { return };
        let params = json!({ "sessionId": session_id, "modeId": mode_id });
        let chosen = mode_id.clone();
        self.request("session/set_mode", params, cx, move |session, result, cx| {
            match result {
                Ok(_) => session.thread.current_mode = Some(chosen),
                Err(error) => session.thread.notice(format!("Could not change the mode: {error}"), true),
            }
            cx.notify();
        });
    }

    /// Connects again now, after the session closed or while it waits.
    pub fn retry(&mut self, cx: &mut Context<Self>) {
        self.phase = Phase::Connecting;
        self.retries = 0;
        cx.notify();
        self.connect(cx);
    }
}
