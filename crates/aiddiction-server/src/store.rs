//! What the server remembers between runs: the projects and the agents added
//! through the API, each a JSON file in the data directory, rewritten whole
//! through a temporary file so a crash never leaves half of one.

use std::path::{Path, PathBuf};

use aiddiction_api::{Agent, Project, Task};
use anyhow::Context as _;
use serde::{Serialize, de::DeserializeOwned};

pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn open(dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self { dir: dir.to_path_buf() })
    }

    pub fn projects(&self) -> anyhow::Result<Vec<Project>> {
        self.read("projects.json")
    }

    pub fn save_projects(&self, projects: &[Project]) -> anyhow::Result<()> {
        self.write("projects.json", projects)
    }

    pub fn custom_agents(&self) -> anyhow::Result<Vec<Agent>> {
        self.read("agents.json")
    }

    pub fn save_custom_agents(&self, agents: &[Agent]) -> anyhow::Result<()> {
        self.write("agents.json", agents)
    }

    pub fn tasks(&self) -> anyhow::Result<Vec<Task>> { self.read("tasks.json") }
    pub fn save_tasks(&self, tasks: &[Task]) -> anyhow::Result<()> { self.write("tasks.json", tasks) }

    /// The access token: the one stored here, or a new one stored now.
    pub fn token(&self) -> anyhow::Result<(String, bool)> {
        let path = self.dir.join("token");
        match std::fs::read_to_string(&path) {
            Ok(token) if !token.trim().is_empty() => Ok((token.trim().to_owned(), false)),
            Ok(_) => self.new_token(&path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => self.new_token(&path),
            Err(error) => Err(error).context("reading the token"),
        }
    }

    fn new_token(&self, path: &Path) -> anyhow::Result<(String, bool)> {
        let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        write_private(path, token.as_bytes())?;
        Ok((token, true))
    }

    fn read<T: DeserializeOwned + Default>(&self, name: &str) -> anyhow::Result<T> {
        let path = self.dir.join(name);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| format!("reading {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
            Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
        }
    }

    fn write<T: Serialize + ?Sized>(&self, name: &str, value: &T) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec_pretty(value)?;
        write_private(&self.dir.join(name), &bytes)
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let temporary = path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .with_context(|| format!("writing {}", temporary.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&temporary, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}
