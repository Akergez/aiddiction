//! The server binary, the fake agent, and the client crate, together: a
//! project is made, a custom agent is added and started in it, a prompt that
//! writes a file is allowed, and a second connection replays the session.
//! And an agent that wants signing in is signed in both ways a client can.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use aiddiction_api::{AgentCommand, AgentInput, InstanceState, NewProject, ProjectSource, TaskInput, ext};
use aiddiction_client::{ServerClient, Socket};
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
    let dir = PathBuf::from(env!("CARGO_BIN_EXE_aiddiction-server")).parent().unwrap().to_path_buf();
    let agent = dir.join("aiddiction-fake-agent");
    if !agent.exists() {
        let status = Command::new(env!("CARGO"))
            .args(["build", "-p", "aiddiction-fake-agent"])
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
    let child = Command::new(env!("CARGO_BIN_EXE_aiddiction-server"))
        .args(["--config", "/nonexistent/config.toml"])
        .env("AIDDICTION_LISTEN", format!("127.0.0.1:{port}"))
        .env("AIDDICTION_DATA_DIR", data.path())
        .env("AIDDICTION_PROJECTS_ROOT", projects.path())
        .env("AIDDICTION_TOKEN", "secret")
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
    aiddiction_client::install_crypto_provider();
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

/// An agent that refuses sessions until it is signed in: once through the
/// server's terminal sign-in, once through `authenticate` and its page.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_is_signed_in_in_a_terminal_and_with_a_page() {
    aiddiction_client::install_crypto_provider();
    let server = start_server().await;
    let client = ServerClient::new(&server.url, "secret").unwrap();
    let marker = tempfile::tempdir().unwrap();
    let signed_in = marker.path().join("signed-in");
    let env = [("FAKE_AGENT_SIGN_IN".to_owned(), signed_in.display().to_string())].into_iter().collect();
    let fake = client
        .add_agent(&AgentInput {
            name: "Locked".into(),
            command: AgentCommand { command: fake_agent().display().to_string(), args: vec![], env },
        })
        .await
        .unwrap();
    let project = client.create_project(&NewProject { name: "Locked".into(), source: ProjectSource::Empty }).await.unwrap();
    let instance = client.start_instance(project.id, &fake.id).await.unwrap();

    let mut acp = Acp { socket: client.acp(instance.id).await.unwrap(), next_id: 0 };
    let init = acp.request("initialize", json!({ "protocolVersion": 1, "clientCapabilities": {} })).await;
    let mut seen = Vec::new();
    let answer = acp.until_response(init, &mut seen).await;
    assert_eq!(answer["result"]["authMethods"][0]["type"], "terminal");
    let status = acp.next().await;
    assert_eq!(status["method"], ext::AUTH_STATUS, "a client is told the agent is signed out");
    assert_eq!(status["params"]["authStatus"]["kind"], "none");

    let new = acp.request("session/new", json!({ "cwd": ".", "mcpServers": [] })).await;
    assert_eq!(acp.until_response(new, &mut seen).await["error"]["code"], -32000);

    assert!(client.start_login(instance.id, "page").await.is_err(), "only terminal methods run on the server");
    let login = client.start_login(instance.id, "terminal").await.unwrap();
    assert!(login.running);
    let mut login = login;
    for _ in 0..100 {
        if login.output.contains("paste the code") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        login = client.login(instance.id).await.unwrap();
    }
    assert!(login.output.contains("https://example.com/fake-sign-in"), "{}", login.output);
    client.login_input(instance.id, "1234").await.unwrap();
    for _ in 0..100 {
        login = client.login(instance.id).await.unwrap();
        if !login.running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(login.success, Some(true), "{}", login.output);
    assert_eq!(std::fs::read_to_string(&signed_in).unwrap(), "1234");
    let new = acp.request("session/new", json!({ "cwd": ".", "mcpServers": [] })).await;
    assert!(acp.until_response(new, &mut seen).await["result"]["sessionId"].is_string());

    std::fs::remove_file(&signed_in).unwrap();
    let authenticate = acp.request("authenticate", json!({ "methodId": "page" })).await;
    let mut page = acp.next().await;
    while page["method"] == "session/update" {
        page = acp.next().await;
    }
    assert_eq!(page["method"], "elicitation/create");
    assert_eq!(page["params"]["requestId"], authenticate, "the page names the client's own request");
    acp.socket
        .outgoing
        .send(json!({ "jsonrpc": "2.0", "id": page["id"], "result": { "action": "accept" } }).to_string())
        .unwrap();
    let mut after = Vec::new();
    let answer = acp.until_response(authenticate, &mut after).await;
    assert!(answer.get("error").is_none(), "{answer}");
    assert!(after.iter().any(|message| message["method"] == "elicitation/complete"));
    assert!(signed_in.exists());
}

/// One of the board's MCP tools, as the person's token calls it.
async fn board(client: &ServerClient, name: &str, arguments: Value) -> Result<Value, String> {
    let message = json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": name, "arguments": arguments } });
    let answer = client.mcp(message).await.unwrap().expect("an answer, not a notification");
    assert!(answer.get("error").is_none(), "{answer}");
    let result = &answer["result"];
    let text = result["content"][0]["text"].as_str().unwrap_or_default().to_owned();
    if result["isError"] == true { Err(text) } else { Ok(serde_json::from_str::<Value>(&text).unwrap()) }
}

async fn fake_agent_on(client: &ServerClient, name: &str) -> aiddiction_api::Agent {
    client
        .add_agent(&AgentInput {
            name: name.into(),
            command: AgentCommand { command: fake_agent().display().to_string(), args: vec![], env: Default::default() },
        })
        .await
        .unwrap()
}

/// MCP starts an agent on a card: the thread is in the common list, the
/// card names it, the settings asked for are the ones it runs with, and a
/// retry is the same start; a setting it refuses is no prompt at all.
#[tokio::test(flavor = "multi_thread")]
async fn start_agent_puts_a_child_thread_in_the_common_list_on_its_card() {
    aiddiction_client::install_crypto_provider();
    let server = start_server().await;
    let client = ServerClient::new(&server.url, "secret").unwrap();
    let _fake = fake_agent_on(&client, "Child agent").await;
    let fake = client.agents().await.unwrap().into_iter().find(|agent| agent.name == "Child agent").unwrap();
    let project = client.create_project(&NewProject { name: "Child work".into(), source: ProjectSource::Empty }).await.unwrap();
    let task = client
        .create_task(project.id, &TaskInput { title: "Fix the child".into(), markdown: "the card".into(), status: None })
        .await
        .unwrap();

    // Until an agent has run once, nothing is known about what it can do.
    let known = board(&client, "list_agents", json!({ "projectId": project.id })).await.unwrap();
    let agent = known.as_array().unwrap().iter().find(|agent| agent["id"] == json!(fake.id)).unwrap();
    assert!(agent.get("profile").is_none(), "{agent}");

    let started = board(
        &client,
        "start_agent",
        json!({
            "projectId": project.id,
            "agentId": fake.id,
            "taskId": task.id,
            "prompt": "Fix the child",
            "requestId": "r1",
            "settings": [
                { "configId": "model", "value": "fake-two" },
                { "configId": "thought_level", "value": "high" },
            ],
        }),
    )
    .await
    .unwrap();
    let session = started["sessionId"].as_str().unwrap().to_owned();
    let instance = started["instanceId"].as_str().unwrap().parse().unwrap();
    assert_eq!(started["taskId"], json!(task.id));
    assert_eq!(started["projectId"], json!(project.id));
    assert_eq!(started["agentId"], json!(fake.id));
    assert_eq!(started["board"], true);

    // The turn runs and ends; the list of conversations has it on the card.
    let mut info = client.instance(instance).await.unwrap();
    for _ in 0..100 {
        if info.sessions.iter().any(|shown| shown.session_id == session && !shown.busy) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        info = client.instance(instance).await.unwrap();
    }
    let shown = info.sessions.iter().find(|shown| shown.session_id == session).expect("the session is listed");
    assert_eq!(shown.task_id, Some(task.id));
    assert_eq!(shown.title, "Fix the child");
    assert!(!shown.busy, "{shown:?}");

    // The card names the thread.
    let linked = client.tasks(project.id).await.unwrap().into_iter().find(|card| card.id == task.id).unwrap().session.unwrap();
    assert_eq!((linked.instance_id, linked.session_id.as_str()), (instance, session.as_str()));

    // The settings asked for, in the order asked: the model, then the effort.
    let mut acp = Acp { socket: client.acp(instance).await.unwrap(), next_id: 0 };
    let attach = acp.request(ext::ATTACH, json!({ "sessionId": session, "afterSeq": 0 })).await;
    let answer = acp.until_response(attach, &mut Vec::new()).await;
    let options = answer["result"]["configOptions"].as_array().unwrap();
    let current = |id: &str| options.iter().find(|option| option["id"] == id).unwrap()["currentValue"].clone();
    assert_eq!(current("model"), json!("fake-two"));
    assert_eq!(current("thought_level"), json!("high"));

    // Retrying the call is the same start, not a second agent.
    let again = board(
        &client,
        "start_agent",
        json!({ "projectId": project.id, "agentId": fake.id, "taskId": task.id, "prompt": "Fix the child", "requestId": "r1" }),
    )
    .await
    .unwrap();
    assert_eq!(again["sessionId"], json!(session));
    assert_eq!(client.instance(instance).await.unwrap().sessions.len(), 1);

    // A setting the agent refuses is named, and no prompt is sent.
    let before = client.instance(instance).await.unwrap().sessions.len();
    let refused = board(
        &client,
        "start_agent",
        json!({
            "projectId": project.id,
            "agentId": fake.id,
            "taskId": task.id,
            "prompt": "must not run",
            "requestId": "r2",
            "settings": [{ "configId": "no_such_option", "value": "x" }],
        }),
    )
    .await
    .unwrap_err();
    assert!(refused.contains("no_such_option"), "{refused}");
    let after = client.instance(instance).await.unwrap();
    assert_eq!(after.sessions.len(), before + 1, "the conversation was opened before the setting was refused");
    let unused = after.sessions.iter().find(|shown| shown.session_id != session).unwrap();
    assert!(!unused.busy, "nothing was sent: {unused:?}");
    assert!(unused.title.is_empty(), "nothing was sent: {unused:?}");

    // Following the card finds its thread, and only it.
    let children = board(&client, "list_subagents", json!({ "projectId": project.id, "taskId": task.id })).await.unwrap();
    assert_eq!(children[0]["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(children[0]["sessions"][0]["sessionId"], json!(session));

    // What the agent said about itself is kept, and how it was last set up.
    let known = board(&client, "list_agents", json!({ "projectId": project.id })).await.unwrap();
    let agent = known.as_array().unwrap().iter().find(|agent| agent["id"] == json!(fake.id)).unwrap();
    assert_eq!(agent["profile"]["capabilities"]["mcpCapabilities"]["http"], true);
    assert_eq!(agent["profile"]["stale"], false);
    assert_eq!(agent["profile"]["agentInfo"]["name"], "fake-agent");
    assert!(agent["lastChoices"]["configOptions"].is_array());
}
