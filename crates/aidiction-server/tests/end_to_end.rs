//! The server binary, the fake agent, and the client crate, together: a
//! project is made, a custom agent is added and started in it, a prompt that
//! writes a file is allowed, and a second connection replays the session.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use aidiction_api::{AgentCommand, AgentInput, InstanceState, NewProject, ProjectSource, ext};
use aidiction_client::{ServerClient, Socket};
use serde_json::{Value, json};

struct Server {
    child: Child,
    _data: tempfile::TempDir,
    projects: tempfile::TempDir,
    url: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn fake_agent() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_BIN_EXE_aidiction-server")).parent().unwrap().to_path_buf();
    let agent = dir.join("aidiction-fake-agent");
    if !agent.exists() {
        let status = Command::new(env!("CARGO"))
            .args(["build", "-p", "aidiction-fake-agent"])
            .status()
            .unwrap();
        assert!(status.success());
    }
    agent
}

async fn start_server() -> Server {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let data = tempfile::tempdir().unwrap();
    let projects = tempfile::tempdir().unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_aidiction-server"))
        .args(["--config", "/nonexistent/config.toml"])
        .env("AIDICTION_LISTEN", format!("127.0.0.1:{port}"))
        .env("AIDICTION_DATA_DIR", data.path())
        .env("AIDICTION_PROJECTS_ROOT", projects.path())
        .env("AIDICTION_TOKEN", "secret")
        .env("FAKE_AGENT_DELAY_MS", "0")
        .spawn()
        .unwrap();
    let url = format!("127.0.0.1:{port}");
    let client = ServerClient::new(&url, "secret").unwrap();
    for _ in 0..100 {
        if client.health().await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Server { child, _data: data, projects, url }
}

struct Acp {
    socket: Socket,
    next_id: u64,
}

impl Acp {
    async fn request(&mut self, method: &str, params: Value) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.socket
            .outgoing
            .send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string())
            .unwrap();
        id
    }

    async fn next(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(10), self.socket.incoming.recv())
            .await
            .expect("the server said nothing for 10 s")
            .expect("the socket closed");
        serde_json::from_str(&line).unwrap()
    }

    /// Reads until the response to `id`, answering permission requests with
    /// `allow` and keeping every notification.
    async fn until_response(&mut self, id: u64, seen: &mut Vec<Value>) -> Value {
        loop {
            let message = self.next().await;
            if message["id"] == id && message.get("method").is_none() {
                return message;
            }
            if message["method"] == "session/request_permission" {
                let answer = json!({ "jsonrpc": "2.0", "id": message["id"], "result": { "outcome": { "outcome": "selected", "optionId": "allow" } } });
                self.socket.outgoing.send(answer.to_string()).unwrap();
            }
            seen.push(message);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_custom_agent_writes_a_file_and_the_session_replays() {
    aidiction_client::install_crypto_provider();
    let server = start_server().await;
    let client = ServerClient::new(&server.url, "secret").unwrap();
    client.check().await.unwrap();

    let wrong = ServerClient::new(&server.url, "wrong").unwrap();
    assert!(wrong.projects().await.unwrap_err().is_unauthorized());

    let agents = client.agents().await.unwrap();
    assert!(agents.iter().any(|agent| agent.id == "claude"));
    assert!(agents.iter().any(|agent| agent.id == "codex"));
    let fake = client
        .add_agent(&AgentInput {
            name: "Fake".into(),
            command: AgentCommand { command: fake_agent().display().to_string(), args: vec![], env: Default::default() },
        })
        .await
        .unwrap();
    assert_eq!(fake.id, "fake");

    let project = client
        .create_project(&NewProject { name: "Demo project".into(), source: ProjectSource::Empty })
        .await
        .unwrap();
    assert_eq!(project.dir_name, "demo-project");
    let instance = client.start_instance(project.id, &fake.id).await.unwrap();

    let mut acp = Acp { socket: client.acp(instance.id).await.unwrap(), next_id: 0 };
    let init = acp.request("initialize", json!({ "protocolVersion": 1, "clientCapabilities": {} })).await;
    let answer = acp.until_response(init, &mut Vec::new()).await;
    assert_eq!(answer["result"]["agentInfo"]["name"], "fake-agent");

    let new = acp.request("session/new", json!({ "cwd": "/somewhere/else", "mcpServers": [] })).await;
    let answer = acp.until_response(new, &mut Vec::new()).await;
    let session = answer["result"]["sessionId"].as_str().unwrap().to_owned();

    let mut seen = Vec::new();
    let prompt = acp
        .request("session/prompt", json!({ "sessionId": session, "prompt": [{ "type": "text", "text": "make a file" }] }))
        .await;
    let answer = acp.until_response(prompt, &mut seen).await;
    assert_eq!(answer["result"]["stopReason"], "end_turn");
    let written = std::fs::read_to_string(server.projects.path().join("demo-project/hello.txt")).unwrap();
    assert_eq!(written, "Hello from the fake agent!\n");
    assert!(seen.iter().any(|m| m["params"]["update"]["sessionUpdate"] == "user_message_chunk"));

    let info = client.instance(instance.id).await.unwrap();
    assert_eq!(info.state, InstanceState::Ready);
    assert_eq!(info.sessions.len(), 1);
    assert_eq!(info.sessions[0].title, "make a file");

    // A second connection — a phone waking up — is given the whole session.
    drop(acp);
    let mut again = Acp { socket: client.acp(instance.id).await.unwrap(), next_id: 100 };
    let attach = again.request(ext::ATTACH, json!({ "sessionId": session, "afterSeq": 0 })).await;
    let answer = again.until_response(attach, &mut Vec::new()).await;
    assert_eq!(answer["result"]["known"], true);
    assert_eq!(answer["result"]["busy"], false);
    let last_seq = answer["result"]["lastSeq"].as_u64().unwrap();
    let mut replayed = Vec::new();
    while replayed.last().and_then(|m: &Value| m["params"]["_meta"][ext::SEQ_META].as_u64()) != Some(last_seq) {
        replayed.push(again.next().await);
    }
    let text: String = replayed
        .iter()
        .filter(|m| m["params"]["update"]["sessionUpdate"] == "agent_message_chunk")
        .filter_map(|m| m["params"]["update"]["content"]["text"].as_str())
        .collect();
    assert!(text.contains("You said: **make a file**"), "{text}");
    assert!(replayed.iter().any(|m| m["method"] == ext::PROMPT_FINISHED));

    client.stop_instance(instance.id).await.unwrap();
    for _ in 0..50 {
        if client.instance(instance.id).await.unwrap().state == InstanceState::Exited {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the agent did not stop");
}
