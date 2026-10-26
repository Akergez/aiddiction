//! Signing an agent in: the ways its `initialize` offers, sorted by what the
//! chat does for each. Pure, so it is tested here.

use serde_json::Value;

/// ACP's "authentication required": a session or a turn the agent refused
/// until it is signed in.
pub const AUTH_REQUIRED: i64 = -32000;

#[derive(Debug, Clone, PartialEq)]
pub enum MethodKind {
    /// A command for a terminal; the server runs it and the chat shows what
    /// it says and passes on what the person types (Claude Code).
    Terminal,
    /// `authenticate` with a key the person types (Codex's `api-key`).
    ApiKey,
    /// `authenticate` alone; the agent signs itself in, perhaps showing a
    /// page on the way (Codex's ChatGPT device code).
    Agent,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AuthMethod {
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: MethodKind,
}

/// The agent's `authMethods`.
pub fn methods(init: &Value) -> Vec<AuthMethod> {
    init["authMethods"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|method| {
            let id = method["id"].as_str()?.to_owned();
            let kind = if method["type"] == "terminal" {
                MethodKind::Terminal
            } else if method.pointer("/_meta/api-key").is_some() {
                MethodKind::ApiKey
            } else {
                MethodKind::Agent
            };
            Some(AuthMethod {
                name: method["name"].as_str().filter(|name| !name.is_empty()).unwrap_or(&id).trim().to_owned(),
                description: method["description"].as_str().unwrap_or_default().trim().to_owned(),
                id,
                kind,
            })
        })
        .collect()
}

/// Whether an `_auth/status_update` says the agent is signed out.
pub fn signed_out(params: &Value) -> bool {
    params.pointer("/authStatus/kind").and_then(Value::as_str) == Some("none")
}

/// The first web address in what a sign-in wrote.
pub fn first_url(text: &str) -> Option<&str> {
    let start = text.find("https://").or_else(|| text.find("http://"))?;
    let rest = &text[start..];
    let end = rest.find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>')).unwrap_or(rest.len());
    Some(rest[..end].trim_end_matches(['.', ',', ')']))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn methods_are_sorted_by_what_signing_in_takes() {
        let init = json!({ "authMethods": [
            { "id": "claude-ai-login", "name": "Claude Subscription", "description": "Use Claude subscription ", "type": "terminal", "args": [] },
            { "id": "api-key", "name": "API Key", "_meta": { "api-key": { "provider": "openai" } } },
            { "id": "chat-gpt-device-code", "name": "ChatGPT (device code)" },
            { "name": "no id" },
        ] });
        let found = methods(&init);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].kind, MethodKind::Terminal);
        assert_eq!(found[0].description, "Use Claude subscription");
        assert_eq!(found[1].kind, MethodKind::ApiKey);
        assert_eq!(found[2].kind, MethodKind::Agent);
        assert!(methods(&json!({})).is_empty());
    }

    #[test]
    fn the_status_says_when_nobody_is_signed_in() {
        assert!(signed_out(&json!({ "authStatus": { "kind": "none", "label": "Not logged in" } })));
        assert!(!signed_out(&json!({ "authStatus": { "kind": "subscription" } })));
    }

    #[test]
    fn the_address_is_found_in_what_a_sign_in_wrote() {
        let output = "Opening browser to sign in…\nIf the browser didn't open, visit: https://claude.com/cai/oauth/authorize?code=true&state=x\nPaste code here if prompted >";
        assert_eq!(first_url(output), Some("https://claude.com/cai/oauth/authorize?code=true&state=x"));
        assert_eq!(first_url("go to https://a.b/c."), Some("https://a.b/c"));
        assert_eq!(first_url("nothing"), None);
    }
}
