//! `AIDDICTION_DEMO=1`: a server of its own with the fake agent, so the
//! application can be tried (and screenshotted) without a real server.
//!
//! The server and the fake agent are looked for beside this executable
//! (`cargo build --workspace` puts them there). Its data lives under this
//! installation's data directory; point `XDG_DATA_HOME` and
//! `XDG_CONFIG_HOME` somewhere temporary to keep a demo away from real
//! settings.

use std::path::PathBuf;
use std::process::{Child, Command};

use gpui_kit::{App, Global};

use crate::settings::{ServerEntry, Settings};

const TOKEN: &str = "demo";
const PORT: u16 = 7979;

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
    let child = Command::new(dir.join("aiddiction-server"))
        .arg("--config")
        .arg(&config)
        .env("AIDDICTION_LISTEN", format!("127.0.0.1:{PORT}"))
        .env("AIDDICTION_DATA_DIR", data.join("server"))
        .env("AIDDICTION_PROJECTS_ROOT", data.join("projects"))
        .env("AIDDICTION_TOKEN", TOKEN)
        .spawn();
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
    // Give the server a moment, so the first connection finds it.
    std::thread::sleep(std::time::Duration::from_millis(300));
    Settings::update(cx, |settings| {
        if !settings.servers.iter().any(|server| server.id == "demo") {
            settings.servers.insert(
                0,
                ServerEntry {
                    id: "demo".into(),
                    name: "Demo server".into(),
                    url: format!("127.0.0.1:{PORT}"),
                    token: TOKEN.into(),
                },
            );
        }
        settings.active = Some("demo".into());
    });
}
