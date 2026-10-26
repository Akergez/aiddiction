//! An agent's sign-in that wants a terminal (ACP's `terminal` auth methods:
//! Claude Code's `auth login`), run by the server with pipes for one, so a
//! client anywhere reads what it says and types what it asks for.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use aiddiction_api::{Agent, Login};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::mpsc;

use crate::instances::{shell_command, stop_group};

/// How much of what a sign-in writes is kept: its last bytes.
const OUTPUT_LIMIT: usize = 64 << 10;

pub struct TerminalLogin {
    method_id: String,
    shown: Mutex<Shown>,
    input: mpsc::UnboundedSender<String>,
    pid: Option<u32>,
}

#[derive(Default)]
struct Shown {
    output: VecDeque<u8>,
    running: bool,
    success: Option<bool>,
}

impl TerminalLogin {
    /// Runs the agent's command with the method's arguments in `root`;
    /// `on_success` when it ends well.
    pub fn start(
        agent: &Agent,
        root: &Path,
        init: &Value,
        method_id: &str,
        on_success: impl FnOnce() + Send + 'static,
    ) -> Result<Arc<Self>, String> {
        let (args, env) = terminal_method(init, method_id).ok_or_else(|| format!("the agent has no terminal sign-in \"{method_id}\""))?;
        let mut command = shell_command(agent);
        command
            .args(&args)
            .current_dir(root)
            .env_remove("AIDDICTION_TOKEN")
            .envs(&agent.command.env)
            .envs(&env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .process_group(0);
        let mut child = command.spawn().map_err(|error| format!("starting the sign-in: {error}"))?;
        let (input, mut lines) = mpsc::unbounded_channel::<String>();
        let login = Arc::new(Self {
            method_id: method_id.to_owned(),
            shown: Mutex::new(Shown { running: true, ..Default::default() }),
            input,
            pid: child.id(),
        });

        let mut stdin = child.stdin.take().expect("piped");
        tokio::spawn(async move {
            while let Some(line) = lines.recv().await {
                if stdin.write_all(format!("{line}\n").as_bytes()).await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
            }
        });
        let stdout = tokio::spawn(collect(child.stdout.take().expect("piped"), login.clone()));
        let stderr = tokio::spawn(collect(child.stderr.take().expect("piped"), login.clone()));
        let waiting = login.clone();
        tokio::spawn(async move {
            let status = child.wait().await;
            let _ = tokio::join!(stdout, stderr);
            let success = status.is_ok_and(|status| status.success());
            {
                let mut shown = waiting.shown.lock().unwrap();
                shown.running = false;
                shown.success = Some(success);
            }
            if success {
                on_success();
            }
        });
        Ok(login)
    }

    pub fn info(&self) -> Login {
        let shown = self.shown.lock().unwrap();
        let (front, back) = shown.output.as_slices();
        let raw = String::from_utf8_lossy(&[front, back].concat()).into_owned();
        Login { method_id: self.method_id.clone(), output: plain(&raw), running: shown.running, success: shown.success }
    }

    pub fn write(&self, text: &str) {
        let _ = self.input.send(text.to_owned());
    }

    pub fn stop(&self) {
        if self.shown.lock().unwrap().running
            && let Some(pid) = self.pid {
                stop_group(pid);
            }
    }
}

async fn collect(mut from: impl AsyncRead + Unpin, login: Arc<TerminalLogin>) {
    let mut buffer = [0u8; 4096];
    while let Ok(read) = from.read(&mut buffer).await {
        if read == 0 {
            break;
        }
        let mut shown = login.shown.lock().unwrap();
        shown.output.extend(&buffer[..read]);
        let over = shown.output.len().saturating_sub(OUTPUT_LIMIT);
        shown.output.drain(..over);
    }
}

/// The arguments and environment of the `terminal` auth method `id` in the
/// agent's answer to `initialize`.
pub fn terminal_method(init: &Value, id: &str) -> Option<(Vec<String>, BTreeMap<String, String>)> {
    let method = init["authMethods"].as_array()?.iter().find(|method| method["id"] == id && method["type"] == "terminal")?;
    let args = method["args"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
    let env = method["env"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(name, value)| Some((name.clone(), value.as_str()?.to_owned())))
        .collect();
    Some((args, env))
}

/// Terminal output as text: escape sequences dropped, carriage returns as
/// line ends.
fn plain(raw: &str) -> String {
    let mut text = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                // CSI: parameters up to a final byte.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL or ESC \.
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' => {
                if chars.peek() != Some(&'\n') {
                    text.push('\n');
                }
            }
            '\n' | '\t' => text.push(c),
            c if c.is_control() => {}
            c => text.push(c),
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_terminal_methods_are_run() {
        let init = json!({ "authMethods": [
            { "id": "claude-ai-login", "type": "terminal", "args": ["--cli", "auth", "login"], "env": { "A": "1" } },
            { "id": "api-key", "name": "API Key" },
        ] });
        let (args, env) = terminal_method(&init, "claude-ai-login").unwrap();
        assert_eq!(args, ["--cli", "auth", "login"]);
        assert_eq!(env["A"], "1");
        assert!(terminal_method(&init, "api-key").is_none());
        assert!(terminal_method(&init, "nothing").is_none());
    }

    #[test]
    fn output_loses_its_escape_sequences() {
        assert_eq!(plain("\x1b[1mVisit\x1b[0m: x\r\n\x1b]8;;https://a\x07link\x1b]8;;\x1b\\\rdone"), "Visit: x\nlink\ndone");
    }

    #[tokio::test]
    async fn a_sign_in_reads_what_it_is_given() {
        let dir = tempfile::tempdir().unwrap();
        let agent = Agent {
            id: "a".into(),
            name: "A".into(),
            source: aiddiction_api::AgentSource::Custom,
            command: aiddiction_api::AgentCommand { command: "sh -c 'printf \"code > \"; read code; echo got $code; exit 3' x".into(), args: vec![], env: Default::default() },
        };
        let init = json!({ "authMethods": [{ "id": "t", "type": "terminal" }] });
        let login = TerminalLogin::start(&agent, dir.path(), &init, "t", || {}).unwrap();
        login.write("1234");
        for _ in 0..100 {
            if !login.info().running {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let info = login.info();
        assert_eq!(info.output, "code > got 1234\n");
        assert_eq!(info.success, Some(false));
    }
}
