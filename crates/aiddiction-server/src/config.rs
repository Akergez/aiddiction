//! The server's configuration file and where it keeps its data.
//!
//! `<config>/aiddiction-server/config.toml` (the platform's standard config
//! directory, see `aiddiction-paths`), every key optional:
//!
//! ```toml
//! listen = "127.0.0.1:7878"
//! projects_root = "~/aiddiction-projects"
//! token = "…"                 # otherwise generated once into the data dir
//!
//! [[agents]]                  # added to (or replacing) the built-in ones
//! id = "gemini"
//! name = "Gemini CLI"
//! command = "gemini"
//! args = ["--experimental-acp"]
//! env = { }
//! ```

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct File {
    listen: Option<SocketAddr>,
    projects_root: Option<String>,
    token: Option<String>,
    #[serde(default)]
    agents: Vec<ConfigAgent>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigAgent {
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub listen: SocketAddr,
    pub projects_root: PathBuf,
    pub data_dir: PathBuf,
    pub token: Option<String>,
    pub agents: Vec<ConfigAgent>,
}

impl Config {
    /// Reads the file if there is one. `AIDDICTION_LISTEN`,
    /// `AIDDICTION_PROJECTS_ROOT`, `AIDDICTION_DATA_DIR` and `AIDDICTION_TOKEN`
    /// override it, which is what tests and the demo mode use.
    pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        let path = path
            .map(Path::to_path_buf)
            .unwrap_or_else(|| aiddiction_paths::config_dir("aiddiction-server").join("config.toml"));
        let file: File = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("reading {}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => File::default(),
            Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
        };

        let env = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
        let listen = match env("AIDDICTION_LISTEN") {
            Some(listen) => listen.parse().context("AIDDICTION_LISTEN")?,
            None => file.listen.unwrap_or_else(|| ([127, 0, 0, 1], 7878).into()),
        };
        let projects_root = env("AIDDICTION_PROJECTS_ROOT")
            .or(file.projects_root)
            .map(|root| aiddiction_paths::expand_home(&root))
            .unwrap_or_else(|| aiddiction_paths::home_dir().join("aiddiction-projects"));
        let data_dir = env("AIDDICTION_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| aiddiction_paths::data_dir("aiddiction-server"));

        Ok(Self {
            listen,
            projects_root,
            data_dir,
            token: env("AIDDICTION_TOKEN").or(file.token),
            agents: file.agents,
        })
    }
}
