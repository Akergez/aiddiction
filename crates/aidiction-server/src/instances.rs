//! Agent processes: one per agent and project, each behind its own [`Proxy`].

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aidiction_api::{Agent, Instance as InstanceInfo, InstanceId, InstanceState, ProjectId, ServerEvent};
use anyhow::Context as _;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::Command;
use tokio::sync::{broadcast, mpsc};

use crate::proxy::Proxy;

/// How many stderr lines an instance keeps to show when it fails.
const STDERR_LINES: usize = 100;
/// `npx` may download the adapter on first use, so this is generous.
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(180);

pub struct Supervisor {
    instances: Mutex<HashMap<InstanceId, Arc<Instance>>>,
    events: broadcast::Sender<ServerEvent>,
}

pub struct Instance {
    pub id: InstanceId,
    pub project_id: ProjectId,
    pub agent: Agent,
    pub proxy: Arc<Proxy>,
    status: Mutex<(InstanceState, Option<String>)>,
    stderr: Mutex<VecDeque<String>>,
    pid: Mutex<Option<u32>>,
}

impl Instance {
    pub fn info(&self) -> InstanceInfo {
        let (state, message) = self.status.lock().unwrap().clone();
        InstanceInfo {
            id: self.id,
            project_id: self.project_id,
            agent_id: self.agent.id.clone(),
            agent_name: self.agent.name.clone(),
            state,
            message,
            sessions: self.proxy.sessions(),
            stderr_tail: self.stderr.lock().unwrap().iter().cloned().collect(),
        }
    }

    pub fn state(&self) -> InstanceState {
        self.status.lock().unwrap().0
    }

    fn set_state(&self, state: InstanceState, message: Option<String>) {
        *self.status.lock().unwrap() = (state, message);
    }

    fn running(&self) -> bool {
        matches!(self.state(), InstanceState::Starting | InstanceState::Ready)
    }

    /// Ends the agent and everything it started: the process group is asked
    /// first, then told.
    pub fn stop(&self) {
        let Some(pid) = *self.pid.lock().unwrap() else { return };
        let group = -(pid as i32);
        // SAFETY: plain syscalls on a process group this server created.
        unsafe { libc::kill(group, libc::SIGTERM) };
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(3)).await;
            unsafe { libc::kill(group, libc::SIGKILL) };
        });
    }
}

impl Supervisor {
    pub fn new(events: broadcast::Sender<ServerEvent>) -> Self {
        Self { instances: Mutex::new(HashMap::new()), events }
    }

    pub fn get(&self, id: InstanceId) -> Option<Arc<Instance>> {
        self.instances.lock().unwrap().get(&id).cloned()
    }

    pub fn for_project(&self, project: ProjectId) -> Vec<Arc<Instance>> {
        let mut found: Vec<_> = self
            .instances
            .lock()
            .unwrap()
            .values()
            .filter(|instance| instance.project_id == project)
            .cloned()
            .collect();
        found.sort_by_key(|a| a.id);
        found
    }

    /// Forgets an instance that is no longer running.
    pub fn remove(&self, id: InstanceId) {
        self.instances.lock().unwrap().remove(&id);
    }

    pub fn stop_project(&self, project: ProjectId) {
        let mut instances = self.instances.lock().unwrap();
        for instance in instances.values().filter(|instance| instance.project_id == project) {
            instance.stop();
        }
        instances.retain(|_, instance| instance.project_id != project);
    }

    pub fn stop_all(&self) {
        for instance in self.instances.lock().unwrap().values() {
            instance.stop();
        }
    }

    /// The running instance of `agent` in the project, or a new one. A new
    /// one answers at once in [`InstanceState::Starting`]; clients may
    /// connect straight away, their `initialize` waits for the agent's.
    pub fn start(&self, project: ProjectId, root: PathBuf, agent: Agent) -> anyhow::Result<Arc<Instance>> {
        let mut instances = self.instances.lock().unwrap();
        if let Some(running) = instances
            .values()
            .find(|instance| instance.project_id == project && instance.agent.id == agent.id && instance.running())
        {
            return Ok(running.clone());
        }
        // An exited instance of the same agent is replaced.
        instances.retain(|_, instance| !(instance.project_id == project && instance.agent.id == agent.id));

        let root = root.canonicalize().with_context(|| format!("opening {}", root.display()))?;
        let mut command = shell_command(&agent);
        command
            .current_dir(&root)
            .env_remove("AIDICTION_TOKEN")
            .envs(&agent.command.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .process_group(0);
        let mut child = command.spawn().with_context(|| format!("starting {}", agent.name))?;

        let (to_agent, mut agent_rx) = mpsc::unbounded_channel::<String>();
        let id = uuid::Uuid::now_v7();
        let events = self.events.clone();
        let proxy = Arc::new(Proxy::new(root, to_agent, move || {
            let _ = events.send(ServerEvent::InstancesChanged { project_id: project });
        }));
        let instance = Arc::new(Instance {
            id,
            project_id: project,
            agent: agent.clone(),
            proxy: proxy.clone(),
            status: Mutex::new((InstanceState::Starting, None)),
            stderr: Mutex::new(VecDeque::new()),
            pid: Mutex::new(child.id()),
        });
        instances.insert(id, instance.clone());
        drop(instances);
        tracing::info!(agent = %agent.id, %project, pid = ?child.id(), "agent started");

        let mut stdin = child.stdin.take().expect("piped");
        tokio::spawn(async move {
            while let Some(line) = agent_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() || stdin.write_all(b"\n").await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
            }
        });

        // Lines can be large (a model catalogue runs past 100 KB); the
        // reader has no limit of its own.
        let stdout = child.stdout.take().expect("piped");
        let reader_proxy = proxy.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if !line.trim().is_empty() {
                    reader_proxy.on_agent_line(&line);
                }
            }
        });

        let stderr = child.stderr.take().expect("piped");
        let stderr_instance = instance.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(agent = %stderr_instance.agent.id, "stderr: {line}");
                let mut tail = stderr_instance.stderr.lock().unwrap();
                tail.push_back(line);
                if tail.len() > STDERR_LINES {
                    tail.pop_front();
                }
            }
        });

        let initialize = proxy.initialize();
        let init_instance = instance.clone();
        let init_events = self.events.clone();
        tokio::spawn(async move {
            let outcome = match tokio::time::timeout(INITIALIZE_TIMEOUT, initialize).await {
                Ok(Ok(Ok(result))) => Ok(result),
                Ok(Ok(Err(error))) => Err(format!(
                    "the agent refused to initialize: {}",
                    error.get("message").and_then(|m| m.as_str()).unwrap_or("unknown error")
                )),
                Ok(Err(_)) => Err("the agent exited before it initialized".to_owned()),
                Err(_) => Err("the agent did not initialize in time".to_owned()),
            };
            match &outcome {
                Ok(_) => init_instance.set_state(InstanceState::Ready, None),
                Err(message) => {
                    if init_instance.running() {
                        init_instance.set_state(InstanceState::Failed, Some(message.clone()));
                    }
                    init_instance.stop();
                }
            }
            init_instance.proxy.initialized(outcome);
            let _ = init_events.send(ServerEvent::InstancesChanged { project_id: project });
        });

        let exit_instance = instance.clone();
        let exit_events = self.events.clone();
        tokio::spawn(async move {
            let status = child.wait().await;
            let message = match status {
                Ok(status) => format!("the agent exited ({status})"),
                Err(error) => format!("lost the agent: {error}"),
            };
            tracing::info!(agent = %exit_instance.agent.id, "{message}");
            *exit_instance.pid.lock().unwrap() = None;
            let failed = exit_instance.state() == InstanceState::Failed;
            if !failed {
                exit_instance.set_state(InstanceState::Exited, Some(message.clone()));
            }
            exit_instance.proxy.agent_exited(&message);
            let _ = exit_events.send(ServerEvent::InstancesChanged { project_id: project });
        });

        let _ = self.events.send(ServerEvent::InstancesChanged { project_id: project });
        Ok(instance)
    }
}

/// The agent's command line, run by bash so it reads the way it would in a
/// terminal (`npx -y pkg`, `FOO=1 agent --acp`), with the configured
/// arguments after it. Bash runs a lone command in its own place; whatever
/// the command starts shares its process group and is stopped with it.
fn shell_command(agent: &Agent) -> Command {
    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg(format!("{} \"$@\"", agent.command.command))
        .arg("aidiction-agent")
        .args(&agent.command.args);
    command
}
