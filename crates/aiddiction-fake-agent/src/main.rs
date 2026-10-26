//! A scripted ACP agent over stdio, for tests and the demo.
//!
//! It streams a thought and an answer to every prompt. A prompt that says
//! "file" also runs a tool call: it asks for permission, writes `hello.txt`
//! through the client's `fs/write_text_file`, and reports a diff. A prompt
//! that says "plan" publishes a plan; "crash" ends the process.
//!
//! With `FAKE_AGENT_SIGN_IN` naming a file, it is signed in while that file
//! exists and refuses new sessions until then. It signs in two ways: in a
//! terminal (run with `--sign-in`, it shows an address and reads a code), or
//! by `authenticate`, which shows a page through `elicitation/create`.

use std::collections::VecDeque;
use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;
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
    sign_in: Option<PathBuf>,
}

fn main() {
    let sign_in = std::env::var_os("FAKE_AGENT_SIGN_IN").map(PathBuf::from);
    if std::env::args().any(|arg| arg == "--sign-in") {
        sign_in_in_a_terminal(sign_in);
    }
    let delay = std::env::var("FAKE_AGENT_DELAY_MS").ok().and_then(|ms| ms.parse().ok()).unwrap_or(40);
    let mut agent = Agent {
        sign_in,
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

/// What a terminal sign-in does: an address to visit, a code to paste.
fn sign_in_in_a_terminal(file: Option<PathBuf>) -> ! {
    print!("Visit https://example.com/fake-sign-in and paste the code here > ");
    let _ = std::io::stdout().flush();
    let mut code = String::new();
    let _ = std::io::stdin().read_line(&mut code);
    if code.trim().is_empty() {
        println!("\nNo code.");
        std::process::exit(1);
    }
    if let Some(file) = file {
        let _ = std::fs::write(file, code.trim());
    }
    println!("\nSigned in.");
    std::process::exit(0);
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
    fn signed_in(&self) -> bool {
        self.sign_in.as_ref().is_none_or(|file| file.exists())
    }

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
            Some("initialize") => {
                let methods = if self.sign_in.is_some() {
                    json!([
                        { "id": "terminal", "name": "In a terminal", "type": "terminal", "args": ["--sign-in"] },
                        { "id": "page", "name": "With a page" },
                    ])
                } else {
                    json!([])
                };
                reply(json!({
                    "protocolVersion": 1,
                    "agentCapabilities": { "loadSession": false, "promptCapabilities": {} },
                    "agentInfo": { "name": "fake-agent", "version": env!("CARGO_PKG_VERSION") },
                    "authMethods": methods,
                }));
                if self.sign_in.is_some() {
                    let kind = if self.signed_in() { "subscription" } else { "none" };
                    send(json!({ "jsonrpc": "2.0", "method": "_auth/status_update", "params": { "authStatus": { "kind": kind } } }));
                }
            }
            Some("authenticate") => {
                let page = self.request("elicitation/create", json!({
                    "mode": "url",
                    "requestId": id,
                    "url": "https://example.com/fake-device",
                    "message": "Open the page and enter FAKE-CODE",
                    "elicitationId": "fake-elicitation",
                }));
                let accepted = page.as_ref().and_then(|page| page.pointer("/result/action")).is_some_and(|action| action == "accept");
                if !accepted {
                    send(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32800, "message": "sign-in cancelled" } }));
                    return;
                }
                if let Some(file) = &self.sign_in {
                    let _ = std::fs::write(file, "page");
                }
                send(json!({ "jsonrpc": "2.0", "method": "elicitation/complete", "params": { "elicitationId": "fake-elicitation" } }));
                reply(json!({}));
            }
            Some("session/new") if !self.signed_in() => send(json!({
                "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": "Authentication required" },
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
        if lower.contains("plan") || lower.contains("make a file") {
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

        if lower.contains("make a file") {
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
