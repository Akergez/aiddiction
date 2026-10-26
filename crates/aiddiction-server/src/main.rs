//! `aiddiction-server`: keeps projects and runs ACP agents in them for
//! Aiddiction clients.
//!
//! ```sh
//! aiddiction-server [--config path/to/config.toml]
//! aiddiction-server token          # print the access token
//! aiddiction-server install [--linger]   # as a systemd user service
//! aiddiction-server uninstall
//! ```

mod agents;
mod config;
mod http;
mod install;
mod instances;
mod login;
mod mcp;
mod projects;
mod proxy;
mod store;

use std::sync::Arc;

use anyhow::Context as _;
use tokio::sync::broadcast;

use crate::agents::Catalog;
use crate::config::Config;
use crate::instances::Supervisor;
use crate::projects::Projects;
use crate::store::Store;

/// How long a stopping server waits for its clients to hang up.
const SHUTDOWN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(3);

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,aiddiction_server=info".into()),
        )
        .init();

    let mut args = std::env::args().skip(1);
    let mut config_path = None;
    let mut command = None;
    let mut linger = false;
    let mut listen = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => config_path = Some(args.next().context("--config needs a path")?),
            "token" | "install" | "uninstall" if command.is_none() => command = Some(arg),
            "--linger" => linger = true,
            "--listen" => listen = Some(args.next().context("--listen needs an address, like 0.0.0.0:7878")?.parse().context("--listen")?),
            "--help" | "-h" => {
                println!("usage: aiddiction-server [--config FILE] [token | install [--linger] [--listen ADDRESS] | uninstall]");
                return Ok(());
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
    }
    match command.as_deref() {
        Some("install") => {
            let config = config_path.map(std::path::PathBuf::from);
            return install::install(install::Options { linger, config, listen });
        }
        Some("uninstall") => return install::uninstall(),
        _ if linger || listen.is_some() => anyhow::bail!("--linger and --listen go with install"),
        _ => {}
    }
    let print_token = command.as_deref() == Some("token");

    let config = Config::load(config_path.as_deref().map(std::path::Path::new))?;
    let store = Arc::new(Store::open(&config.data_dir)?);
    let (token, new_token) = match &config.token {
        Some(token) => (token.clone(), false),
        None => store.token()?,
    };
    if print_token {
        println!("{token}");
        return Ok(());
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(serve(config, store, token, new_token))
}

async fn serve(config: Config, store: Arc<Store>, token: String, new_token: bool) -> anyhow::Result<()> {
    let (events, _) = broadcast::channel(256);
    let catalog = Catalog::new(&config.agents, store.custom_agents()?);
    let projects = Arc::new(Projects::open(&config.projects_root, &config.data_dir, store.clone(), events.clone())?);
    let state = Arc::new(http::AppState {
        token: token.clone(),
        store,
        catalog,
        projects,
        supervisor: Supervisor::new(events.clone()),
        events,
    });

    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("listening on {}", config.listen))?;
    let address = listener.local_addr()?;
    tracing::info!("listening on http://{address}, projects in {}", config.projects_root.display());
    if new_token {
        tracing::info!("a new access token was made: {token}");
    }
    if !address.ip().is_loopback() {
        tracing::warn!("listening beyond this machine without TLS: put it behind a TLS proxy or a VPN");
    }

    let app = http::router(state.clone());
    let deadline_state = state.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("a SIGTERM handler");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
            // A graceful shutdown waits for every connection to close, and a
            // client's event socket need never close: past this, the server
            // stops its agents and goes regardless.
            tokio::spawn(async move {
                tokio::time::sleep(SHUTDOWN_DEADLINE).await;
                tracing::warn!("connections still open after {} s; stopping anyway", SHUTDOWN_DEADLINE.as_secs());
                deadline_state.supervisor.stop_all();
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                std::process::exit(0);
            });
        })
        .await?;
    state.supervisor.stop_all();
    // Give the agents the moment `stop` promises them.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    Ok(())
}
