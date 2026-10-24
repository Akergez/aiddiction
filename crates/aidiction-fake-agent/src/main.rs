//! A scripted ACP agent over stdio, for tests and the demo.
//!
//! It streams a thought and an answer to every prompt. A prompt that says
//! "file" also runs a tool call: it asks for permission, writes `hello.txt`
//! through the client's `fs/write_text_file`, and reports a diff. A prompt
//! that says "plan" publishes a plan; "crash" ends the process.

use std::collections::VecDeque;
use std::io::{BufRead as _, Write as _};
use std::time::Duration;

use serde_json::{Value, json};

struct Agent {
    input: std::io::Lines<std::io::StdinLock<'static>>,
    /// Messages read while waiting for a particular response.
    queued: VecDeque<Value>,
    next_id: u64,
    sessions: u64,
    cwd: String,
    delay: Duration,
}

fn main() {
    let delay = std::env::var("FAKE_AGENT_DELAY_MS").ok().and_then(|ms| ms.parse().ok()).unwrap_or(40);
    let mut agent = Agent {
        input: std::io::stdin().lock().lines(),
        queued: VecDeque::new(),
        next_id: 0,
        sessions: 0,
        cwd: String::new(),
        delay: Duration::from_millis(delay),
    };
    while let Some(message) = agent.next_message() {
        agent.handle(message);
    }
}

fn send(message: Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{message}");
    let _ = out.flush();
}

fn update(session: &str, update: Value) {
    send(json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": session, "update": update } }));
}

fn text_chunk(kind: &str, text: &str) -> Value {
    json!({ "sessionUpdate": kind, "content": { "type": "text", "text": text } })
}

impl Agent {
    fn next_message(&mut self) -> Option<Value> {
        if let Some(message) = self.queued.pop_front() {
            return Some(message);
        }
        loop {
            let line = self.input.next()?.ok()?;
            if let Ok(message) = serde_json::from_str(&line) {
                return Some(message);
            }
        }
    }

    /// Sends a request to the client and waits for its answer, keeping
    /// whatever else arrives meanwhile.
    fn request(&mut self, method: &str, params: Value) -> Option<Value> {
        self.next_id += 1;
        let id = format!("fake-{}", self.next_id);
        send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let line = self.input.next()?.ok()?;
            let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
            if message.get("method").is_none() && message["id"] == id {
                return Some(message);
            }
            self.queued.push_back(message);
        }
    }

    fn cancelled(&mut self) -> bool {
        self.queued.iter().any(|message| message["method"] == "session/cancel")
    }

    fn handle(&mut self, message: Value) {
        let id = message["id"].clone();
        let reply = |result: Value| send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
        match message["method"].as_str() {
            Some("initialize") => reply(json!({
                "protocolVersion": 1,
                "agentCapabilities": { "loadSession": false, "promptCapabilities": {} },
                "agentInfo": { "name": "fake-agent", "version": env!("CARGO_PKG_VERSION") },
                "authMethods": [],
            })),
            Some("session/new") => {
                self.sessions += 1;
                self.cwd = message["params"]["cwd"].as_str().unwrap_or_default().to_owned();
                let session = format!("fake-session-{}-{}", std::process::id(), self.sessions);
                reply(json!({
                    "sessionId": session,
                    "modes": {
                        "currentModeId": "default",
                        "availableModes": [
                            { "id": "default", "name": "Ask first" },
                            { "id": "auto", "name": "Auto" },
                        ],
                    },
                }));
                update(&session, json!({
                    "sessionUpdate": "available_commands_update",
                    "availableCommands": [
                        { "name": "file", "description": "Write hello.txt" },
                        { "name": "plan", "description": "Show a plan" },
                    ],
                }));
            }
            Some("session/set_mode") => {
                let session = message["params"]["sessionId"].as_str().unwrap_or_default().to_owned();
                reply(json!({}));
                update(&session, json!({ "sessionUpdate": "current_mode_update", "currentModeId": message["params"]["modeId"] }));
            }
            Some("session/prompt") => {
                let stop = self.prompt(&message["params"]);
                send(json!({ "jsonrpc": "2.0", "id": id, "result": { "stopReason": stop } }));
            }
            Some("session/cancel") => {}
            Some(_) if !id.is_null() => send(json!({
                "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "method not found" },
            })),
            _ => {}
        }
    }

    fn prompt(&mut self, params: &Value) -> &'static str {
        let session = params["sessionId"].as_str().unwrap_or_default().to_owned();
        let text: String = params["prompt"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|block| block["text"].as_str())
            .collect();
        let lower = text.to_lowercase();
        if lower.contains("crash") {
            std::process::exit(3);
        }

        update(&session, text_chunk("agent_thought_chunk", "Reading the request…"));
        if lower.contains("plan") || lower.contains("file") {
            update(&session, json!({
                "sessionUpdate": "plan",
                "entries": [
                    { "content": "Understand the request", "priority": "high", "status": "completed" },
                    { "content": "Write hello.txt", "priority": "medium", "status": "in_progress" },
                    { "content": "Report back", "priority": "low", "status": "pending" },
                ],
            }));
        }

        let answer = format!("You said: **{}**. ", text.trim());
        for word in answer.split_inclusive(' ') {
            if self.cancelled() {
                return "cancelled";
            }
            update(&session, text_chunk("agent_message_chunk", word));
            std::thread::sleep(self.delay);
        }

        if lower.contains("file") {
            let path = format!("{}/hello.txt", self.cwd.trim_end_matches('/'));
            let call = "call-1";
            update(&session, json!({
                "sessionUpdate": "tool_call",
                "toolCallId": call,
                "title": "Write hello.txt",
                "kind": "edit",
                "status": "pending",
                "locations": [{ "path": path }],
            }));
            let answer = self.request("session/request_permission", json!({
                "sessionId": session,
                "toolCall": { "toolCallId": call },
                "options": [
                    { "optionId": "allow", "name": "Allow", "kind": "allow_once" },
                    { "optionId": "reject", "name": "Reject", "kind": "reject_once" },
                ],
            }));
            let allowed = answer
                .as_ref()
                .and_then(|answer| answer.pointer("/result/outcome/optionId"))
                .is_some_and(|option| option == "allow");
            if !allowed {
                update(&session, json!({ "sessionUpdate": "tool_call_update", "toolCallId": call, "status": "failed" }));
                update(&session, text_chunk("agent_message_chunk", "\n\nAll right, I left the file alone."));
                return "end_turn";
            }
            update(&session, json!({ "sessionUpdate": "tool_call_update", "toolCallId": call, "status": "in_progress" }));
            let content = "Hello from the fake agent!\n";
            let written = self.request("fs/write_text_file", json!({ "sessionId": session, "path": path, "content": content }));
            let ok = written.as_ref().is_some_and(|written| written.get("error").is_none());
            update(&session, json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": call,
                "status": if ok { "completed" } else { "failed" },
                "content": [{ "type": "diff", "path": path, "oldText": null, "newText": content }],
            }));
            update(&session, text_chunk("agent_message_chunk", if ok { "\n\nDone: `hello.txt` is written." } else { "\n\nThe write failed." }));
        }
        "end_turn"
    }
}
