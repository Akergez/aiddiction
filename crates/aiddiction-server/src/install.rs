//! `aiddiction-server install`: the server as a systemd user service, in one
//! command.
//!
//! ```sh
//! aiddiction-server install [--linger] [--config FILE]
//! aiddiction-server uninstall
//! ```
//!
//! `install` copies the running binary to `~/.local/bin` (unless it already
//! runs from there), writes `~/.config/systemd/user/aiddiction-server.service`,
//! and enables and (re)starts it. The unit carries the `PATH` of the shell
//! `install` was run from: the agents are started by the service, and Claude
//! Code and Codex come through `npx`, so the Node the person uses — often
//! nvm's, which systemd knows nothing of — has to be on it. Running `install`
//! again (after an update, or from another shell) rewrites the unit and
//! restarts the service.
//!
//! `--linger` keeps the service running without a login session
//! (`loginctl enable-linger`): what a server reached from a phone wants.
//!
//! `uninstall` stops and removes the service. The binary, the configuration,
//! the data and the projects stay.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, bail};

use crate::config::Config;
use crate::store::Store;

const UNIT: &str = "aiddiction-server.service";

pub struct Options {
    pub linger: bool,
    pub config: Option<PathBuf>,
}

fn unit_path() -> PathBuf {
    aiddiction_paths::config_dir("systemd").join("user").join(UNIT)
}

/// `$XDG_BIN_HOME`, or `~/.local/bin`.
fn bin_dir() -> PathBuf {
    std::env::var_os("XDG_BIN_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| aiddiction_paths::home_dir().join(".local/bin"))
}

pub fn install(options: Options) -> anyhow::Result<()> {
    require_systemd()?;

    // The binary, where the unit will find it after this one is gone.
    let current = std::env::current_exe().context("finding this program")?;
    let binary = bin_dir().join("aiddiction-server");
    if current.canonicalize().ok() != binary.canonicalize().ok() {
        std::fs::create_dir_all(bin_dir()).with_context(|| format!("creating {}", bin_dir().display()))?;
        // Beside it, then over it: a running copy keeps its old file.
        let partial = binary.with_extension("new");
        std::fs::copy(&current, &partial).with_context(|| format!("copying to {}", partial.display()))?;
        std::fs::rename(&partial, &binary).with_context(|| format!("installing {}", binary.display()))?;
        println!("installed {}", binary.display());
    }

    let config_path = options.config.as_deref().map(std::path::absolute).transpose()?;
    let config = Config::load(config_path.as_deref())?;
    // Made now rather than on the service's first start, so it can be shown.
    let token = match &config.token {
        Some(token) => token.clone(),
        None => Store::open(&config.data_dir)?.token()?.0,
    };

    let path = std::env::var("PATH").unwrap_or_default();
    if !path.split(':').any(|dir| Path::new(dir).join("node").is_file()) {
        eprintln!("warning: no `node` on this PATH; Claude Code and Codex are started with npx and will not run");
    }
    if !path.split(':').any(|dir| Path::new(dir).join("git").is_file()) {
        eprintln!("warning: no `git` on this PATH; projects cannot be cloned");
    }

    let unit = unit_path();
    std::fs::create_dir_all(unit.parent().expect("a unit has a directory"))?;
    std::fs::write(&unit, render_unit(&binary, config_path.as_deref(), &path))
        .with_context(|| format!("writing {}", unit.display()))?;
    println!("wrote {}", unit.display());

    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", UNIT])?;
    // A restart starts it if it was not running, and picks up a new binary
    // or unit if it was.
    systemctl(&["restart", UNIT])?;

    if options.linger {
        let user = std::env::var("USER").context("USER is not set")?;
        run("loginctl", &["enable-linger", &user])?;
        println!("the service keeps running after you log out");
    }

    println!();
    println!("aiddiction-server is running on http://{}", config.listen);
    println!("token: {token}");
    println!();
    println!("status:  systemctl --user status aiddiction-server");
    println!("log:     journalctl --user -u aiddiction-server -f");
    if config.listen.ip().is_loopback() {
        println!(
            "it listens on this machine only; for a phone, set `listen` in {}",
            aiddiction_paths::config_dir("aiddiction-server").join("config.toml").display()
        );
    }
    if !options.linger {
        println!("it stops when you log out; `aiddiction-server install --linger` keeps it running");
    }
    Ok(())
}

pub fn uninstall() -> anyhow::Result<()> {
    require_systemd()?;
    let unit = unit_path();
    if unit.exists() {
        // Stopping an inactive unit is not an error worth stopping for.
        let _ = systemctl(&["disable", "--now", UNIT]);
        std::fs::remove_file(&unit).with_context(|| format!("removing {}", unit.display()))?;
        systemctl(&["daemon-reload"])?;
        println!("removed {}", unit.display());
    } else {
        println!("the service is not installed");
    }
    println!(
        "kept: {}, the configuration, the data and the projects",
        bin_dir().join("aiddiction-server").display()
    );
    Ok(())
}

fn require_systemd() -> anyhow::Result<()> {
    if !cfg!(target_os = "linux") {
        bail!("installing as a service needs systemd, which this system does not have; run `aiddiction-server` directly");
    }
    let available = Command::new("systemctl").args(["--user", "--version"]).output().is_ok_and(|out| out.status.success());
    if !available {
        bail!("`systemctl --user` is not available here");
    }
    Ok(())
}

fn systemctl(args: &[&str]) -> anyhow::Result<()> {
    let mut all = vec!["--user"];
    all.extend_from_slice(args);
    run("systemctl", &all)
}

fn run(program: &str, args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new(program).args(args).status().with_context(|| format!("running {program}"))?;
    if !status.success() {
        bail!("`{program} {}` failed ({status})", args.join(" "));
    }
    Ok(())
}

/// The unit file. Paths are quoted the way systemd reads them, so a home
/// directory with a space in it still works.
fn render_unit(binary: &Path, config: Option<&Path>, path: &str) -> String {
    let mut exec = quote(&binary.display().to_string());
    if let Some(config) = config {
        exec.push_str(" --config ");
        exec.push_str(&quote(&config.display().to_string()));
    }
    format!(
        "# Written by `aiddiction-server install`; run it again to rewrite this.\n\
         [Unit]\n\
         Description=Aiddiction server: projects and ACP agents\n\
         After=network-online.target\n\
         \n\
         [Service]\n\
         ExecStart={exec}\n\
         # The PATH `install` was run with: the agents need Node, git and their CLIs.\n\
         Environment={environment}\n\
         Restart=on-failure\n\
         # SIGTERM to the server, which stops its agents on the way out.\n\
         KillMode=mixed\n\
         TimeoutStopSec=10\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        environment = quote(&format!("PATH={path}")),
    )
}

/// A double-quoted systemd string.
fn quote(text: &str) -> String {
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%").replace('$', "$$");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unit_runs_the_binary_with_the_installing_shells_path() {
        let unit = render_unit(
            Path::new("/home/a b/.local/bin/aiddiction-server"),
            Some(Path::new("/etc/aiddiction.toml")),
            "/home/a b/.nvm/versions/node/v25/bin:/usr/bin",
        );
        assert!(unit.contains(r#"ExecStart="/home/a b/.local/bin/aiddiction-server" --config "/etc/aiddiction.toml""#));
        assert!(unit.contains(r#"Environment="PATH=/home/a b/.nvm/versions/node/v25/bin:/usr/bin""#));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn systemd_specials_are_escaped() {
        assert_eq!(quote(r#"a"b%c$d\e"#), r#""a\"b%%c$$d\\e""#);
    }
}
