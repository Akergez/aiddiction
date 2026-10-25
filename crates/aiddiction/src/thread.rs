//! A conversation with an agent as the interface shows it, built from ACP
//! `session/update` notifications. A much smaller cousin of Zed's
//! `acp_thread::AcpThread`: messages, thoughts, tool calls, the plan, the
//! commands and modes the agent offers, and its questions to the person.
//!
//! Updates are read as JSON rather than as the schema crate's types, so an
//! update kind this client does not know yet is passed over instead of
//! failing the whole message.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    User { text: String },
    Assistant { text: String, thought: String },
    Tool(ToolCall),
    /// Something the client says about the conversation (an error, a stop).
    Notice { text: String, error: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub status: ToolStatus,
    pub content: Vec<ToolContent>,
    pub locations: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl ToolStatus {
    fn parse(status: &str) -> Self {
        match status {
            "in_progress" => Self::InProgress,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolContent {
    Text(String),
    Diff { path: String, old: Option<String>, new: String },
    Terminal(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlanEntry {
    pub content: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Mode {
    pub id: String,
    pub name: String,
}

/// The agent waiting on a person: `session/request_permission`.
#[derive(Debug, Clone, PartialEq)]
pub struct Permission {
    /// The JSON-RPC id to answer.
    pub request_id: Value,
    pub title: String,
    pub options: Vec<PermissionOption>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PermissionOption {
    pub id: String,
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Default)]
pub struct Thread {
    pub entries: Vec<Entry>,
    pub plan: Vec<PlanEntry>,
    pub commands: Vec<Command>,
    pub modes: Vec<Mode>,
    pub current_mode: Option<String>,
    pub permissions: Vec<Permission>,
    /// A turn ended: the next user chunk is a new message.
    closed_user: bool,
    /// The lowest entry index changed since [`Thread::take_dirty`].
    dirty_from: Option<usize>,
}

impl Thread {
    /// Applies one `session/update` notification's `update` object.
    pub fn apply(&mut self, update: &Value) {
        let text = || update.pointer("/content/text").and_then(Value::as_str).unwrap_or_default();
        match update["sessionUpdate"].as_str().unwrap_or_default() {
            "user_message_chunk" => {
                let chunk = content_text(&update["content"]);
                let open = !self.closed_user;
                match self.entries.last_mut() {
                    // Chunks of one message arrive one after another.
                    Some(Entry::User { text }) if open => text.push_str(&chunk),
                    _ => self.push(Entry::User { text: chunk }),
                }
                self.closed_user = false;
                self.touch_last();
            }
            "agent_message_chunk" => {
                let chunk = content_text(&update["content"]);
                self.assistant().0.push_str(&chunk);
            }
            "agent_thought_chunk" => {
                let chunk = text().to_owned();
                self.assistant().1.push_str(&chunk);
            }
            "tool_call" => {
                let call = ToolCall {
                    id: str_of(update, "toolCallId"),
                    title: str_of(update, "title"),
                    kind: str_of(update, "kind"),
                    status: ToolStatus::parse(update["status"].as_str().unwrap_or("pending")),
                    content: tool_content(&update["content"]),
                    locations: locations(&update["locations"]),
                };
                match self.tool_index(&call.id) {
                    Some(index) => {
                        self.entries[index] = Entry::Tool(call);
                        self.touch(index);
                    }
                    None => self.push(Entry::Tool(call)),
                }
            }
            "tool_call_update" => {
                let id = str_of(update, "toolCallId");
                let Some(index) = self.tool_index(&id) else { return };
                if let Entry::Tool(call) = &mut self.entries[index] {
                    if let Some(status) = update["status"].as_str() {
                        call.status = ToolStatus::parse(status);
                    }
                    if let Some(title) = update["title"].as_str() {
                        call.title = title.to_owned();
                    }
                    if update.get("content").is_some_and(|content| !content.is_null()) {
                        call.content = tool_content(&update["content"]);
                    }
                    if update.get("locations").is_some_and(|locations| !locations.is_null()) {
                        call.locations = locations(&update["locations"]);
                    }
                }
                self.touch(index);
            }
            "plan" => {
                self.plan = update["entries"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|entry| PlanEntry { content: str_of(entry, "content"), status: str_of(entry, "status") })
                    .collect();
            }
            "available_commands_update" => {
                self.commands = update["availableCommands"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|command| Command { name: str_of(command, "name"), description: str_of(command, "description") })
                    .collect();
            }
            "current_mode_update" => {
                self.current_mode = update["currentModeId"].as_str().map(str::to_owned);
            }
            _ => {}
        }
    }

    /// Reads the modes a `session/new` or `session/load` answer offers.
    pub fn set_modes(&mut self, result: &Value) {
        let modes = &result["modes"];
        self.modes = modes["availableModes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|mode| Mode { id: str_of(mode, "id"), name: str_of(mode, "name") })
            .collect();
        self.current_mode = modes["currentModeId"].as_str().map(str::to_owned);
    }

    /// The next user chunk starts a new message even if the last entry is one:
    /// called when a turn ends.
    pub fn end_turn(&mut self) {
        self.closed_user = true;
    }

    pub fn notice(&mut self, text: impl Into<String>, error: bool) {
        self.push(Entry::Notice { text: text.into(), error });
    }

    pub fn add_permission(&mut self, request_id: Value, params: &Value) {
        let title = params
            .pointer("/toolCall/title")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                let id = params.pointer("/toolCall/toolCallId").and_then(Value::as_str)?;
                self.tool_index(id).and_then(|index| match &self.entries[index] {
                    Entry::Tool(call) => Some(call.title.clone()),
                    _ => None,
                })
            })
            .unwrap_or_else(|| "The agent asks for permission".into());
        let options = params["options"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|option| PermissionOption {
                id: str_of(option, "optionId"),
                name: str_of(option, "name"),
                kind: str_of(option, "kind"),
            })
            .collect();
        self.permissions.retain(|permission| permission.request_id != request_id);
        self.permissions.push(Permission { request_id, title, options });
    }

    pub fn remove_permission(&mut self, request_id: &Value) {
        self.permissions.retain(|permission| &permission.request_id != request_id);
    }

    /// Forgets everything, for a session that is replayed from its start.
    pub fn clear(&mut self) {
        *self = Thread { dirty_from: Some(0), ..Thread::default() };
    }

    /// The lowest index changed since the last call, if any.
    pub fn take_dirty(&mut self) -> Option<usize> {
        self.dirty_from.take()
    }

    fn assistant(&mut self) -> (&mut String, &mut String) {
        if !matches!(self.entries.last(), Some(Entry::Assistant { .. })) {
            self.push(Entry::Assistant { text: String::new(), thought: String::new() });
        }
        self.touch_last();
        match self.entries.last_mut() {
            Some(Entry::Assistant { text, thought }) => (text, thought),
            _ => unreachable!(),
        }
    }

    fn tool_index(&self, id: &str) -> Option<usize> {
        self.entries.iter().rposition(|entry| matches!(entry, Entry::Tool(call) if call.id == id))
    }

    fn push(&mut self, entry: Entry) {
        self.entries.push(entry);
        self.touch_last();
    }

    fn touch_last(&mut self) {
        if let Some(last) = self.entries.len().checked_sub(1) {
            self.touch(last);
        }
    }

    fn touch(&mut self, index: usize) {
        self.dirty_from = Some(self.dirty_from.map_or(index, |from| from.min(index)));
    }
}

/// What a run of tool calls in a row adds up to, for its one line in the
/// conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolRun {
    pub status: ToolStatus,
    /// "Write hello.txt" while one runs; "3 steps · edited a.rs, b.rs" after.
    pub text: String,
}

impl Thread {
    /// Where the run of tool calls `index` belongs to starts; `index` itself
    /// when it is not a tool call.
    pub fn run_start(&self, index: usize) -> usize {
        let mut start = index;
        while start > 0
            && matches!(self.entries.get(start), Some(Entry::Tool(_)))
            && matches!(self.entries.get(start - 1), Some(Entry::Tool(_)))
        {
            start -= 1;
        }
        start
    }

    /// The tool calls of the run starting at `start`.
    pub fn run(&self, start: usize) -> Vec<&ToolCall> {
        self.entries[start.min(self.entries.len())..]
            .iter()
            .map_while(|entry| match entry {
                Entry::Tool(call) => Some(call),
                _ => None,
            })
            .collect()
    }
}

/// Sums a run of tool calls up: one still going is named, a finished run is
/// counted and its edited files listed.
pub fn summarize(run: &[&ToolCall]) -> ToolRun {
    let status = if run.iter().any(|call| call.status == ToolStatus::Failed) {
        ToolStatus::Failed
    } else if let Some(call) = run.iter().rev().find(|call| matches!(call.status, ToolStatus::InProgress | ToolStatus::Pending)) {
        let text = if call.title.is_empty() { call.kind.clone() } else { call.title.clone() };
        return ToolRun { status: call.status, text };
    } else {
        ToolStatus::Completed
    };
    let mut parts = vec![if run.len() == 1 { "1 step".to_owned() } else { format!("{} steps", run.len()) }];
    let mut edited: Vec<&str> = Vec::new();
    for call in run.iter().filter(|call| matches!(call.kind.as_str(), "edit" | "delete" | "move")) {
        for path in &call.locations {
            let name = path.rsplit('/').next().unwrap_or(path);
            if !edited.contains(&name) {
                edited.push(name);
            }
        }
    }
    if !edited.is_empty() {
        let shown = edited.iter().take(3).copied().collect::<Vec<_>>().join(", ");
        let more = edited.len().saturating_sub(3);
        parts.push(if more > 0 { format!("edited {shown} and {more} more") } else { format!("edited {shown}") });
    }
    let commands = run.iter().filter(|call| call.kind == "execute").count();
    if commands > 0 {
        parts.push(if commands == 1 { "ran a command".to_owned() } else { format!("ran {commands} commands") });
    }
    if status == ToolStatus::Failed {
        parts.push("one failed".to_owned());
    }
    ToolRun { status, text: parts.join(" · ") }
}

fn str_of(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_owned()
}

/// The text of a content block; other kinds are named, not shown.
fn content_text(content: &Value) -> String {
    match content["type"].as_str() {
        Some("text") => str_of(content, "text"),
        Some("resource_link") => format!("[{}]", content["name"].as_str().unwrap_or("file")),
        Some("image") => "[image]".into(),
        Some("resource") => content
            .pointer("/resource/text")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| "[resource]".into()),
        _ => String::new(),
    }
}

fn tool_content(content: &Value) -> Vec<ToolContent> {
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| match item["type"].as_str()? {
            "content" => Some(ToolContent::Text(content_text(&item["content"]))),
            "diff" => Some(ToolContent::Diff {
                path: str_of(item, "path"),
                old: item["oldText"].as_str().map(str::to_owned),
                new: str_of(item, "newText"),
            }),
            "terminal" => Some(ToolContent::Terminal(str_of(item, "terminalId"))),
            _ => None,
        })
        .collect()
}

fn locations(locations: &Value) -> Vec<String> {
    locations
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|location| location["path"].as_str().map(str::to_owned))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chunk(kind: &str, text: &str) -> Value {
        json!({ "sessionUpdate": kind, "content": { "type": "text", "text": text } })
    }

    #[test]
    fn chunks_join_into_one_message_per_turn() {
        let mut thread = Thread::default();
        thread.apply(&chunk("user_message_chunk", "hi"));
        thread.apply(&chunk("agent_thought_chunk", "hmm"));
        thread.apply(&chunk("agent_message_chunk", "hel"));
        thread.apply(&chunk("agent_message_chunk", "lo"));
        thread.end_turn();
        thread.apply(&chunk("user_message_chunk", "again"));
        assert_eq!(
            thread.entries,
            vec![
                Entry::User { text: "hi".into() },
                Entry::Assistant { text: "hello".into(), thought: "hmm".into() },
                Entry::User { text: "again".into() },
            ]
        );
    }

    #[test]
    fn a_tool_call_is_updated_in_place() {
        let mut thread = Thread::default();
        thread.apply(&json!({ "sessionUpdate": "tool_call", "toolCallId": "t", "title": "Edit", "kind": "edit", "status": "pending" }));
        thread.apply(&chunk("agent_message_chunk", "working"));
        thread.take_dirty();
        thread.apply(&json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "t", "status": "completed",
            "content": [{ "type": "diff", "path": "/a", "oldText": null, "newText": "x" }],
        }));
        assert_eq!(thread.take_dirty(), Some(0));
        let Entry::Tool(call) = &thread.entries[0] else { panic!() };
        assert_eq!(call.status, ToolStatus::Completed);
        assert_eq!(call.content, vec![ToolContent::Diff { path: "/a".into(), old: None, new: "x".into() }]);
    }

    #[test]
    fn tool_calls_in_a_row_are_one_run_summed_up() {
        let mut thread = Thread::default();
        thread.apply(&chunk("agent_message_chunk", "on it"));
        thread.apply(&json!({ "sessionUpdate": "tool_call", "toolCallId": "a", "title": "Read a.rs", "kind": "read", "status": "completed" }));
        thread.apply(&json!({ "sessionUpdate": "tool_call", "toolCallId": "b", "title": "Edit", "kind": "edit", "status": "completed", "locations": [{ "path": "/p/src/a.rs" }] }));
        thread.apply(&json!({ "sessionUpdate": "tool_call", "toolCallId": "c", "title": "cargo test", "kind": "execute", "status": "in_progress" }));
        assert_eq!(thread.run_start(3), 1);
        assert_eq!(thread.run_start(0), 0);
        let run = thread.run(1);
        assert_eq!(run.len(), 3);
        assert_eq!(summarize(&run), ToolRun { status: ToolStatus::InProgress, text: "cargo test".into() });
        thread.apply(&json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c", "status": "completed" }));
        assert_eq!(
            summarize(&thread.run(1)),
            ToolRun { status: ToolStatus::Completed, text: "3 steps · edited a.rs · ran a command".into() }
        );
    }

    #[test]
    fn a_permission_takes_its_title_from_the_tool_call() {
        let mut thread = Thread::default();
        thread.apply(&json!({ "sessionUpdate": "tool_call", "toolCallId": "t", "title": "Write a.txt" }));
        thread.add_permission(json!(7), &json!({ "toolCall": { "toolCallId": "t" }, "options": [{ "optionId": "y", "name": "Allow", "kind": "allow_once" }] }));
        assert_eq!(thread.permissions[0].title, "Write a.txt");
        thread.remove_permission(&json!(7));
        assert!(thread.permissions.is_empty());
    }
}
