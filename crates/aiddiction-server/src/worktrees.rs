//! The checkouts of a project an agent can work in: its own folder, and git
//! worktrees the server makes beside it with a branch of their own.
//!
//! A worktree has its own working files and index and shares the
//! repository's objects and refs, so several agents can work in one
//! project without stepping on each other's changes. The server makes them
//! outside the main checkout, removes one only when it is asked to, has
//! nothing running in it and nothing uncommitted, and leaves its branch
//! behind.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::{Arc, Mutex};

use aiddiction_api::{NewWorktree, Project, ProjectId, ServerEvent, Worktree, WorktreeId};
use tokio::process::Command;
use tokio::sync::broadcast;

use crate::agents::slug;
use crate::proxy::now_ms;
use crate::store::Store;

pub struct Worktrees {
    /// Where the server makes the checkouts it manages.
    root: PathBuf,
    store: Arc<Store>,
    list: Mutex<Vec<Worktree>>,
    events: broadcast::Sender<ServerEvent>,
}

#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("no such worktree")]
    NotFound,
    #[error("the project's own checkout is not a worktree to remove")]
    Main,
    #[error("{0}")]
    Invalid(String),
    #[error("the agent is still working in it")]
    Busy,
    #[error("it has changes; commit them or remove the worktree yourself")]
    Dirty,
    #[error(transparent)]
    Io(#[from] anyhow::Error),
}

impl Worktrees {
    pub fn open(root: &Path, store: Arc<Store>, events: broadcast::Sender<ServerEvent>) -> anyhow::Result<Self> {
        let mut list = store.worktrees()?;
        // A folder that is gone is not a checkout any more.
        let before = list.len();
        list.retain(|worktree| Path::new(&worktree.path).is_dir());
        if list.len() != before {
            store.save_worktrees(&list)?;
        }
        Ok(Self { root: root.join(".aiddiction-worktrees"), store, list: Mutex::new(list), events })
    }

    /// The checkouts of a project, its own first.
    pub async fn list_for(&self, project: &Project, dir: &Path) -> Vec<Worktree> {
        let mut found = vec![main(project, dir, branch_of(dir).await)];
        let managed: Vec<_> = self.list.lock().unwrap().iter().filter(|worktree| worktree.project_id == project.id).cloned().collect();
        found.extend(managed);
        found
    }

    /// One checkout by id: the project's own (`project.id`) or one made here.
    pub async fn get(&self, project: &Project, dir: &Path, id: WorktreeId) -> Result<Worktree, WorktreeError> {
        if id == project.id {
            return Ok(main(project, dir, branch_of(dir).await));
        }
        self.list
            .lock()
            .unwrap()
            .iter()
            .find(|worktree| worktree.id == id && worktree.project_id == project.id)
            .cloned()
            .ok_or(WorktreeError::NotFound)
    }

    /// Makes a worktree on a new branch, outside the main checkout.
    pub async fn create(&self, project: &Project, dir: &Path, input: NewWorktree) -> Result<Worktree, WorktreeError> {
        let name = match (input.name.as_deref().map(str::trim).filter(|name| !name.is_empty()), input.branch.as_deref()) {
            (Some(name), _) => slug(name, "worktree"),
            (None, Some(branch)) => slug(branch, "worktree"),
            (None, None) => format!("worktree-{}", &uuid::Uuid::now_v7().to_string()[..8]),
        };
        let branch = input
            .branch
            .as_deref()
            .map(str::trim)
            .filter(|branch| !branch.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("aiddiction/{name}"));
        checkout_branch(&branch)?;

        if self.list.lock().unwrap().iter().any(|worktree| worktree.project_id == project.id && worktree.name == name) {
            return Err(WorktreeError::Invalid(format!("the project already has a worktree called {name}")));
        }
        if let Ok(output) = git(dir, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).await
            && output.status.success()
        {
            return Err(WorktreeError::Invalid(format!("the branch {branch} already exists")));
        }
        let base = input.base.as_deref().map(str::trim).filter(|base| !base.is_empty()).unwrap_or("HEAD").to_owned();
        let base_commit = match git(dir, &["rev-parse", "--verify", &format!("{base}^{{commit}}")]).await {
            Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            _ => return Err(WorktreeError::Invalid(format!("{base} is not a commit here"))),
        };

        let path = self.path_of(project.id, &name);
        if path.exists() {
            return Err(WorktreeError::Invalid(format!("{} already exists", path.display())));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| WorktreeError::Invalid(format!("{}: {error}", parent.display())))?;
        }
        let path_text = path.to_string_lossy().into_owned();
        let output = git(dir, &["worktree", "add", "-b", &branch, &path_text, &base])
            .await
            .map_err(|error| WorktreeError::Invalid(format!("could not make the worktree: {error}")))?;
        if !output.status.success() {
            return Err(WorktreeError::Invalid(format!(
                "could not make the worktree: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }

        let worktree = Worktree {
            id: uuid::Uuid::now_v7(),
            project_id: project.id,
            main: false,
            name,
            branch: Some(branch),
            base_commit: Some(base_commit),
            path: path_text,
            created_at_ms: now_ms(),
        };
        let mut list = self.list.lock().unwrap();
        list.push(worktree.clone());
        self.store.save_worktrees(&list)?;
        drop(list);
        let _ = self.events.send(ServerEvent::WorktreesChanged { project_id: project.id });
        Ok(worktree)
    }

    /// Removes a worktree the server made. Its branch stays.
    pub async fn remove(&self, project: &Project, dir: &Path, id: WorktreeId) -> Result<(), WorktreeError> {
        if id == project.id {
            return Err(WorktreeError::Main);
        }
        let worktree = self
            .list
            .lock()
            .unwrap()
            .iter()
            .find(|worktree| worktree.id == id && worktree.project_id == project.id)
            .cloned()
            .ok_or(WorktreeError::NotFound)?;
        let path = PathBuf::from(&worktree.path);
        if path.is_dir() {
            if !dirty(&path).await? {
                let output = git(dir, &["worktree", "remove", &worktree.path]).await?;
                if !output.status.success() {
                    return Err(WorktreeError::Dirty);
                }
            } else {
                return Err(WorktreeError::Dirty);
            }
        }
        let _ = git(dir, &["worktree", "prune"]).await;
        self.forget(&[worktree.id]);
        let _ = self.events.send(ServerEvent::WorktreesChanged { project_id: project.id });
        Ok(())
    }

    /// Forgets a project's worktrees and takes their folders away: the
    /// project is being deleted, and these are the server's own.
    pub fn remove_project(&self, project: &Project, dir: &Path) {
        let taken = self.known(project.id);
        for worktree in &taken {
            let path = PathBuf::from(&worktree.path);
            if !path.is_dir() {
                continue;
            }
            // Project deletion is already the person's decision; a checkout
            // left behind would be dead weight nobody can find.
            let removed = std::process::Command::new("git")
                .args(["worktree", "remove", "--force", &worktree.path])
                .current_dir(dir)
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("LC_ALL", "C")
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false);
            if !removed {
                let _ = std::fs::remove_dir_all(&path);
            }
        }
        let _ = std::process::Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .output();
        let parent = self.root.join(project.id.to_string());
        let _ = std::fs::remove_dir_all(&parent);
        self.forget(&taken.iter().map(|worktree| worktree.id).collect::<Vec<_>>());
    }

    /// Worktrees of a project as they were last kept, without asking git.
    pub fn known(&self, project: ProjectId) -> Vec<Worktree> {
        self.list.lock().unwrap().iter().filter(|worktree| worktree.project_id == project).cloned().collect()
    }

    fn forget(&self, ids: &[WorktreeId]) {
        let mut list = self.list.lock().unwrap();
        list.retain(|worktree| !ids.contains(&worktree.id));
        if let Err(error) = self.store.save_worktrees(&list) {
            tracing::warn!("could not keep the worktrees: {error:#}");
        }
    }

    fn path_of(&self, project: ProjectId, name: &str) -> PathBuf {
        self.root.join(project.to_string()).join(name)
    }
}

/// The project's own checkout, under its own id.
fn main(project: &Project, dir: &Path, branch: Option<String>) -> Worktree {
    Worktree {
        id: project.id,
        project_id: project.id,
        main: true,
        name: "Main checkout".to_owned(),
        branch,
        base_commit: None,
        path: dir.to_string_lossy().into_owned(),
        created_at_ms: project.created_at_ms,
    }
}

/// A branch name git accepts.
fn checkout_branch(branch: &str) -> Result<(), WorktreeError> {
    let output = std::process::Command::new("git")
        .args(["check-ref-format", "--branch", branch])
        .env("LC_ALL", "C")
        .output()
        .map_err(|error| WorktreeError::Invalid(format!("git: {error}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(WorktreeError::Invalid(format!("not a branch name: {branch}")))
    }
}

/// The branch checked out in `dir`, when it is on one.
async fn branch_of(dir: &Path) -> Option<String> {
    let output = git(dir, &["symbolic-ref", "--short", "-q", "HEAD"]).await.ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned()).filter(|branch| !branch.is_empty())
}

/// Whether the checkout has anything uncommitted, untracked files included.
async fn dirty(dir: &Path) -> Result<bool, WorktreeError> {
    let output = git(dir, &["status", "--porcelain"]).await?;
    if !output.status.success() {
        return Err(WorktreeError::Invalid(format!("git status: {}", String::from_utf8_lossy(&output.stderr).trim())));
    }
    Ok(!output.stdout.iter().all(u8::is_ascii_whitespace))
}

async fn git(dir: &Path, args: &[&str]) -> Result<Output, WorktreeError> {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .env("LC_ALL", "C")
        .output()
        .await
        .map_err(|error| WorktreeError::Invalid(format!("running git: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_blocking(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn repository(root: &Path) {
        git_blocking(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        git_blocking(root, &["add", "."]);
        git_blocking(root, &["commit", "-q", "-m", "first"]);
    }

    async fn setup() -> (tempfile::TempDir, tempfile::TempDir, Arc<Store>, Project, broadcast::Sender<ServerEvent>) {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(data.path()).unwrap());
        let project = Project {
            id: uuid::Uuid::now_v7(),
            name: "Demo".into(),
            dir_name: "demo".into(),
            path: None,
            state: aiddiction_api::ProjectState::Ready,
            git_url: None,
            status_message: None,
            created_at_ms: now_ms(),
        };
        let dir = root.path().join("demo");
        std::fs::create_dir(&dir).unwrap();
        repository(&dir);
        (root, data, store, project, broadcast::channel(8).0)
    }

    #[tokio::test]
    async fn a_worktree_has_its_own_files_and_branch_and_goes_away_without_its_branch() {
        let (root, _data, store, project, events) = setup().await;
        let dir = root.path().join("demo");
        let worktrees = Worktrees::open(root.path(), store, events).unwrap();
        let listed = worktrees.list_for(&project, &dir).await;
        assert_eq!(listed.len(), 1);
        assert!(listed[0].main);
        assert_eq!(listed[0].branch.as_deref(), Some("main"));

        let made = worktrees.create(&project, &dir, NewWorktree { name: Some("Feature".into()), ..NewWorktree::default() }).await.unwrap();
        assert_eq!(made.name, "feature");
        assert_eq!(made.branch.as_deref(), Some("aiddiction/feature"));
        assert!(!Path::new(&made.path).starts_with(&dir));
        assert!(Path::new(&made.path).join("a.txt").is_file());
        std::fs::write(Path::new(&made.path).join("b.txt"), "two\n").unwrap();
        // The files are the worktree's own: the main checkout has not seen it.
        assert!(std::fs::read_dir(&dir).unwrap().all(|entry| entry.unwrap().file_name() != "b.txt"));

        // Changes stop the removal; the folder stays.
        assert!(matches!(worktrees.remove(&project, &dir, made.id).await, Err(WorktreeError::Dirty)));
        std::fs::remove_file(Path::new(&made.path).join("b.txt")).unwrap();
        worktrees.remove(&project, &dir, made.id).await.unwrap();
        assert!(!Path::new(&made.path).exists());
        assert!(worktrees.known(project.id).is_empty());
        // The branch was kept.
        let output = git(&dir, &["rev-parse", "--verify", "refs/heads/aiddiction/feature"]).await.unwrap();
        assert!(output.status.success());

        // The project's own checkout is not one to remove.
        assert!(matches!(worktrees.remove(&project, &dir, project.id).await, Err(WorktreeError::Main)));
    }

    #[tokio::test]
    async fn a_branch_that_is_there_already_is_refused() {
        let (root, _data, store, project, events) = setup().await;
        let dir = root.path().join("demo");
        let worktrees = Worktrees::open(root.path(), store, events).unwrap();
        let made = worktrees.create(&project, &dir, NewWorktree::default()).await.unwrap();
        let again = worktrees
            .create(&project, &dir, NewWorktree { name: Some("other".into()), branch: made.branch.clone(), ..NewWorktree::default() })
            .await;
        assert!(matches!(again, Err(WorktreeError::Invalid(message)) if message.contains("already exists")));
        worktrees.remove(&project, &dir, made.id).await.unwrap();
    }
}
