//! `AIDDICTION_DEMO=1`: a server of its own with the fake agent, and a sample
//! git repository with uncommitted changes as its first project, so the
//! application — the review included — can be tried (and screenshotted)
//! without a real server.
//!
//! The server and the fake agent are looked for beside this executable
//! (`cargo build --workspace` puts them there). Its data lives under this
//! installation's data directory; point `XDG_DATA_HOME` and
//! `XDG_CONFIG_HOME` somewhere temporary to keep a demo away from real
//! settings.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use gpui_kit::{App, Global};

use crate::settings::{ServerEntry, Settings};

const TOKEN: &str = "demo";
/// How long the server has to start answering.
const STARTUP: Duration = Duration::from_secs(10);

struct DemoServer(Child);

impl Global for DemoServer {}

pub fn start_if_requested(cx: &mut App) {
    if std::env::var_os("AIDDICTION_DEMO").is_none() {
        return;
    }
    let Some(dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(PathBuf::from)) else {
        return;
    };
    let data = crate::paths::data_dir().join("demo");
    let config = data.join("config.toml");
    let _ = std::fs::create_dir_all(&data);
    let agent = dir.join("aiddiction-fake-agent");
    let _ = std::fs::write(
        &config,
        format!("[[agents]]\nid = \"fake\"\nname = \"Fake agent\"\ncommand = {:?}\n", agent.display().to_string()),
    );
    // A free port of its own every time: a demo server some earlier run left
    // behind must not be the one this run talks to.
    let port = match free_port() {
        Ok(port) => port,
        Err(error) => {
            tracing::error!("no port for the demo server: {error}");
            return;
        }
    };
    let url = format!("127.0.0.1:{port}");
    let mut command = Command::new(dir.join("aiddiction-server"));
    command
        .arg("--config")
        .arg(&config)
        .env("AIDDICTION_LISTEN", &url)
        .env("AIDDICTION_DATA_DIR", data.join("server"))
        .env("AIDDICTION_PROJECTS_ROOT", data.join("projects"))
        .env("AIDDICTION_TOKEN", TOKEN);
    // The kernel ends the server when this process ends, however it ends:
    // `on_app_quit` below does not run on a crash or a kill.
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt as _;
        // SAFETY: prctl is async-signal-safe, which is all pre_exec asks.
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
    }
    let child = command.spawn();
    match child {
        Ok(child) => {
            cx.set_global(DemoServer(child));
            cx.on_app_quit(|cx| {
                if let Some(server) = cx.try_global::<DemoServer>() {
                    // SIGTERM: the server stops its agents on the way out.
                    // SAFETY: a plain signal to the child this process started.
                    unsafe { libc::kill(server.0.id() as i32, libc::SIGTERM) };
                }
                async {}
            })
            .detach();
        }
        Err(error) => {
            tracing::error!("could not start the demo server: {error}");
            return;
        }
    }
    let client = match aiddiction_client::ServerClient::new(&url, TOKEN) {
        Ok(client) => client,
        Err(error) => {
            tracing::error!("the demo server's address: {error}");
            return;
        }
    };
    if let Err(error) = wait_for(&client) {
        tracing::error!("the demo server did not start: {error}");
    } else if let Err(error) = add_sample(&data, &client) {
        tracing::warn!("no sample repository: {error}");
    }
    // The entry is rewritten every time: the port is new every time.
    Settings::update(cx, |settings| {
        settings.servers.retain(|server| server.id != "demo");
        settings.servers.insert(
            0,
            ServerEntry { id: "demo".into(), name: "Demo server".into(), url: url.clone(), token: TOKEN.into() },
        );
        settings.active = Some("demo".into());
    });
}

/// A port nothing listens on now. Another process could take it before the
/// server does, which on one machine running one demo does not happen.
fn free_port() -> std::io::Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// Until the server answers, for at most [`STARTUP`].
fn wait_for(client: &aiddiction_client::ServerClient) -> Result<(), String> {
    let started = Instant::now();
    crate::runtime::runtime().block_on(async {
        loop {
            match client.health().await {
                Ok(_) => return Ok(()),
                Err(error) if started.elapsed() > STARTUP => return Err(error.to_string()),
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    })
}

/// A repository with a commit and changes on top of it — one file changed,
/// one deleted, one new — made once and added to the demo server as a
/// project.
fn add_sample(data: &Path, client: &aiddiction_client::ServerClient) -> Result<(), String> {
    let repo = data.join("sample-repo");
    if !repo.join(".git").exists() {
        std::fs::create_dir_all(repo.join("src")).map_err(|error| error.to_string())?;
        let write = |path: &str, text: &str| std::fs::write(repo.join(path), text).map_err(|error| error.to_string());
        write("src/lib.rs", SAMPLE_BEFORE)?;
        write("README.md", "# Sample\n\nA repository to review.\n")?;
        write("old.txt", "this file goes away\n")?;
        git(&repo, &["init", "-q", "-b", "main"])?;
        git(&repo, &["add", "."])?;
        git(&repo, &["commit", "-q", "-m", "First version"])?;
        write("src/lib.rs", SAMPLE_AFTER)?;
        std::fs::remove_file(repo.join("old.txt")).map_err(|error| error.to_string())?;
        write("NOTES.md", "Things to look at:\n\n- the division by zero\n")?;
    }
    let client = client.clone();
    let source = aiddiction_api::ProjectSource::Existing { path: repo.display().to_string() };
    crate::runtime::runtime().block_on(async move {
        let known = client.projects().await.map_err(|error| error.to_string())?;
        if known.iter().any(|project| project.path.as_deref() == Some(repo.display().to_string().as_str())) {
            return Ok(());
        }
        let project = aiddiction_api::NewProject { name: "Sample repo".into(), source };
        client.create_project(&project).await.map(|_| ()).map_err(|error| error.to_string())
    })
}

fn git(dir: &Path, args: &[&str]) -> Result<(), String> {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Demo")
        .env("GIT_AUTHOR_EMAIL", "demo@example.com")
        .env("GIT_COMMITTER_NAME", "Demo")
        .env("GIT_COMMITTER_EMAIL", "demo@example.com")
        .status()
        .map_err(|error| error.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("git {args:?} failed")) }
}

const SAMPLE_BEFORE: &str = "\
/// Averages of a few numbers.
pub fn mean(values: &[f64]) -> f64 {
    let sum: f64 = values.iter().sum();
    sum / values.len() as f64
}

pub fn largest(values: &[f64]) -> f64 {
    let mut largest = values[0];
    for value in values {
        if *value > largest {
            largest = *value;
        }
    }
    largest
}
";

const SAMPLE_AFTER: &str = "\
/// Averages of a few numbers.
pub fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let sum: f64 = values.iter().sum();
    Some(sum / values.len() as f64)
}

pub fn largest(values: &[f64]) -> f64 {
    values.iter().copied().fold(f64::MIN, f64::max)
}

pub fn median(values: &mut [f64]) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    values[values.len() / 2]
}
";
