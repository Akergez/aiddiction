//! The server binary, the fake agent, and the client crate, together: a
//! project is made, a custom agent is added and started in it, a prompt that
//! writes a file is allowed, and a second connection replays the session.
//! And an agent that wants signing in is signed in both ways a client can.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use aiddiction_api::{
    AgentCommand, AgentInput, InstanceState, NewProject, ProjectSource, TaskInput, ext,
};
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
    let dir = PathBuf::from(env!("CARGO_BIN_EXE_aiddiction-server"))
        .parent()
        .unwrap()
        .to_path_buf();
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
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
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
    Server {
        child,
        _data: data,
        projects,
        url,
    }
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
            .send(
                json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
                    .to_string(),
            )
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
            command: AgentCommand {
                command: fake_agent().display().to_string(),
                args: vec![],
                env: Default::default(),
            },
        })
        .await
        .unwrap();
    assert_eq!(fake.id, "fake");

    let project = client
        .create_project(&NewProject {
            name: "Demo project".into(),
            source: ProjectSource::Empty,
        })
        .await
        .unwrap();
    assert_eq!(project.dir_name, "demo-project");
    let instance = client
        .start_instance(project.id, &fake.id, None)
        .await
        .unwrap();

    let mut acp = Acp {
        socket: client.acp(instance.id).await.unwrap(),
        next_id: 0,
    };
    let init = acp
        .request(
            "initialize",
            json!({ "protocolVersion": 1, "clientCapabilities": {} }),
        )
        .await;
    let answer = acp.until_response(init, &mut Vec::new()).await;
    assert_eq!(answer["result"]["agentInfo"]["name"], "fake-agent");

    let new = acp
        .request(
            "session/new",
            json!({ "cwd": "/somewhere/else", "mcpServers": [] }),
        )
        .await;
    let answer = acp.until_response(new, &mut Vec::new()).await;
    let session = answer["result"]["sessionId"].as_str().unwrap().to_owned();

    let mut seen = Vec::new();
    let prompt = acp
        .request(
            "session/prompt",
            json!({ "sessionId": session, "prompt": [{ "type": "text", "text": "make a file" }] }),
        )
        .await;
    let answer = acp.until_response(prompt, &mut seen).await;
    assert_eq!(answer["result"]["stopReason"], "end_turn");
    let written =
        std::fs::read_to_string(server.projects.path().join("demo-project/hello.txt")).unwrap();
    assert_eq!(written, "Hello from the fake agent!\n");
    assert!(
        seen.iter()
            .any(|m| m["params"]["update"]["sessionUpdate"] == "user_message_chunk")
    );

    let info = client.instance(instance.id).await.unwrap();
    assert_eq!(info.state, InstanceState::Ready);
    assert_eq!(info.sessions.len(), 1);
    assert_eq!(info.sessions[0].title, "make a file");

    // A second connection — a phone waking up — is given the whole session.
    drop(acp);
    let mut again = Acp {
        socket: client.acp(instance.id).await.unwrap(),
        next_id: 100,
    };
    let attach = again
        .request(ext::ATTACH, json!({ "sessionId": session, "afterSeq": 0 }))
        .await;
    let answer = again.until_response(attach, &mut Vec::new()).await;
    assert_eq!(answer["result"]["known"], true);
    assert_eq!(answer["result"]["busy"], false);
    let last_seq = answer["result"]["lastSeq"].as_u64().unwrap();
    let mut replayed = Vec::new();
    while replayed
        .last()
        .and_then(|m: &Value| m["params"]["_meta"][ext::SEQ_META].as_u64())
        != Some(last_seq)
    {
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
    let env = [(
        "FAKE_AGENT_SIGN_IN".to_owned(),
        signed_in.display().to_string(),
    )]
    .into_iter()
    .collect();
    let fake = client
        .add_agent(&AgentInput {
            name: "Locked".into(),
            command: AgentCommand {
                command: fake_agent().display().to_string(),
                args: vec![],
                env,
            },
        })
        .await
        .unwrap();
    let project = client
        .create_project(&NewProject {
            name: "Locked".into(),
            source: ProjectSource::Empty,
        })
        .await
        .unwrap();
    let instance = client
        .start_instance(project.id, &fake.id, None)
        .await
        .unwrap();

    let mut acp = Acp {
        socket: client.acp(instance.id).await.unwrap(),
        next_id: 0,
    };
    let init = acp
        .request(
            "initialize",
            json!({ "protocolVersion": 1, "clientCapabilities": {} }),
        )
        .await;
    let mut seen = Vec::new();
    let answer = acp.until_response(init, &mut seen).await;
    assert_eq!(answer["result"]["authMethods"][0]["type"], "terminal");
    let status = acp.next().await;
    assert_eq!(
        status["method"],
        ext::AUTH_STATUS,
        "a client is told the agent is signed out"
    );
    assert_eq!(status["params"]["authStatus"]["kind"], "none");

    let new = acp
        .request("session/new", json!({ "cwd": ".", "mcpServers": [] }))
        .await;
    assert_eq!(
        acp.until_response(new, &mut seen).await["error"]["code"],
        -32000
    );

    assert!(
        client.start_login(instance.id, "page").await.is_err(),
        "only terminal methods run on the server"
    );
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
    assert!(
        login.output.contains("https://example.com/fake-sign-in"),
        "{}",
        login.output
    );
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
    let new = acp
        .request("session/new", json!({ "cwd": ".", "mcpServers": [] }))
        .await;
    assert!(acp.until_response(new, &mut seen).await["result"]["sessionId"].is_string());

    std::fs::remove_file(&signed_in).unwrap();
    let authenticate = acp
        .request("authenticate", json!({ "methodId": "page" }))
        .await;
    let mut page = acp.next().await;
    while page["method"] == "session/update" {
        page = acp.next().await;
    }
    assert_eq!(page["method"], "elicitation/create");
    assert_eq!(
        page["params"]["requestId"], authenticate,
        "the page names the client's own request"
    );
    acp.socket
        .outgoing
        .send(
            json!({ "jsonrpc": "2.0", "id": page["id"], "result": { "action": "accept" } })
                .to_string(),
        )
        .unwrap();
    let mut after = Vec::new();
    let answer = acp.until_response(authenticate, &mut after).await;
    assert!(answer.get("error").is_none(), "{answer}");
    assert!(
        after
            .iter()
            .any(|message| message["method"] == "elicitation/complete")
    );
    assert!(signed_in.exists());
}

/// One of the board's MCP tools, as the person's token calls it.
async fn board(client: &ServerClient, name: &str, arguments: Value) -> Result<Value, String> {
    let message = json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": name, "arguments": arguments } });
    let answer = client
        .mcp(message)
        .await
        .unwrap()
        .expect("an answer, not a notification");
    assert!(answer.get("error").is_none(), "{answer}");
    let result = &answer["result"];
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    if result["isError"] == true {
        Err(text)
    } else {
        Ok(serde_json::from_str::<Value>(&text).unwrap())
    }
}

async fn fake_agent_on(client: &ServerClient, name: &str) -> aiddiction_api::Agent {
    client
        .add_agent(&AgentInput {
            name: name.into(),
            command: AgentCommand {
                command: fake_agent().display().to_string(),
                args: vec![],
                env: Default::default(),
            },
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
    let fake = client
        .agents()
        .await
        .unwrap()
        .into_iter()
        .find(|agent| agent.name == "Child agent")
        .unwrap();
    let project = client
        .create_project(&NewProject {
            name: "Child work".into(),
            source: ProjectSource::Empty,
        })
        .await
        .unwrap();
    let task = client
        .create_task(
            project.id,
            &TaskInput {
                title: "Fix the child".into(),
                markdown: "the card".into(),
                status: None,
            },
        )
        .await
        .unwrap();

    // Until an agent has run once, nothing is known about what it can do.
    let known = board(&client, "list_agents", json!({ "projectId": project.id }))
        .await
        .unwrap();
    let agent = known
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["id"] == json!(fake.id))
        .unwrap();
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
        if info
            .sessions
            .iter()
            .any(|shown| shown.session_id == session && !shown.busy)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        info = client.instance(instance).await.unwrap();
    }
    let shown = info
        .sessions
        .iter()
        .find(|shown| shown.session_id == session)
        .expect("the session is listed");
    assert_eq!(shown.task_id, Some(task.id));
    assert_eq!(shown.title, "Fix the child");
    assert!(!shown.busy, "{shown:?}");

    // The card names the thread.
    let linked = client
        .tasks(project.id)
        .await
        .unwrap()
        .into_iter()
        .find(|card| card.id == task.id)
        .unwrap()
        .session
        .unwrap();
    assert_eq!(
        (linked.instance_id, linked.session_id.as_str()),
        (instance, session.as_str())
    );

    // The settings asked for, in the order asked: the model, then the effort.
    let mut acp = Acp {
        socket: client.acp(instance).await.unwrap(),
        next_id: 0,
    };
    let attach = acp
        .request(ext::ATTACH, json!({ "sessionId": session, "afterSeq": 0 }))
        .await;
    let answer = acp.until_response(attach, &mut Vec::new()).await;
    let options = answer["result"]["configOptions"].as_array().unwrap();
    let current = |id: &str| {
        options.iter().find(|option| option["id"] == id).unwrap()["currentValue"].clone()
    };
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
    assert_eq!(
        after.sessions.len(),
        before + 1,
        "the conversation was opened before the setting was refused"
    );
    let unused = after
        .sessions
        .iter()
        .find(|shown| shown.session_id != session)
        .unwrap();
    assert!(!unused.busy, "nothing was sent: {unused:?}");
    assert!(unused.title.is_empty(), "nothing was sent: {unused:?}");

    // Following the card finds its thread, and only it.
    let children = board(
        &client,
        "list_subagents",
        json!({ "projectId": project.id, "taskId": task.id }),
    )
    .await
    .unwrap();
    assert_eq!(children[0]["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(children[0]["sessions"][0]["sessionId"], json!(session));

    // What the agent said about itself is kept, and how it was last set up.
    let known = board(&client, "list_agents", json!({ "projectId": project.id }))
        .await
        .unwrap();
    let agent = known
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["id"] == json!(fake.id))
        .unwrap();
    assert_eq!(
        agent["profile"]["capabilities"]["mcpCapabilities"]["http"],
        true
    );
    assert_eq!(agent["profile"]["stale"], false);
    assert_eq!(agent["profile"]["agentInfo"]["name"], "fake-agent");
    assert!(agent["lastChoices"]["configOptions"].is_array());
}

/// A message to an agent already started is another turn of the same
/// conversation: no second session, the card stays, and the context it
/// reports grows with every turn.
#[tokio::test(flavor = "multi_thread")]
async fn a_message_continues_the_same_conversation_and_says_how_full_it_is() {
    aiddiction_client::install_crypto_provider();
    let server = start_server().await;
    let client = ServerClient::new(&server.url, "secret").unwrap();
    let _fake = fake_agent_on(&client, "Chatty child").await;
    let fake = client
        .agents()
        .await
        .unwrap()
        .into_iter()
        .find(|agent| agent.name == "Chatty child")
        .unwrap();
    let project = client
        .create_project(&NewProject {
            name: "Continue".into(),
            source: ProjectSource::Empty,
        })
        .await
        .unwrap();
    let task = client
        .create_task(
            project.id,
            &TaskInput {
                title: "Keep going".into(),
                markdown: String::new(),
                status: None,
            },
        )
        .await
        .unwrap();

    let started = board(
        &client,
        "start_agent",
        json!({
            "projectId": project.id,
            "agentId": fake.id,
            "taskId": task.id,
            "prompt": "First turn",
        }),
    )
    .await
    .unwrap();
    let session = started["sessionId"].as_str().unwrap().to_owned();
    let instance: aiddiction_api::InstanceId =
        started["instanceId"].as_str().unwrap().parse().unwrap();

    let idle = |info: &aiddiction_api::Instance| {
        info.sessions
            .iter()
            .find(|shown| shown.session_id == session)
            .filter(|shown| !shown.busy)
            .cloned()
    };
    let mut info = client.instance(instance).await.unwrap();
    for _ in 0..100 {
        if idle(&info).is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        info = client.instance(instance).await.unwrap();
    }
    let first = idle(&info).expect("the first turn ended");
    let usage = first.usage.clone().expect("the agent says how full it is");
    assert_eq!((usage.used, usage.size), (1_000, 200_000));
    assert_eq!(usage.cost.unwrap().currency, "USD");

    // The same conversation takes another message, no new agent or session.
    let sent = board(
        &client,
        "send_message",
        json!({
            "projectId": project.id,
            "sessionId": session,
            "prompt": "Second turn",
            "requestId": "m1",
        }),
    )
    .await
    .unwrap();
    assert_eq!(sent["sessionId"], json!(session));
    assert_eq!(sent["instanceId"], json!(instance));
    assert_eq!(sent["taskId"], json!(task.id));
    assert_eq!(sent["sent"], true);
    // What the context was before this message was sent.
    assert_eq!(sent["usage"]["used"], json!(1_000));

    let mut info = client.instance(instance).await.unwrap();
    for _ in 0..100 {
        if idle(&info).is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        info = client.instance(instance).await.unwrap();
    }
    let second = idle(&info).expect("the second turn ended");
    assert_eq!(
        info.sessions.len(),
        1,
        "a message does not open another conversation"
    );
    assert_eq!(second.title, "First turn", "the first words name it");
    assert_eq!(second.usage.unwrap().used, 2_000, "the context grew");

    // The conversation list gives an orchestrator the same reading.
    let children = board(
        &client,
        "list_subagents",
        json!({ "projectId": project.id, "taskId": task.id }),
    )
    .await
    .unwrap();
    let shown = &children[0]["sessions"][0];
    assert_eq!(shown["usage"]["used"], json!(2_000));
    assert_eq!(shown["usage"]["size"], json!(200_000));
    assert_eq!(shown["busy"], false);
    assert_eq!(shown["taskId"], json!(task.id));

    // A retry is the same message, not a third turn.
    let before = client.instance(instance).await.unwrap().sessions[0].last_seq;
    let again = board(
        &client,
        "send_message",
        json!({
            "projectId": project.id,
            "sessionId": session,
            "prompt": "Second turn",
            "requestId": "m1",
        }),
    )
    .await
    .unwrap();
    assert_eq!(again, sent);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after = client.instance(instance).await.unwrap();
    assert_eq!(after.sessions.len(), 1);
    assert_eq!(after.sessions[0].last_seq, before, "a retry sends nothing");

    // A conversation nobody started is named as missing, not started anew.
    let missing = board(
        &client,
        "send_message",
        json!({ "sessionId": "no-such-session", "prompt": "Hello?" }),
    )
    .await
    .unwrap_err();
    assert!(missing.contains("no conversation"), "{missing}");
}

fn git_in(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap()
}

/// A worktree is a checkout of its own on a new branch: agents work there,
/// it cannot be removed under a running agent or with changes, and its
/// branch stays when it goes.
#[tokio::test(flavor = "multi_thread")]
async fn agents_work_in_a_worktree_of_their_own_and_its_branch_outlives_it() {
    aiddiction_client::install_crypto_provider();
    let server = start_server().await;
    let client = ServerClient::new(&server.url, "secret").unwrap();
    let fake = fake_agent_on(&client, "Worktree agent").await;

    let repo = tempfile::tempdir().unwrap();
    assert!(
        git_in(repo.path(), &["init", "-q", "-b", "main"])
            .status
            .success()
    );
    std::fs::write(repo.path().join("a.txt"), "one\n").unwrap();
    assert!(git_in(repo.path(), &["add", "."]).status.success());
    assert!(
        git_in(repo.path(), &["commit", "-q", "-m", "first"])
            .status
            .success()
    );
    let project = client
        .create_project(&NewProject {
            name: "With worktrees".into(),
            source: ProjectSource::Existing {
                path: repo.path().display().to_string(),
            },
        })
        .await
        .unwrap();

    let checkouts = client.worktrees(project.id).await.unwrap();
    assert_eq!(checkouts.len(), 1);
    assert!(checkouts[0].main);
    assert_eq!(checkouts[0].branch.as_deref(), Some("main"));
    assert_eq!(checkouts[0].id, project.id);

    let made = client
        .create_worktree(
            project.id,
            &aiddiction_api::NewWorktree {
                name: Some("Feature".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let branch = made.branch.clone().unwrap();
    assert!(branch.starts_with("aiddiction/main-"), "{branch}");
    assert_eq!(
        made.base_commit.as_deref().map(|commit| commit.len()),
        Some(40)
    );
    assert!(!std::path::Path::new(&made.path).starts_with(repo.path()));
    assert!(std::path::Path::new(&made.path).join("a.txt").is_file());
    assert_eq!(client.worktrees(project.id).await.unwrap().len(), 2);

    // An agent in it is an agent of its own, and it is reused there.
    let in_worktree = client
        .start_instance(project.id, &fake.id, Some(made.id))
        .await
        .unwrap();
    assert_eq!(in_worktree.worktree_id, made.id);
    assert_eq!(in_worktree.worktree_name, "feature");
    assert_eq!(
        client
            .start_instance(project.id, &fake.id, Some(made.id))
            .await
            .unwrap()
            .id,
        in_worktree.id
    );
    let in_main = client
        .start_instance(project.id, &fake.id, None)
        .await
        .unwrap();
    assert_ne!(in_main.id, in_worktree.id);
    assert_eq!(in_main.worktree_id, project.id);

    // Not while it runs.
    let busy = client
        .delete_worktree(project.id, made.id)
        .await
        .unwrap_err();
    assert!(
        matches!(busy, aiddiction_client::Error::Api { status: 409, .. }),
        "{busy}"
    );
    client.stop_instance(in_worktree.id).await.unwrap();
    client.stop_instance(in_main.id).await.unwrap();

    // Not with changes in it.
    std::fs::write(std::path::Path::new(&made.path).join("b.txt"), "two\n").unwrap();
    let dirty = client
        .delete_worktree(project.id, made.id)
        .await
        .unwrap_err();
    assert!(
        matches!(dirty, aiddiction_client::Error::Api { status: 409, .. }),
        "{dirty}"
    );
    std::fs::remove_file(std::path::Path::new(&made.path).join("b.txt")).unwrap();
    client.delete_worktree(project.id, made.id).await.unwrap();
    assert!(!std::path::Path::new(&made.path).exists());
    assert!(
        git_in(
            repo.path(),
            &["rev-parse", "--verify", &format!("refs/heads/{branch}")]
        )
        .status
        .success()
    );

    // The project's own checkout is not one to remove.
    let main = client
        .delete_worktree(project.id, project.id)
        .await
        .unwrap_err();
    assert!(
        matches!(main, aiddiction_client::Error::Api { status: 400, .. }),
        "{main}"
    );

    // Deleting the project takes the server's worktrees with it, and leaves
    // the adopted folder and the branch alone.
    let extra = client
        .create_worktree(
            project.id,
            &aiddiction_api::NewWorktree {
                name: Some("gone".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let extra_branch = extra.branch.clone().unwrap();
    client.delete_project(project.id, true).await.unwrap();
    assert!(!std::path::Path::new(&extra.path).exists());
    assert!(
        repo.path().join("a.txt").is_file(),
        "an adopted folder stays"
    );
    assert!(
        git_in(
            repo.path(),
            &[
                "rev-parse",
                "--verify",
                &format!("refs/heads/{extra_branch}")
            ]
        )
        .status
        .success()
    );
}

/// MCP starts an agent in a given checkout: the thread is in that worktree,
/// and following the card shows it there.
#[tokio::test(flavor = "multi_thread")]
async fn start_agent_works_in_the_worktree_it_is_given() {
    aiddiction_client::install_crypto_provider();
    let server = start_server().await;
    let client = ServerClient::new(&server.url, "secret").unwrap();
    let fake = fake_agent_on(&client, "Checkout agent").await;
    let repo = tempfile::tempdir().unwrap();
    assert!(
        git_in(repo.path(), &["init", "-q", "-b", "main"])
            .status
            .success()
    );
    std::fs::write(repo.path().join("a.txt"), "one\n").unwrap();
    assert!(git_in(repo.path(), &["add", "."]).status.success());
    assert!(
        git_in(repo.path(), &["commit", "-q", "-m", "first"])
            .status
            .success()
    );
    let project = client
        .create_project(&NewProject {
            name: "MCP worktrees".into(),
            source: ProjectSource::Existing {
                path: repo.path().display().to_string(),
            },
        })
        .await
        .unwrap();
    let made = client
        .create_worktree(
            project.id,
            &aiddiction_api::NewWorktree {
                name: Some("card".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let task = client
        .create_task(
            project.id,
            &TaskInput {
                title: "In a worktree".into(),
                markdown: String::new(),
                status: None,
            },
        )
        .await
        .unwrap();

    let started = board(
        &client,
        "start_agent",
        json!({
            "projectId": project.id,
            "agentId": fake.id,
            "taskId": task.id,
            "worktreeId": made.id,
            "prompt": "Work in the worktree",
        }),
    )
    .await
    .unwrap();
    let instance = started["instanceId"].as_str().unwrap().parse().unwrap();
    assert_eq!(started["worktreeId"], json!(made.id));
    let info = client.instance(instance).await.unwrap();
    assert_eq!(info.worktree_id, made.id);
    assert_eq!(info.worktree_name, "card");

    let children = board(
        &client,
        "list_subagents",
        json!({ "projectId": project.id, "taskId": task.id, "worktreeId": made.id }),
    )
    .await
    .unwrap();
    assert_eq!(children[0]["worktreeId"], json!(made.id));
    assert_eq!(children[0]["sessions"].as_array().unwrap().len(), 1);
    let none_here = board(
        &client,
        "list_subagents",
        json!({ "projectId": project.id, "taskId": task.id, "worktreeId": project.id }),
    )
    .await
    .unwrap();
    assert_eq!(none_here, json!([]));

    // The agent runs in the worktree's folder: the fake agent writes a file
    // there when it is told to, and the project's checkout does not see it.
    let session = started["sessionId"].as_str().unwrap().to_owned();
    let mut acp = Acp {
        socket: client.acp(instance).await.unwrap(),
        next_id: 0,
    };
    let init = acp
        .request(
            "initialize",
            json!({ "protocolVersion": 1, "clientCapabilities": {} }),
        )
        .await;
    acp.until_response(init, &mut Vec::new()).await;
    let prompt = acp
        .request(
            "session/prompt",
            json!({ "sessionId": session, "prompt": [{ "type": "text", "text": "make a file" }] }),
        )
        .await;
    let answer = acp.until_response(prompt, &mut Vec::new()).await;
    assert_eq!(answer["result"]["stopReason"], "end_turn");
    assert!(std::path::Path::new(&made.path).join("hello.txt").is_file());
    assert!(!repo.path().join("hello.txt").exists());
}

/// A checkout the person made themselves is adopted on the way: the agent
/// runs in it under an id of its own, the same folder is the same checkout,
/// another repository's checkout is refused, and removing the id leaves the
/// folder and its branch alone.
#[tokio::test]
async fn start_agent_adopts_a_worktree_the_person_made() {
    aiddiction_client::install_crypto_provider();
    let server = start_server().await;
    let client = ServerClient::new(&server.url, "secret").unwrap();
    let fake = fake_agent_on(&client, "Adopting agent").await;
    let repo = tempfile::tempdir().unwrap();
    assert!(
        git_in(repo.path(), &["init", "-q", "-b", "main"])
            .status
            .success()
    );
    std::fs::write(repo.path().join("a.txt"), "one\n").unwrap();
    assert!(git_in(repo.path(), &["add", "."]).status.success());
    assert!(
        git_in(repo.path(), &["commit", "-q", "-m", "first"])
            .status
            .success()
    );
    let project = client
        .create_project(&NewProject {
            name: "Adopted worktrees".into(),
            source: ProjectSource::Existing {
                path: repo.path().display().to_string(),
            },
        })
        .await
        .unwrap();
    let place = tempfile::tempdir().unwrap();
    let path = place.path().join("coder");
    assert!(
        git_in(
            repo.path(),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "coder",
                path.to_str().unwrap(),
                "HEAD"
            ]
        )
        .status
        .success()
    );

    let start = json!({
        "projectId": project.id,
        "agentId": fake.id,
        "prompt": "Work in the checkout",
        "worktreePath": path.display().to_string(),
    });
    let started = board(&client, "start_agent", start.clone()).await.unwrap();
    let id: aiddiction_api::WorktreeId = started["worktreeId"].as_str().unwrap().parse().unwrap();
    assert_ne!(id, project.id);
    let checkouts = client.worktrees(project.id).await.unwrap();
    assert_eq!(checkouts.len(), 2);
    let adopted = checkouts.iter().find(|checkout| checkout.id == id).unwrap();
    assert!(!adopted.main);
    assert!(!adopted.managed);
    assert_eq!(adopted.name, "coder");
    assert_eq!(adopted.branch.as_deref(), Some("coder"));

    // The same folder is the same checkout, and the agent runs in it.
    let again = board(&client, "start_agent", start).await.unwrap();
    assert_eq!(again["worktreeId"], json!(id));
    assert_eq!(client.worktrees(project.id).await.unwrap().len(), 2);
    let instance = again["instanceId"].as_str().unwrap().parse().unwrap();
    let info = client.instance(instance).await.unwrap();
    assert_eq!(info.worktree_id, id);
    assert_eq!(info.worktree_name, "coder");

    // A checkout of another repository is not this project's to work in.
    let other = tempfile::tempdir().unwrap();
    assert!(
        git_in(other.path(), &["init", "-q", "-b", "main"])
            .status
            .success()
    );
    std::fs::write(other.path().join("b.txt"), "two\n").unwrap();
    assert!(git_in(other.path(), &["add", "."]).status.success());
    assert!(
        git_in(other.path(), &["commit", "-q", "-m", "first"])
            .status
            .success()
    );
    let refused = board(
        &client,
        "start_agent",
        json!({
            "projectId": project.id,
            "agentId": fake.id,
            "prompt": "Work elsewhere",
            "worktreePath": other.path().display().to_string(),
        }),
    )
    .await;
    assert!(
        matches!(&refused, Err(message) if message.contains("another repository")),
        "{refused:?}"
    );

    // Removing the adopted checkout forgets the id; its folder and branch
    // stay, and the agent has to be stopped first.
    assert!(client.delete_worktree(project.id, id).await.is_err());
    client.stop_instance(info.id).await.unwrap();
    // Stopping only signals the agent; the checkout frees when it is gone.
    for _ in 0..50 {
        if client.instance(info.id).await.is_err() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    client.delete_worktree(project.id, id).await.unwrap();
    assert!(path.is_dir());
    assert_eq!(client.worktrees(project.id).await.unwrap().len(), 1);
    assert!(
        git_in(repo.path(), &["rev-parse", "--verify", "refs/heads/coder"])
            .status
            .success()
    );
}

/// An agent can be given a goal instead of a prompt: the adapter's
/// `_session/goal` takes it, the caller's words and the card go into the
/// objective, and the thread is in the common list like any other turn.
#[tokio::test]
async fn start_agent_sets_a_goal_when_asked() {
    aiddiction_client::install_crypto_provider();
    let server = start_server().await;
    let client = ServerClient::new(&server.url, "secret").unwrap();
    let fake = fake_agent_on(&client, "Goal agent").await;
    let repo = tempfile::tempdir().unwrap();
    assert!(
        git_in(repo.path(), &["init", "-q", "-b", "main"])
            .status
            .success()
    );
    std::fs::write(repo.path().join("a.txt"), "one\n").unwrap();
    assert!(git_in(repo.path(), &["add", "."]).status.success());
    assert!(
        git_in(repo.path(), &["commit", "-q", "-m", "first"])
            .status
            .success()
    );
    let project = client
        .create_project(&NewProject {
            name: "Goals".into(),
            source: ProjectSource::Existing {
                path: repo.path().display().to_string(),
            },
        })
        .await
        .unwrap();
    let task = client
        .create_task(
            project.id,
            &TaskInput {
                title: "A goal card".into(),
                markdown: String::new(),
                status: None,
            },
        )
        .await
        .unwrap();

    let started = board(
        &client,
        "start_agent",
        json!({
            "projectId": project.id,
            "agentId": fake.id,
            "taskId": task.id,
            "goal": "Make MIG-08-079 green",
            "prompt": "Read the card first",
        }),
    )
    .await
    .unwrap();
    let session = started["sessionId"].as_str().unwrap().to_owned();

    // The fake agent writes the objective it was handed into the checkout.
    let goal_file = repo.path().join("goal.txt");
    for _ in 0..50 {
        if goal_file.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let objective = std::fs::read_to_string(&goal_file).unwrap();
    assert!(
        objective.starts_with("Make MIG-08-079 green\n\nRead the card first"),
        "{objective}"
    );
    assert!(objective.contains(&task.id.to_string()), "{objective}");

    // The goal's turn is in the common list, on the card.
    let children = board(
        &client,
        "list_subagents",
        json!({ "projectId": project.id, "taskId": task.id }),
    )
    .await
    .unwrap();
    assert!(
        children[0]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|one| one["sessionId"] == json!(session))
    );
    // The agent's own report of the goal reaches the list.
    let mut goal = Value::Null;
    for _ in 0..50 {
        let children = board(
            &client,
            "list_subagents",
            json!({ "projectId": project.id, "taskId": task.id }),
        )
        .await
        .unwrap();
        goal = children[0]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|one| one["sessionId"] == json!(session))
            .and_then(|one| one.get("goal"))
            .cloned()
            .unwrap_or(Value::Null);
        if !goal.is_null() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(goal["status"], json!("active"));
    assert!(
        goal["objective"]
            .as_str()
            .unwrap()
            .starts_with("Make MIG-08-079 green"),
        "{goal}"
    );
}

/// A goal can be changed or cleared on a conversation that already runs, not
/// only at its start; the list says what the agent pursues.
#[tokio::test(flavor = "multi_thread")]
async fn a_goal_can_be_set_and_cleared_on_a_running_conversation() {
    aiddiction_client::install_crypto_provider();
    let server = start_server().await;
    let client = ServerClient::new(&server.url, "secret").unwrap();
    let fake = fake_agent_on(&client, "Running goal").await;
    let repo = tempfile::tempdir().unwrap();
    assert!(
        git_in(repo.path(), &["init", "-q", "-b", "main"])
            .status
            .success()
    );
    std::fs::write(repo.path().join("a.txt"), "one\n").unwrap();
    assert!(git_in(repo.path(), &["add", "."]).status.success());
    assert!(
        git_in(repo.path(), &["commit", "-q", "-m", "first"])
            .status
            .success()
    );
    let project = client
        .create_project(&NewProject {
            name: "Running goals".into(),
            source: ProjectSource::Existing {
                path: repo.path().display().to_string(),
            },
        })
        .await
        .unwrap();

    let started = board(
        &client,
        "start_agent",
        json!({
            "projectId": project.id,
            "agentId": fake.id,
            "prompt": "First, just say hello",
        }),
    )
    .await
    .unwrap();
    let session = started["sessionId"].as_str().unwrap().to_owned();
    let instance: aiddiction_api::InstanceId =
        started["instanceId"].as_str().unwrap().parse().unwrap();
    let mut info = client.instance(instance).await.unwrap();
    for _ in 0..100 {
        if info
            .sessions
            .iter()
            .any(|one| one.session_id == session && !one.busy)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        info = client.instance(instance).await.unwrap();
    }

    let set = board(
        &client,
        "set_goal",
        json!({
            "projectId": project.id,
            "sessionId": session,
            "objective": "Keep the tests green",
        }),
    )
    .await
    .unwrap();
    assert_eq!(set["sessionId"], json!(session));
    assert_eq!(set["goal"]["objective"], json!("Keep the tests green"));
    assert_eq!(set["goal"]["status"], json!("active"));

    // The fake agent took it, and the list says so.
    let goal_file = repo.path().join("goal.txt");
    for _ in 0..50 {
        if goal_file.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        std::fs::read_to_string(&goal_file).unwrap(),
        "Keep the tests green"
    );
    let children = board(
        &client,
        "list_subagents",
        json!({ "projectId": project.id }),
    )
    .await
    .unwrap();
    let shown = children[0]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|one| one["sessionId"] == json!(session))
        .unwrap();
    assert_eq!(shown["goal"]["objective"], json!("Keep the tests green"));
    assert_eq!(shown["goal"]["status"], json!("active"));

    // It can be cleared, and the agent stops pursuing it.
    let cleared = board(
        &client,
        "set_goal",
        json!({
            "projectId": project.id,
            "sessionId": session,
            "clear": true,
        }),
    )
    .await
    .unwrap();
    assert_eq!(cleared["goal"], json!(null));
    for _ in 0..50 {
        if !goal_file.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!goal_file.exists());
    let children = board(
        &client,
        "list_subagents",
        json!({ "projectId": project.id }),
    )
    .await
    .unwrap();
    let shown = children[0]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|one| one["sessionId"] == json!(session))
        .unwrap();
    assert!(shown.get("goal").is_none_or(Value::is_null), "{shown}");

    // What is asked for must be one thing: an objective or a clear.
    let both = board(
        &client,
        "set_goal",
        json!({
            "projectId": project.id,
            "sessionId": session,
            "objective": "x",
            "clear": true,
        }),
    )
    .await
    .unwrap_err();
    assert!(both.contains("not both"), "{both}");
    let neither = board(
        &client,
        "set_goal",
        json!({ "projectId": project.id, "sessionId": session }),
    )
    .await
    .unwrap_err();
    assert!(neither.contains("objective is required"), "{neither}");
    let gone = board(
        &client,
        "set_goal",
        json!({ "sessionId": "no-such-session", "objective": "x" }),
    )
    .await
    .unwrap_err();
    assert!(gone.contains("no conversation"), "{gone}");
}

/// A start with neither a prompt nor a goal is refused before an agent runs.
#[tokio::test]
async fn start_agent_needs_a_prompt_or_a_goal() {
    aiddiction_client::install_crypto_provider();
    let server = start_server().await;
    let client = ServerClient::new(&server.url, "secret").unwrap();
    let fake = fake_agent_on(&client, "Nothing to do").await;
    let repo = tempfile::tempdir().unwrap();
    assert!(
        git_in(repo.path(), &["init", "-q", "-b", "main"])
            .status
            .success()
    );
    let project = client
        .create_project(&NewProject {
            name: "Nothing to do".into(),
            source: ProjectSource::Existing {
                path: repo.path().display().to_string(),
            },
        })
        .await
        .unwrap();
    let refused = board(
        &client,
        "start_agent",
        json!({ "projectId": project.id, "agentId": fake.id }),
    )
    .await;
    assert!(
        matches!(&refused, Err(message) if message.contains("prompt or goal")),
        "{refused:?}"
    );
    assert!(client.instances(project.id).await.unwrap().is_empty());
}

/// The board can make, list and remove checkouts itself: an orchestrator
/// never needs a folder, only ids.
#[tokio::test]
async fn worktrees_are_made_listed_and_removed_from_the_board() {
    aiddiction_client::install_crypto_provider();
    let server = start_server().await;
    let client = ServerClient::new(&server.url, "secret").unwrap();
    let fake = fake_agent_on(&client, "Checkout agent").await;
    let repo = tempfile::tempdir().unwrap();
    assert!(
        git_in(repo.path(), &["init", "-q", "-b", "main"])
            .status
            .success()
    );
    std::fs::write(repo.path().join("a.txt"), "one\n").unwrap();
    assert!(git_in(repo.path(), &["add", "."]).status.success());
    assert!(
        git_in(repo.path(), &["commit", "-q", "-m", "first"])
            .status
            .success()
    );
    let project = client
        .create_project(&NewProject {
            name: "Board checkouts".into(),
            source: ProjectSource::Existing {
                path: repo.path().display().to_string(),
            },
        })
        .await
        .unwrap();

    // Only the project's own checkout at first.
    let listed = board(
        &client,
        "list_worktrees",
        json!({ "projectId": project.id }),
    )
    .await
    .unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["main"], json!(true));
    assert_eq!(listed[0]["worktreeId"], json!(project.id));

    let made = board(
        &client,
        "create_worktree",
        json!({ "projectId": project.id, "name": "coder", "base": "HEAD" }),
    )
    .await
    .unwrap();
    let id: aiddiction_api::WorktreeId = made["worktreeId"].as_str().unwrap().parse().unwrap();
    let branch = made["branch"].as_str().unwrap().to_owned();
    assert!(branch.starts_with("aiddiction/main-"), "{branch}");
    assert_eq!(made["name"], json!("coder"));
    let path = std::path::PathBuf::from(made["path"].as_str().unwrap());
    assert!(path.is_dir());
    assert_eq!(made["clone"], json!(true));
    // A clone is a checkout of its own: a commit there is in the project
    // only once the board takes it.
    std::fs::write(path.join("b.txt"), "two\n").unwrap();
    assert!(git_in(&path, &["add", "."]).status.success());
    assert!(
        git_in(
            &path,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "second"
            ]
        )
        .status
        .success()
    );
    let sha = String::from_utf8_lossy(&git_in(&path, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_owned();
    assert!(
        !git_in(
            repo.path(),
            &["rev-parse", "--verify", &format!("refs/heads/{branch}")]
        )
        .status
        .success()
    );
    // The clone's work is the only copy: it is not removed under the round.
    let untaken = board(
        &client,
        "remove_worktree",
        json!({ "projectId": project.id, "worktreeId": id }),
    )
    .await
    .unwrap_err();
    assert!(untaken.contains("results first"), "{untaken}");

    let taken = board(
        &client,
        "get_worktree_results",
        json!({ "projectId": project.id, "worktreeId": id }),
    )
    .await
    .unwrap();
    assert_eq!(taken["branch"], json!(branch));
    assert_eq!(taken["commit"], json!(sha));
    assert!(
        git_in(
            repo.path(),
            &["rev-parse", "--verify", &format!("refs/heads/{branch}")]
        )
        .status
        .success()
    );

    let listed = board(
        &client,
        "list_worktrees",
        json!({ "projectId": project.id }),
    )
    .await
    .unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 2);
    assert_eq!(listed[1]["worktreeId"], json!(id));
    assert_eq!(listed[1]["branch"], json!(null), "the round was taken out");
    assert_eq!(listed[1]["lastResult"], json!(branch));
    assert_eq!(listed[1]["managed"], json!(true));

    // The orchestrator merges it; a new round starts from the branch as it
    // is now, and the commit is already there.
    assert!(
        git_in(repo.path(), &["merge", "--no-ff", "--no-edit", &branch])
            .status
            .success()
    );
    let fresh = board(
        &client,
        "start_new_worktree_work",
        json!({ "projectId": project.id, "worktreeId": id }),
    )
    .await
    .unwrap();
    let next = fresh["branch"].as_str().unwrap().to_owned();
    assert!(next.starts_with("aiddiction/main-"), "{next}");
    assert_ne!(next, branch);
    assert_eq!(fresh["base"], json!("main"));
    assert!(path.join("b.txt").is_file(), "the new round has the merge");

    // An agent started there works in that checkout.
    let started = board(
        &client,
        "start_agent",
        json!({
            "projectId": project.id,
            "agentId": fake.id,
            "worktreeId": id,
            "prompt": "Work there",
        }),
    )
    .await
    .unwrap();
    assert_eq!(started["worktreeId"], json!(id));

    // It cannot go under a running agent.
    let refused = board(
        &client,
        "remove_worktree",
        json!({ "projectId": project.id, "worktreeId": id }),
    )
    .await;
    assert!(
        matches!(&refused, Err(message) if message.contains("still running")),
        "{refused:?}"
    );
    let instance: aiddiction_api::InstanceId =
        started["instanceId"].as_str().unwrap().parse().unwrap();
    client.stop_instance(instance).await.unwrap();
    for _ in 0..50 {
        if client.instance(instance).await.is_err() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // The round must be taken before the folder goes.
    let untaken = board(
        &client,
        "remove_worktree",
        json!({ "projectId": project.id, "worktreeId": id }),
    )
    .await
    .unwrap_err();
    assert!(untaken.contains("results first"), "{untaken}");
    board(
        &client,
        "get_worktree_results",
        json!({ "projectId": project.id, "worktreeId": id }),
    )
    .await
    .unwrap();
    board(
        &client,
        "remove_worktree",
        json!({ "projectId": project.id, "worktreeId": id }),
    )
    .await
    .unwrap();
    assert!(!path.exists());
    assert!(
        git_in(
            repo.path(),
            &["rev-parse", "--verify", &format!("refs/heads/{branch}")]
        )
        .status
        .success(),
        "the taken branch stays in the project"
    );
    let listed = board(
        &client,
        "list_worktrees",
        json!({ "projectId": project.id }),
    )
    .await
    .unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
}

/// A round of a checkout gets a temporary branch of its own: the work goes
/// into the project's repository and out of the checkout, and the next round
/// starts from the base as it is then. Both a clone and a shared worktree do
/// this; uncommitted work stops it.
#[tokio::test(flavor = "multi_thread")]
async fn a_round_goes_to_the_project_and_the_next_starts_from_the_base() {
    aiddiction_client::install_crypto_provider();
    let server = start_server().await;
    let client = ServerClient::new(&server.url, "secret").unwrap();

    let repo = tempfile::tempdir().unwrap();
    assert!(
        git_in(repo.path(), &["init", "-q", "-b", "main"])
            .status
            .success()
    );
    std::fs::write(repo.path().join("a.txt"), "one\n").unwrap();
    assert!(git_in(repo.path(), &["add", "."]).status.success());
    assert!(
        git_in(repo.path(), &["commit", "-q", "-m", "first"])
            .status
            .success()
    );
    let project = client
        .create_project(&NewProject {
            name: "Rounds".into(),
            source: ProjectSource::Existing {
                path: repo.path().display().to_string(),
            },
        })
        .await
        .unwrap();

    let mut rounds = Vec::new();
    for (name, mode) in [("clone", "clone"), ("shared", "worktree")] {
        let made = board(
            &client,
            "create_worktree",
            json!({ "projectId": project.id, "name": name, "mode": mode }),
        )
        .await
        .unwrap();
        assert_eq!(made["base"], "main", "the branch it follows: {made}");
        let branch = made["branch"].as_str().unwrap().to_owned();
        assert!(branch.starts_with("aiddiction/main-"), "{branch}");
        let id: aiddiction_api::WorktreeId = made["worktreeId"].as_str().unwrap().parse().unwrap();
        let path = PathBuf::from(made["path"].as_str().unwrap());

        std::fs::write(path.join(format!("{name}.txt")), format!("from {name}\n")).unwrap();
        assert!(git_in(&path, &["add", "."]).status.success());
        assert!(
            git_in(&path, &["commit", "-q", "-m", name])
                .status
                .success(),
            "a commit in the {name}"
        );
        let sha = String::from_utf8_lossy(&git_in(&path, &["rev-parse", "HEAD"]).stdout)
            .trim()
            .to_owned();

        // Uncommitted work is not handed over.
        std::fs::write(path.join("scratch.txt"), "wip\n").unwrap();
        let dirty = board(
            &client,
            "get_worktree_results",
            json!({ "projectId": project.id, "worktreeId": id }),
        )
        .await
        .unwrap_err();
        assert!(dirty.contains("changes"), "{dirty}");
        std::fs::remove_file(path.join("scratch.txt")).unwrap();

        let taken = board(
            &client,
            "get_worktree_results",
            json!({ "projectId": project.id, "worktreeId": id }),
        )
        .await
        .unwrap();
        assert_eq!(taken["branch"], json!(branch));
        assert_eq!(taken["commit"], json!(sha));
        // The project has the branch; the checkout has no branch of it.
        assert!(
            git_in(
                repo.path(),
                &["rev-parse", "--verify", &format!("refs/heads/{branch}")]
            )
            .status
            .success(),
            "the {name}'s branch is in the project"
        );
        assert!(
            !git_in(&path, &["symbolic-ref", "-q", "HEAD"])
                .status
                .success(),
            "the {name} is left without a branch"
        );
        rounds.push((id, path, branch, name));
    }

    // The orchestrator merges both results into the base.
    for (_, _, branch, _) in &rounds {
        assert!(
            git_in(repo.path(), &["merge", "--no-ff", "--no-edit", branch])
                .status
                .success(),
            "merge {branch} in the base"
        );
    }
    let base_commit = String::from_utf8_lossy(&git_in(repo.path(), &["rev-parse", "main"]).stdout)
        .trim()
        .to_owned();

    // The next round starts from the base as it is now, with both results.
    for (id, path, branch, name) in &rounds {
        let fresh = board(
            &client,
            "start_new_worktree_work",
            json!({ "projectId": project.id, "worktreeId": id }),
        )
        .await
        .unwrap();
        let next = fresh["branch"].as_str().unwrap().to_owned();
        assert!(next.starts_with("aiddiction/main-"), "{next}");
        assert_ne!(&next, branch, "a round gets a branch of its own");
        assert_eq!(fresh["base"], json!("main"));
        assert_eq!(fresh["baseCommit"], json!(base_commit));
        assert!(path.join(format!("{name}.txt")).is_file());
        assert!(path.join("a.txt").is_file());

        // While the round runs, another one is refused.
        let running = board(
            &client,
            "start_new_worktree_work",
            json!({ "projectId": project.id, "worktreeId": id }),
        )
        .await
        .unwrap_err();
        assert!(running.contains("results first"), "{running}");
    }
}
