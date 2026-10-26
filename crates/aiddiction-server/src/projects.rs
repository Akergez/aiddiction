//! Projects: directories under the projects root, made empty or by cloning.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aiddiction_api::{DirEntry, DirListing, NewProject, Project, ProjectId, ProjectSource, ProjectState, ServerEvent, Task, TaskId, TaskInput, TaskStatus};
use anyhow::Context as _;
use tokio::io::AsyncReadExt as _;
use tokio::process::Command;
use tokio::sync::broadcast;

use crate::agents::slug;
use crate::proxy::now_ms;
use crate::store::Store;

/// Clone output is passed on at most this often.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(500);
/// A clone that prints nothing for this long is given up on.
const CLONE_SILENCE: Duration = Duration::from_secs(300);

pub struct Projects {
    root: PathBuf,
    store: Arc<Store>,
    list: Mutex<Vec<Project>>,
    events: broadcast::Sender<ServerEvent>,
    askpass: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("no such project")]
    NotFound,
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] anyhow::Error),
}

impl Projects {
    pub fn open(root: &Path, data_dir: &Path, store: Arc<Store>, events: broadcast::Sender<ServerEvent>) -> anyhow::Result<Self> {
        std::fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
        let mut list = store.projects()?;
        // A clone the server was killed in the middle of will not finish.
        for project in &mut list {
            if project.state == ProjectState::Cloning {
                project.state = ProjectState::Failed;
                project.status_message = Some("the server stopped while cloning".into());
            }
        }
        let askpass = data_dir.join("askpass.sh");
        write_askpass(&askpass)?;
        Ok(Self { root: root.to_path_buf(), store, list: Mutex::new(list), events, askpass })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn list(&self) -> Vec<Project> {
        self.list.lock().unwrap().clone()
    }

    pub fn get(&self, id: ProjectId) -> Option<Project> {
        self.list.lock().unwrap().iter().find(|project| project.id == id).cloned()
    }

    pub fn tasks(&self, project: ProjectId) -> Result<Vec<Task>, ProjectError> {
        self.get(project).ok_or(ProjectError::NotFound)?;
        Ok(self.store.tasks()?.into_iter().filter(|task| task.project_id == project).collect())
    }

    pub fn create_task(&self, project: ProjectId, input: TaskInput) -> Result<Task, ProjectError> {
        self.get(project).ok_or(ProjectError::NotFound)?;
        let title = input.title.trim();
        if title.is_empty() { return Err(ProjectError::Invalid("the task needs a title".into())); }
        let now = now_ms();
        let task = Task { id: uuid::Uuid::now_v7(), project_id: project, title: title.into(), markdown: input.markdown, status: input.status.unwrap_or(TaskStatus::Todo), created_at_ms: now, updated_at_ms: now };
        let mut tasks = self.store.tasks()?; tasks.push(task.clone()); self.store.save_tasks(&tasks)?;
        let _ = self.events.send(ServerEvent::TasksChanged { project_id: project });
        Ok(task)
    }

    pub fn update_task(&self, project: ProjectId, id: TaskId, input: TaskInput) -> Result<Task, ProjectError> {
        self.get(project).ok_or(ProjectError::NotFound)?;
        let mut tasks = self.store.tasks()?;
        let task = tasks.iter_mut().find(|task| task.id == id && task.project_id == project).ok_or(ProjectError::NotFound)?;
        if input.title.trim().is_empty() { return Err(ProjectError::Invalid("the task needs a title".into())); }
        task.title = input.title.trim().into(); task.markdown = input.markdown; if let Some(status) = input.status { task.status = status; } task.updated_at_ms = now_ms();
        let updated = task.clone(); self.store.save_tasks(&tasks)?; let _ = self.events.send(ServerEvent::TasksChanged { project_id: project }); Ok(updated)
    }

    pub fn delete_task(&self, project: ProjectId, id: TaskId) -> Result<(), ProjectError> {
        self.get(project).ok_or(ProjectError::NotFound)?;
        let mut tasks = self.store.tasks()?; let before = tasks.len(); tasks.retain(|task| !(task.id == id && task.project_id == project));
        if tasks.len() == before { return Err(ProjectError::NotFound); } self.store.save_tasks(&tasks)?; let _ = self.events.send(ServerEvent::TasksChanged { project_id: project }); Ok(())
    }

    pub fn dir(&self, project: &Project) -> PathBuf {
        match &project.path {
            Some(path) => PathBuf::from(path),
            None => self.root.join(&project.dir_name),
        }
    }

    pub fn create(self: &Arc<Self>, input: NewProject) -> Result<Project, ProjectError> {
        if let ProjectSource::Existing { path } = &input.source {
            return self.adopt(&input.name, path);
        }
        let name = input.name.trim();
        if name.is_empty() {
            return Err(ProjectError::Invalid("the project needs a name".into()));
        }
        if let ProjectSource::Git { url, .. } = &input.source {
            check_git_url(url)?;
        }

        let mut list = self.list.lock().unwrap();
        let base = slug(name, "project");
        let mut dir_name = base.clone();
        let mut n = 2;
        while list.iter().any(|project| project.dir_name == dir_name) || self.root.join(&dir_name).exists() {
            dir_name = format!("{base}-{n}");
            n += 1;
        }
        let mut project = Project {
            id: uuid::Uuid::now_v7(),
            name: name.to_owned(),
            dir_name,
            path: None,
            state: ProjectState::Ready,
            git_url: None,
            status_message: None,
            created_at_ms: now_ms(),
        };
        match input.source {
            ProjectSource::Empty => {
                std::fs::create_dir(self.dir(&project)).context("creating the project directory")?;
            }
            ProjectSource::Existing { .. } => unreachable!("adopted above"),
            ProjectSource::Git { url, branch, token } => {
                project.state = ProjectState::Cloning;
                project.git_url = Some(url.clone());
                project.status_message = Some("Starting git…".into());
                let this = self.clone();
                let id = project.id;
                let target = self.dir(&project);
                tokio::spawn(async move {
                    let outcome = this.run_clone(id, &url, branch.as_deref(), token.as_deref(), &target).await;
                    this.finish_clone(id, outcome);
                });
            }
        }
        list.push(project.clone());
        self.store.save_projects(&list)?;
        drop(list);
        let _ = self.events.send(ServerEvent::ProjectsChanged);
        Ok(project)
    }

    /// A folder that is already on the server, taken as it is: nothing is made
    /// and, when the project is deleted, nothing is removed.
    fn adopt(&self, name: &str, path: &str) -> Result<Project, ProjectError> {
        let path = aiddiction_paths::expand_home(path.trim());
        if !path.is_absolute() {
            return Err(ProjectError::Invalid(format!("not a full path: {}", path.display())));
        }
        let path = path.canonicalize().map_err(|error| ProjectError::Invalid(format!("{}: {error}", path.display())))?;
        if !path.is_dir() {
            return Err(ProjectError::Invalid(format!("not a folder: {}", path.display())));
        }
        if path.parent().is_none() || self.root.canonicalize().is_ok_and(|root| root == path) {
            return Err(ProjectError::Invalid(format!("{} cannot be a project", path.display())));
        }
        let folder = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        let name = match name.trim() {
            "" => folder.clone(),
            name => name.to_owned(),
        };

        let mut list = self.list.lock().unwrap();
        if let Some(known) = list.iter().find(|project| self.dir(project).canonicalize().is_ok_and(|dir| dir == path)) {
            return Err(ProjectError::Invalid(format!("{} is already the project “{}”", path.display(), known.name)));
        }
        let project = Project {
            id: uuid::Uuid::now_v7(),
            name,
            dir_name: folder,
            path: Some(path.display().to_string()),
            state: ProjectState::Ready,
            git_url: None,
            status_message: None,
            created_at_ms: now_ms(),
        };
        list.push(project.clone());
        self.store.save_projects(&list)?;
        drop(list);
        let _ = self.events.send(ServerEvent::ProjectsChanged);
        Ok(project)
    }

    pub fn rename(&self, id: ProjectId, name: &str) -> Result<Project, ProjectError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(ProjectError::Invalid("the project needs a name".into()));
        }
        let mut list = self.list.lock().unwrap();
        let project = list.iter_mut().find(|project| project.id == id).ok_or(ProjectError::NotFound)?;
        project.name = name.to_owned();
        let renamed = project.clone();
        self.store.save_projects(&list)?;
        drop(list);
        let _ = self.events.send(ServerEvent::ProjectsChanged);
        Ok(renamed)
    }

    /// Forgets the project. Its directory goes too only when asked, and never
    /// for a folder that was adopted rather than made.
    pub fn delete(&self, id: ProjectId, delete_files: bool) -> Result<(), ProjectError> {
        let mut list = self.list.lock().unwrap();
        let index = list.iter().position(|project| project.id == id).ok_or(ProjectError::NotFound)?;
        let project = list.remove(index);
        self.store.save_projects(&list)?;
        drop(list);
        if delete_files && project.path.is_none() {
            let dir = self.dir(&project);
            if dir.starts_with(&self.root) && dir != self.root {
                std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
            }
        }
        let _ = self.events.send(ServerEvent::ProjectsChanged);
        Ok(())
    }

    fn set_status(&self, id: ProjectId, message: String) {
        let mut list = self.list.lock().unwrap();
        if let Some(project) = list.iter_mut().find(|project| project.id == id) {
            project.status_message = Some(message);
        }
        drop(list);
        let _ = self.events.send(ServerEvent::ProjectsChanged);
    }

    fn finish_clone(&self, id: ProjectId, outcome: anyhow::Result<()>) {
        let mut list = self.list.lock().unwrap();
        if let Some(project) = list.iter_mut().find(|project| project.id == id) {
            match outcome {
                Ok(()) => {
                    project.state = ProjectState::Ready;
                    project.status_message = None;
                }
                Err(error) => {
                    tracing::warn!(%id, "clone failed: {error:#}");
                    project.state = ProjectState::Failed;
                    project.status_message = Some(format!("{error:#}"));
                }
            }
        }
        if let Err(error) = self.store.save_projects(&list) {
            tracing::error!("could not save the projects: {error:#}");
        }
        drop(list);
        let _ = self.events.send(ServerEvent::ProjectsChanged);
    }

    /// `git clone` into a temporary directory beside the target, renamed into
    /// place once it is whole. A token reaches git through `GIT_ASKPASS` and
    /// the environment, never the command line or `.git/config`.
    async fn run_clone(
        &self,
        id: ProjectId,
        url: &str,
        branch: Option<&str>,
        token: Option<&str>,
        target: &Path,
    ) -> anyhow::Result<()> {
        let partial = target.with_file_name(format!(
            ".{}.cloning",
            target.file_name().and_then(|name| name.to_str()).unwrap_or("project")
        ));
        let _ = std::fs::remove_dir_all(&partial);

        let mut command = Command::new("git");
        command.arg("clone").arg("--progress");
        if let Some(branch) = branch.filter(|branch| !branch.trim().is_empty()) {
            command.arg("--branch").arg(branch.trim());
        }
        command
            .arg("--")
            .arg(url)
            .arg(&partial)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("AIDDICTION_TOKEN")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(token) = token.filter(|token| !token.is_empty()) {
            command.env("GIT_ASKPASS", &self.askpass).env("AIDDICTION_GIT_TOKEN", token);
        }
        let mut child = command.spawn().context("starting git")?;
        let mut stderr = child.stderr.take().expect("piped");

        // Progress comes as lines ended by `\r`, errors by `\n`.
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        let mut last_line = String::new();
        let mut last_sent = Instant::now() - PROGRESS_INTERVAL;
        loop {
            let read = tokio::time::timeout(CLONE_SILENCE, stderr.read(&mut chunk)).await;
            let n = match read {
                Ok(Ok(0)) => break,
                Ok(Ok(n)) => n,
                Ok(Err(error)) => return Err(error).context("reading git's output"),
                Err(_) => {
                    let _ = child.kill().await;
                    let _ = std::fs::remove_dir_all(&partial);
                    anyhow::bail!("git printed nothing for {} s", CLONE_SILENCE.as_secs());
                }
            };
            for &byte in &chunk[..n] {
                if byte == b'\r' || byte == b'\n' {
                    let line = String::from_utf8_lossy(&buffer).trim().to_owned();
                    buffer.clear();
                    if !line.is_empty() {
                        last_line = line;
                    }
                } else {
                    buffer.push(byte);
                }
            }
            if !last_line.is_empty() && last_sent.elapsed() >= PROGRESS_INTERVAL {
                last_sent = Instant::now();
                self.set_status(id, last_line.clone());
            }
        }
        let status = child.wait().await.context("waiting for git")?;
        if !status.success() {
            let _ = std::fs::remove_dir_all(&partial);
            anyhow::bail!("{}", if last_line.is_empty() { format!("git failed ({status})") } else { last_line });
        }
        std::fs::rename(&partial, target).with_context(|| format!("moving the clone to {}", target.display()))?;
        Ok(())
    }
}

/// The folders in `path` (the home directory when none is given), sorted by
/// name; dot-folders only when `hidden`.
pub fn list_dirs(path: Option<&str>, hidden: bool) -> Result<DirListing, ProjectError> {
    let path = match path.map(str::trim).filter(|path| !path.is_empty()) {
        Some(path) => aiddiction_paths::expand_home(path),
        None => aiddiction_paths::home_dir(),
    };
    let path = path.canonicalize().map_err(|error| ProjectError::Invalid(format!("{}: {error}", path.display())))?;
    let entries = std::fs::read_dir(&path).map_err(|error| ProjectError::Invalid(format!("{}: {error}", path.display())))?;
    let mut dirs: Vec<DirEntry> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .map(|entry| DirEntry { name: entry.file_name().to_string_lossy().into_owned(), path: entry.path().display().to_string() })
        .filter(|entry| hidden || !entry.name.starts_with('.'))
        .collect();
    dirs.sort_by_key(|entry| entry.name.to_lowercase());
    Ok(DirListing {
        parent: path.parent().map(|parent| parent.display().to_string()),
        path: path.display().to_string(),
        dirs,
    })
}

/// Only remote repositories: a local path or `file://` would let a client
/// copy anything the server can read into a project.
fn check_git_url(url: &str) -> Result<(), ProjectError> {
    let url = url.trim();
    let remote = ["https://", "http://", "ssh://", "git://"].iter().any(|scheme| url.starts_with(scheme))
        || (url.contains('@') && url.contains(':') && !url.contains("://"));
    if url.starts_with('-') || !remote {
        return Err(ProjectError::Invalid(format!("not a remote repository address: {url}")));
    }
    Ok(())
}

fn write_askpass(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let script = "#!/bin/sh\n\
        case \"$1\" in\n\
        Username*) echo \"${AIDDICTION_GIT_USER:-x-access-token}\" ;;\n\
        *) echo \"$AIDDICTION_GIT_TOKEN\" ;;\n\
        esac\n";
    std::fs::write(path, script).with_context(|| format!("writing {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn projects(root: &Path, data: &Path) -> Arc<Projects> {
        let store = Arc::new(Store::open(data).unwrap());
        Arc::new(Projects::open(root, data, store, broadcast::channel(8).0).unwrap())
    }

    #[test]
    fn an_existing_folder_is_adopted_once_and_never_deleted() {
        let (root, data, outside) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let projects = projects(root.path(), data.path());
        let source = || ProjectSource::Existing { path: outside.path().display().to_string() };
        let project = projects.create(NewProject { name: String::new(), source: source() }).unwrap();
        assert_eq!(project.name, outside.path().file_name().unwrap().to_string_lossy());
        assert_eq!(projects.dir(&project), outside.path().canonicalize().unwrap());
        assert!(projects.create(NewProject { name: "again".into(), source: source() }).is_err());
        projects.delete(project.id, true).unwrap();
        assert!(outside.path().exists(), "an adopted folder stays");
    }

    #[test]
    fn a_missing_or_relative_folder_is_refused() {
        let (root, data) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let projects = projects(root.path(), data.path());
        for path in ["relative/path", "/no/such/folder"] {
            let source = ProjectSource::Existing { path: path.into() };
            assert!(projects.create(NewProject { name: "x".into(), source }).is_err(), "{path}");
        }
    }

    #[test]
    fn folders_are_listed_without_hidden_ones() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["b", "A", ".hidden"] {
            std::fs::create_dir(dir.path().join(name)).unwrap();
        }
        std::fs::write(dir.path().join("file"), "").unwrap();
        let listing = list_dirs(Some(&dir.path().display().to_string()), false).unwrap();
        let names: Vec<_> = listing.dirs.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["A", "b"]);
        assert!(listing.parent.is_some());
    }

    #[test]
    fn only_remote_git_addresses_are_accepted() {
        assert!(check_git_url("https://github.com/a/b.git").is_ok());
        assert!(check_git_url("git@github.com:a/b.git").is_ok());
        assert!(check_git_url("/etc").is_err());
        assert!(check_git_url("file:///etc").is_err());
        assert!(check_git_url("--upload-pack=x").is_err());
    }
}
