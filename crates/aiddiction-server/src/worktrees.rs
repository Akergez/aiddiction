//! The checkouts of a project an agent can work in: its own folder, git
//! worktrees the server makes beside it with a branch of their own, and
//! checkouts the person made themselves, which are only kept under an id
//! (`adopt`) so an agent can be started there.
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
    /// One at a time: a name and a branch are checked, a worktree is made and
    /// its record is written, and two of those must not interleave.
    changing: tokio::sync::Mutex<()>,
}

#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("no such worktree")]
    NotFound,
    #[error("the project's own checkout cannot be updated or removed this way")]
    Main,
    #[error("{0}")]
    Invalid(String),
    #[error("the agent is still working in it")]
    Busy,
    #[error("it has changes; commit them or remove the worktree yourself")]
    Dirty,
    #[error("the checkout still has work; take its results first")]
    Unfinished,
    #[error(transparent)]
    Io(#[from] anyhow::Error),
}

impl Worktrees {
    pub fn open(
        root: &Path,
        store: Arc<Store>,
        events: broadcast::Sender<ServerEvent>,
    ) -> anyhow::Result<Self> {
        let mut list = store.worktrees()?;
        // A folder that is gone is not a checkout any more.
        let before = list.len();
        list.retain(|worktree| Path::new(&worktree.path).is_dir());
        if list.len() != before {
            store.save_worktrees(&list)?;
        }
        Ok(Self {
            root: root.join(".aiddiction-worktrees"),
            store,
            list: Mutex::new(list),
            events,
            changing: tokio::sync::Mutex::new(()),
        })
    }

    /// The checkouts of a project, its own first.
    pub async fn list_for(&self, project: &Project, dir: &Path) -> Vec<Worktree> {
        let mut found = vec![main(project, dir, branch_of(dir).await)];
        let managed: Vec<_> = self
            .list
            .lock()
            .unwrap()
            .iter()
            .filter(|worktree| worktree.project_id == project.id)
            .cloned()
            .collect();
        found.extend(managed);
        found
    }

    /// One checkout by id: the project's own (`project.id`) or one made here.
    pub async fn get(
        &self,
        project: &Project,
        dir: &Path,
        id: WorktreeId,
    ) -> Result<Worktree, WorktreeError> {
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
    pub async fn create(
        &self,
        project: &Project,
        dir: &Path,
        input: NewWorktree,
    ) -> Result<Worktree, WorktreeError> {
        let _changing = self.changing.lock().await;
        let (name, base, base_commit) = self.prepare(project, dir, &input).await?;
        let branch = temp_branch(&base);
        checkout_branch(&branch)?;

        let path = self.path_of(project.id, &name);
        if path.exists() {
            return Err(WorktreeError::Invalid(format!(
                "{} already exists",
                path.display()
            )));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                WorktreeError::Invalid(format!("{}: {error}", parent.display()))
            })?;
        }
        let uses_lfs = require_lfs_if_used(dir).await?;
        let path_text = path.to_string_lossy().into_owned();
        let output = git(
            dir,
            &["worktree", "add", "-b", &branch, &path_text, &base_commit],
        )
        .await
        .map_err(|error| WorktreeError::Invalid(format!("could not make the worktree: {error}")))?;
        if !output.status.success() {
            // A failed checkout can leave a half-made worktree, its admin
            // files and a branch behind; take them away so a retry starts
            // clean. (A Git LFS filter that cannot run fails here.)
            take_away(dir, &path, &branch).await;
            return Err(WorktreeError::Invalid(format!(
                "could not make the worktree: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        // A Git LFS checkout smudges what it can; make sure every LFS object
        // is here so the fresh checkout has no phantom modifications.
        materialize_lfs(&path, uses_lfs).await;
        warn_if_dirty(&path, "worktree").await;

        let worktree = Worktree {
            id: uuid::Uuid::now_v7(),
            project_id: project.id,
            main: false,
            managed: true,
            name,
            branch: Some(branch),
            base_commit: Some(base_commit),
            base: Some(base),
            last_result: None,
            path: path_text,
            created_at_ms: now_ms(),
        };
        // The record and the thing it describes go together: a worktree git
        // made but the store could not keep would be unknown after a restart,
        // so it is taken away again, with the branch it was made on.
        let saved = {
            let mut list = self.list.lock().unwrap();
            let mut next = list.clone();
            next.push(worktree.clone());
            match self.store.save_worktrees(&next) {
                Ok(()) => {
                    *list = next;
                    Ok(())
                }
                Err(error) => Err(error),
            }
        };
        if let Err(error) = saved {
            tracing::warn!(
                "the worktree {} was made but could not be kept; taking it away again: {error:#}",
                worktree.path
            );
            match git(dir, &["worktree", "remove", "--force", &worktree.path]).await {
                Ok(output) if output.status.success() => {}
                Ok(output) => tracing::warn!(
                    "could not take the worktree away: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
                Err(error) => tracing::warn!("could not take the worktree away: {error}"),
            }
            let _ = git(dir, &["worktree", "prune"]).await;
            if let Some(branch) = &worktree.branch {
                match git(dir, &["branch", "-D", branch]).await {
                    Ok(output) if output.status.success() => {}
                    Ok(output) => tracing::warn!(
                        "could not delete the branch {branch}: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    ),
                    Err(error) => tracing::warn!("could not delete the branch {branch}: {error}"),
                }
            }
            return Err(WorktreeError::Io(error));
        }
        let _ = self.events.send(ServerEvent::WorktreesChanged {
            project_id: project.id,
        });
        Ok(worktree)
    }

    /// A name, the base it follows and the commit a new checkout starts
    /// from, checked against the project; both kinds of checkout share them.
    /// The temporary branch is made by the caller, since every round gets a
    /// new one.
    async fn prepare(
        &self,
        project: &Project,
        dir: &Path,
        input: &NewWorktree,
    ) -> Result<(String, String, String), WorktreeError> {
        let name = input
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(|name| slug(name, "worktree"))
            // A random tail, not a timestamp: two auto-named makes in one
            // breath must not collide on the name.
            .unwrap_or_else(|| {
                format!(
                    "worktree-{}",
                    &uuid::Uuid::new_v4().simple().to_string()[..8]
                )
            });
        if self
            .list
            .lock()
            .unwrap()
            .iter()
            .any(|worktree| worktree.project_id == project.id && worktree.name == name)
        {
            return Err(WorktreeError::Invalid(format!(
                "the project already has a worktree called {name}"
            )));
        }
        let base = input
            .base
            .as_deref()
            .map(str::trim)
            .filter(|base| !base.is_empty())
            .unwrap_or("HEAD")
            .to_owned();
        // A caller who names no base means the branch the main checkout is
        // on: that is what moves, where a commit would stay where it is.
        let base_ref = if base == "HEAD" {
            branch_of(dir).await.unwrap_or_else(|| base.clone())
        } else {
            base.clone()
        };
        let base_commit = match git(
            dir,
            &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
        )
        .await
        {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).trim().to_owned()
            }
            _ => {
                return Err(WorktreeError::Invalid(format!(
                    "{base} is not a commit here"
                )));
            }
        };
        Ok((name, base_ref, base_commit))
    }

    /// Makes a clone of the project on a new branch. Unlike a worktree it is
    /// self-contained — its `.git` is its own — so it can be handed to an
    /// agent in a container that never sees the project's checkout. Its
    /// commits reach the project by `get_results`.
    pub async fn create_clone(
        &self,
        project: &Project,
        dir: &Path,
        input: NewWorktree,
    ) -> Result<Worktree, WorktreeError> {
        let _changing = self.changing.lock().await;
        let (name, base, base_commit) = self.prepare(project, dir, &input).await?;
        let branch = temp_branch(&base);
        checkout_branch(&branch)?;
        let uses_lfs = require_lfs_if_used(dir).await?;
        let path = self.path_of(project.id, &name);
        // A folder left by an attempt that failed is not a checkout of ours
        // (a record would have stopped the name above): it goes.
        if path.exists() {
            let _ = std::fs::remove_dir_all(&path);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                WorktreeError::Invalid(format!("{}: {error}", parent.display()))
            })?;
        }
        let path_text = path.to_string_lossy().into_owned();
        // Without a checkout of the default branch: only the base is brought
        // out, so files of other commits (large ones, LFS ones that the
        // remote may not have) do not block the clone.
        let output = git(
            dir,
            &[
                "clone",
                "--local",
                "--quiet",
                "--no-checkout",
                ".",
                &path_text,
            ],
        )
        .await
        .map_err(|error| WorktreeError::Invalid(format!("could not make the clone: {error}")))?;
        if !output.status.success() {
            let _ = std::fs::remove_dir_all(&path);
            return Err(WorktreeError::Invalid(format!(
                "could not make the clone: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        // The clone comes on the default branch; the round's temporary
        // branch is the only one it should have, and the one HEAD is on.
        let default = branch_of(&path).await;
        let switched = git(&path, &["switch", "--quiet", "-c", &branch, &base_commit]).await?;
        if !switched.status.success() {
            let _ = std::fs::remove_dir_all(&path);
            return Err(WorktreeError::Invalid(format!(
                "could not make the branch {branch}: {}",
                String::from_utf8_lossy(&switched.stderr).trim()
            )));
        }
        if let Some(default) = default.filter(|default| default != &branch) {
            let gone = git(&path, &["branch", "-D", &default]).await?;
            if !gone.status.success() {
                let _ = std::fs::remove_dir_all(&path);
                return Err(WorktreeError::Invalid(format!(
                    "could not leave only the temporary branch: {}",
                    String::from_utf8_lossy(&gone.stderr).trim()
                )));
            }
        }
        // A Git LFS checkout smudges what it can; make sure every LFS object
        // is here so the fresh checkout has no phantom modifications. A
        // `clone --local` does not carry the source's repo-local LFS filter
        // config over, so Git LFS is pointed at the source repository's
        // objects through its `origin`.
        materialize_lfs(&path, uses_lfs).await;
        warn_if_dirty(&path, "clone").await;
        let worktree = Worktree {
            id: uuid::Uuid::now_v7(),
            project_id: project.id,
            main: false,
            managed: true,
            name,
            branch: Some(branch),
            base_commit: Some(base_commit),
            base: Some(base),
            last_result: None,
            path: path_text,
            created_at_ms: now_ms(),
        };
        {
            let mut list = self.list.lock().unwrap();
            let mut next = list.clone();
            next.push(worktree.clone());
            match self.store.save_worktrees(&next) {
                Ok(()) => *list = next,
                Err(error) => {
                    let _ = std::fs::remove_dir_all(&worktree.path);
                    return Err(WorktreeError::Io(error));
                }
            }
        }
        let _ = self.events.send(ServerEvent::WorktreesChanged {
            project_id: project.id,
        });
        Ok(worktree)
    }

    /// Whether a checkout is a clone of its own rather than a worktree: its
    /// `.git` is a folder, not a file pointing into the project's checkout.
    pub fn is_clone(worktree: &Worktree) -> bool {
        Path::new(&worktree.path).join(".git").is_dir()
    }

    /// Takes a round's work out of a checkout: the temporary branch goes
    /// into the project's repository, where the orchestrator merges it, and
    /// the checkout is left without a branch, so no stale temporary branches
    /// pile up there. A clone fetches its branch into the project first; a
    /// shared worktree already has the ref there. Nothing is taken while the
    /// checkout has uncommitted changes.
    pub async fn get_results(
        &self,
        project: &Project,
        dir: &Path,
        id: WorktreeId,
    ) -> Result<(String, String), WorktreeError> {
        let _changing = self.changing.lock().await;
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
        let Some(branch) = worktree.branch.clone() else {
            return Err(WorktreeError::Invalid(
                "the checkout has no work to take; start a new round first".into(),
            ));
        };
        let path = PathBuf::from(&worktree.path);
        if !path.is_dir() {
            return Err(WorktreeError::Invalid(format!("{} is gone", worktree.path)));
        }
        if dirty(&path).await? {
            return Err(WorktreeError::Dirty);
        }
        let commit = head_of(&path).await?;
        if Self::is_clone(&worktree) {
            let output = git(
                dir,
                &[
                    "fetch",
                    "--quiet",
                    &worktree.path,
                    &format!("+{branch}:{branch}"),
                ],
            )
            .await?;
            if !output.status.success() {
                return Err(WorktreeError::Invalid(format!(
                    "could not fetch {branch}: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
        }
        // The ref is a result now: the checkout keeps no copy of it.
        let detached = git(&path, &["switch", "--quiet", "--detach", &branch]).await?;
        if !detached.status.success() {
            return Err(WorktreeError::Invalid(format!(
                "could not leave the branch {branch}: {}",
                String::from_utf8_lossy(&detached.stderr).trim()
            )));
        }
        if Self::is_clone(&worktree) {
            let gone = git(&path, &["branch", "-D", &branch]).await?;
            if !gone.status.success() {
                return Err(WorktreeError::Invalid(format!(
                    "could not take the branch {branch} out of the clone: {}",
                    String::from_utf8_lossy(&gone.stderr).trim()
                )));
            }
        }
        let mut updated = worktree;
        updated.branch = None;
        updated.last_result = Some(branch.clone());
        self.keep(updated)?;
        let _ = self.events.send(ServerEvent::WorktreesChanged {
            project_id: project.id,
        });
        Ok((branch, commit))
    }

    /// Starts a new round: a fresh temporary branch from the base as the
    /// main checkout has it now. The previous round must have been taken by
    /// `get_results` first, and the checkout must have nothing uncommitted.
    pub async fn start_new_work(
        &self,
        project: &Project,
        dir: &Path,
        id: WorktreeId,
        base: Option<&str>,
    ) -> Result<(String, String, String), WorktreeError> {
        let _changing = self.changing.lock().await;
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
        if worktree.branch.is_some() {
            return Err(WorktreeError::Unfinished);
        }
        let path = PathBuf::from(&worktree.path);
        if !path.is_dir() {
            return Err(WorktreeError::Invalid(format!("{} is gone", worktree.path)));
        }
        if dirty(&path).await? {
            return Err(WorktreeError::Dirty);
        }
        let named = base
            .map(str::trim)
            .filter(|base| !base.is_empty())
            .map(str::to_owned)
            .or_else(|| worktree.base.clone());
        let base = match named {
            Some(base) => base,
            None => branch_of(dir).await.ok_or_else(|| {
                WorktreeError::Invalid(
                    "the main checkout is on no branch; name a base to start from".into(),
                )
            })?,
        };
        let base_commit = match git(
            dir,
            &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
        )
        .await
        {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).trim().to_owned()
            }
            _ => {
                return Err(WorktreeError::Invalid(format!(
                    "{base} is not a commit in the project"
                )));
            }
        };
        let source = if Self::is_clone(&worktree) {
            // The clone's own repository may not have the base branch; the
            // project's checkout does.
            let output = git(&path, &["fetch", "--quiet", &dir.to_string_lossy(), &base]).await?;
            if !output.status.success() {
                return Err(WorktreeError::Invalid(format!(
                    "could not fetch {base} from the project: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            "FETCH_HEAD".to_owned()
        } else {
            base.clone()
        };
        let branch = temp_branch(&base);
        checkout_branch(&branch)?;
        let switched = git(&path, &["switch", "--quiet", "-c", &branch, &source]).await?;
        if !switched.status.success() {
            return Err(WorktreeError::Invalid(format!(
                "could not make the branch {branch}: {}",
                String::from_utf8_lossy(&switched.stderr).trim()
            )));
        }
        let mut updated = worktree;
        updated.branch = Some(branch.clone());
        updated.base = Some(base.clone());
        updated.base_commit = Some(base_commit.clone());
        self.keep(updated)?;
        let _ = self.events.send(ServerEvent::WorktreesChanged {
            project_id: project.id,
        });
        Ok((branch, base, base_commit))
    }

    /// Replaces a record with the one it became, keeping the list as a whole
    /// only when it could be saved.
    fn keep(&self, worktree: Worktree) -> Result<(), WorktreeError> {
        let mut list = self.list.lock().unwrap();
        let mut next = list.clone();
        let Some(entry) = next.iter_mut().find(|entry| entry.id == worktree.id) else {
            return Err(WorktreeError::NotFound);
        };
        *entry = worktree;
        self.store
            .save_worktrees(&next)
            .map_err(WorktreeError::Io)?;
        *list = next;
        Ok(())
    }

    /// A checkout the person made — a git worktree of this project's
    /// repository — kept under an id of its own so an agent can be started
    /// there. Nothing is made: the folder is only checked, and the server
    /// never takes it away (`remove` forgets the id and leaves it).
    pub async fn adopt(
        &self,
        project: &Project,
        dir: &Path,
        path: &str,
    ) -> Result<Worktree, WorktreeError> {
        let _changing = self.changing.lock().await;
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| !home.as_os_str().is_empty());
        let given = expand_home(Path::new(path.trim()), home.as_deref());
        let wanted = if given.is_absolute() {
            given
        } else {
            dir.join(given)
        };
        let wanted = std::fs::canonicalize(&wanted)
            .map_err(|error| WorktreeError::Invalid(format!("{}: {error}", wanted.display())))?;
        let main_dir = std::fs::canonicalize(dir)
            .map_err(|error| WorktreeError::Invalid(format!("{}: {error}", dir.display())))?;
        // The project's own checkout is already a worktree, under its id.
        if wanted == main_dir {
            return Ok(main(project, dir, branch_of(dir).await));
        }
        if top_level(&wanted).await? != wanted {
            return Err(WorktreeError::Invalid(format!(
                "{} is not a checkout of its own",
                wanted.display()
            )));
        }
        if common_dir(&wanted).await? != common_dir(&main_dir).await? {
            return Err(WorktreeError::Invalid(format!(
                "{} belongs to another repository",
                wanted.display()
            )));
        }
        // A second call for the same folder is the same checkout.
        let already = self
            .list
            .lock()
            .unwrap()
            .iter()
            .filter(|worktree| worktree.project_id == project.id)
            .find(|worktree| same_path(&worktree.path, &wanted))
            .cloned();
        if let Some(worktree) = already {
            return Ok(worktree);
        }
        let base = wanted
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "checkout".to_owned());
        let name = {
            let list = self.list.lock().unwrap();
            let mut name = base.clone();
            let mut n = 2;
            while list
                .iter()
                .any(|worktree| worktree.project_id == project.id && worktree.name == name)
            {
                name = format!("{base}-{n}");
                n += 1;
            }
            name
        };
        let worktree = Worktree {
            id: uuid::Uuid::now_v7(),
            project_id: project.id,
            main: false,
            managed: false,
            name,
            branch: branch_of(&wanted).await,
            base_commit: None,
            base: None,
            last_result: None,
            path: wanted.to_string_lossy().into_owned(),
            created_at_ms: now_ms(),
        };
        {
            let mut list = self.list.lock().unwrap();
            let mut next = list.clone();
            next.push(worktree.clone());
            if let Err(error) = self.store.save_worktrees(&next) {
                return Err(WorktreeError::Io(error));
            }
            *list = next;
        }
        let _ = self.events.send(ServerEvent::WorktreesChanged {
            project_id: project.id,
        });
        Ok(worktree)
    }

    /// Removes a worktree: one the server made goes away with its folder,
    /// one it only adopted keeps its folder and branch, and only the id is
    /// forgotten. A branch the server made stays either way.
    pub async fn remove(
        &self,
        project: &Project,
        dir: &Path,
        id: WorktreeId,
    ) -> Result<(), WorktreeError> {
        let _changing = self.changing.lock().await;
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
        if !worktree.managed {
            // The person's own checkout: the id goes, the folder and its
            // branch are left as they are.
            self.forget(&[worktree.id]);
            let _ = self.events.send(ServerEvent::WorktreesChanged {
                project_id: project.id,
            });
            return Ok(());
        }
        if Self::is_clone(&worktree) {
            // A clone is the server's own as well: its folder goes, and the
            // project keeps whatever was fetched from it. Until the round's
            // results are taken, the branch lives only in the clone, so the
            // folder is not taken with it.
            if worktree.branch.is_some() {
                return Err(WorktreeError::Unfinished);
            }
            let path = PathBuf::from(&worktree.path);
            if path.is_dir() {
                std::fs::remove_dir_all(&path).map_err(|error| {
                    WorktreeError::Invalid(format!("{}: {error}", path.display()))
                })?;
            }
            self.forget(&[worktree.id]);
            let _ = self.events.send(ServerEvent::WorktreesChanged {
                project_id: project.id,
            });
            return Ok(());
        }
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
        let _ = self.events.send(ServerEvent::WorktreesChanged {
            project_id: project.id,
        });
        Ok(())
    }

    /// Forgets a project's worktrees and takes their folders away: the
    /// project is being deleted, and these are the server's own.
    pub async fn remove_project(&self, project: &Project, dir: &Path) {
        let _changing = self.changing.lock().await;
        let taken = self.known(project.id);
        for worktree in taken.iter().filter(|worktree| worktree.managed) {
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
        self.list
            .lock()
            .unwrap()
            .iter()
            .filter(|worktree| worktree.project_id == project)
            .cloned()
            .collect()
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
        managed: false,
        name: "Main checkout".to_owned(),
        branch,
        base_commit: None,
        base: None,
        last_result: None,
        path: dir.to_string_lossy().into_owned(),
        created_at_ms: project.created_at_ms,
    }
}

/// The temporary branch of one round: the base it starts from and an id of
/// its own, so results of earlier rounds keep their names in the project.
fn temp_branch(base: &str) -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    format!("aiddiction/{}-{}", slug(base, "base"), &id[..8])
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
        Err(WorktreeError::Invalid(format!(
            "not a branch name: {branch}"
        )))
    }
}

/// `~` and `~/…` are the home of whoever runs the server, as a shell would
/// read them; any other path is left as it is.
fn expand_home(path: &Path, home: Option<&Path>) -> PathBuf {
    let Some(rest) = path.to_str().and_then(|text| text.strip_prefix('~')) else {
        return path.to_path_buf();
    };
    if !(rest.is_empty() || rest.starts_with('/')) {
        return path.to_path_buf();
    }
    let Some(home) = home else {
        return path.to_path_buf();
    };
    let mut expanded = home.to_path_buf();
    if !rest.is_empty() {
        expanded.push(rest.trim_start_matches('/'));
    }
    expanded
}

/// The folder a checkout's repository tops out at, as git sees it: a
/// subfolder of a checkout is not a checkout of its own.
async fn top_level(dir: &Path) -> Result<PathBuf, WorktreeError> {
    let output = git(dir, &["rev-parse", "--show-toplevel"]).await?;
    if !output.status.success() {
        return Err(WorktreeError::Invalid(format!(
            "{} is not a git checkout",
            dir.display()
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    std::fs::canonicalize(&text).map_err(|error| WorktreeError::Invalid(format!("{text}: {error}")))
}

/// The `.git` a checkout shares with the rest of its repository.
async fn common_dir(dir: &Path) -> Result<PathBuf, WorktreeError> {
    let output = git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .await?;
    if !output.status.success() {
        return Err(WorktreeError::Invalid(format!(
            "{} is not a git checkout",
            dir.display()
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    std::fs::canonicalize(&text).map_err(|error| WorktreeError::Invalid(format!("{text}: {error}")))
}

/// Whether a folder is the one a record names, symlinks aside.
fn same_path(recorded: &str, wanted: &Path) -> bool {
    match std::fs::canonicalize(recorded) {
        Ok(path) => path == wanted,
        Err(_) => Path::new(recorded) == wanted,
    }
}

/// The branch checked out in `dir`, when it is on one.
async fn branch_of(dir: &Path) -> Option<String> {
    let output = git(dir, &["symbolic-ref", "--short", "-q", "HEAD"])
        .await
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|branch| !branch.is_empty())
}

/// The commit checked out in `dir`.
async fn head_of(dir: &Path) -> Result<String, WorktreeError> {
    let output = git(dir, &["rev-parse", "HEAD"]).await?;
    if !output.status.success() {
        return Err(WorktreeError::Invalid(format!(
            "git rev-parse HEAD: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// A project that needs Git LFS can't make a sound checkout without it: a
/// missing `git-lfs` makes Git's `filter.lfs.*` hooks fail, which either
/// stops the checkout or leaves LFS pointers and phantom modifications. When
/// the project has no LFS files, nothing changes. Returns whether it uses
/// LFS, so the caller can restore LFS content afterwards.
async fn require_lfs_if_used(dir: &Path) -> Result<bool, WorktreeError> {
    let uses_lfs = repo_uses_lfs(dir).await;
    if uses_lfs && !git_lfs_available().await {
        return Err(WorktreeError::Invalid(
            "the project uses Git LFS, but git-lfs is not installed; \
             install git-lfs and run `git lfs install`, then try again"
                .into(),
        ));
    }
    Ok(uses_lfs)
}

/// Whether any tracked file is marked `filter=lfs` in a `.gitattributes`
/// (root, nested, or `.git/info/attributes`).
async fn repo_uses_lfs(dir: &Path) -> bool {
    let tracked = match git(dir, &["ls-files", "-z"]).await {
        Ok(output) if output.status.success() => output.stdout,
        _ => return false,
    };
    // `git check-attr` applies `.gitattributes` and `.git/info/attributes`;
    // attribute matching doesn't need the files themselves to exist.
    let names: Vec<&str> = tracked
        .split(|byte| *byte == 0)
        .filter_map(|name| std::str::from_utf8(name).ok())
        .filter(|name| !name.is_empty())
        .collect();
    if names.is_empty() {
        return false;
    }
    let mut query = String::new();
    for name in &names {
        query.push_str(name);
        query.push('\0');
    }
    let mut command = Command::new("git");
    command
        .args(["check-attr", "-z", "filter", "--stdin"])
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(query.as_bytes()).await;
        let _ = stdin.shutdown().await;
    }
    let Ok(output) = child.wait_with_output().await else {
        return false;
    };
    // Triples `<path>\0filter\0<value>\0`; `lfs` (not `unspecified`, `unset`,
    // `set`, or another filter's name) means a Git LFS file.
    output
        .stdout
        .split(|byte| *byte == 0)
        .collect::<Vec<_>>()
        .chunks(3)
        .any(|triple| triple.get(2).is_some_and(|value| value == b"lfs"))
}

/// `git-lfs` on PATH (`git lfs version` succeeds).
async fn git_lfs_available() -> bool {
    git(Path::new("."), &["lfs", "version"])
        .await
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Pulls every Git LFS object into a fresh checkout so it has real contents,
/// not pointers, and nothing shows as modified. Best effort: a failed pull
/// is reported, not fatal — the checkout itself is sound.
async fn materialize_lfs(checkout: &Path, uses_lfs: bool) {
    if !uses_lfs || !git_lfs_available().await {
        return;
    }
    // `git lfs pull` skips object checkout unless Git LFS is installed for
    // the repository. A `clone --local` does not inherit repo-local filter
    // config, so make sure it is. `--local` keeps this checkout's settings
    // out of the person's global gitconfig. Best effort: a failed pull is
    // reported, not fatal — the checkout itself is sound.
    match git(checkout, &["lfs", "install", "--local"]).await {
        Ok(output) if output.status.success() => {}
        Ok(output) => tracing::warn!(
            "git lfs install in {} did not succeed: {}",
            checkout.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(error) => tracing::warn!(
            "git lfs install in {} did not succeed: {error}",
            checkout.display()
        ),
    }
    match git(checkout, &["lfs", "pull"]).await {
        Ok(output) if output.status.success() => {}
        Ok(output) => tracing::warn!(
            "git lfs pull in {} did not succeed: {}",
            checkout.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(error) => tracing::warn!(
            "git lfs pull in {} did not succeed: {error}",
            checkout.display()
        ),
    }
}

/// A fresh checkout should have nothing uncommitted; if anything is (Git LFS
/// prints "should have been a pointer, but wasn't" for images committed
/// without going through git-lfs), say so plainly instead of handing back a
/// dirty checkout whose results would be refused later.
async fn warn_if_dirty(checkout: &Path, kind: &str) {
    match dirty(checkout).await {
        Ok(false) => {}
        Ok(true) => tracing::warn!(
            "the {kind} {} is not clean after creation; \
             `git status` shows changes (see `git lfs ls-files`; \
             images marked filter=lfs but committed as raw binary need \
             `git lfs migrate import` / `git add --renormalize`, \
             and git-lfs needs to be installed and `git lfs install`ed)",
            checkout.display()
        ),
        Err(error) => tracing::warn!(
            "could not check the {kind} {} is clean: {error}",
            checkout.display()
        ),
    }
}

/// Takes a partial checkout away again: its worktree admin files, its
/// folder, and the branch `git worktree add -b` may have made.
async fn take_away(dir: &Path, path: &Path, branch: &str) {
    let path_text = path.to_string_lossy().into_owned();
    match git(dir, &["worktree", "remove", "--force", &path_text]).await {
        Ok(output) if output.status.success() => {}
        Ok(output) => tracing::warn!(
            "could not take the worktree away: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(error) => tracing::warn!("could not take the worktree away: {error}"),
    }
    let _ = git(dir, &["worktree", "prune"]).await;
    match git(dir, &["branch", "-D", branch]).await {
        Ok(output) if output.status.success() => {}
        Ok(output) => tracing::warn!(
            "could not delete the branch {branch}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(error) => tracing::warn!("could not delete the branch {branch}: {error}"),
    }
    let _ = std::fs::remove_dir_all(path);
}

/// Whether the checkout has anything uncommitted, untracked files included.
async fn dirty(dir: &Path) -> Result<bool, WorktreeError> {
    let output = git(dir, &["status", "--porcelain"]).await?;
    if !output.status.success() {
        return Err(WorktreeError::Invalid(format!(
            "git status: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
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

    async fn setup() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        Arc<Store>,
        Project,
        broadcast::Sender<ServerEvent>,
    ) {
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

        let made = worktrees
            .create(
                &project,
                &dir,
                NewWorktree {
                    name: Some("Feature".into()),
                    ..NewWorktree::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(made.name, "feature");
        let branch = made.branch.clone().unwrap();
        assert!(branch.starts_with("aiddiction/main-"), "{branch}");
        assert!(!Path::new(&made.path).starts_with(&dir));
        assert!(Path::new(&made.path).join("a.txt").is_file());
        std::fs::write(Path::new(&made.path).join("b.txt"), "two\n").unwrap();
        // The files are the worktree's own: the main checkout has not seen it.
        assert!(
            std::fs::read_dir(&dir)
                .unwrap()
                .all(|entry| entry.unwrap().file_name() != "b.txt")
        );

        // Changes stop the removal; the folder stays.
        assert!(matches!(
            worktrees.remove(&project, &dir, made.id).await,
            Err(WorktreeError::Dirty)
        ));
        std::fs::remove_file(Path::new(&made.path).join("b.txt")).unwrap();
        worktrees.remove(&project, &dir, made.id).await.unwrap();
        assert!(!Path::new(&made.path).exists());
        assert!(worktrees.known(project.id).is_empty());
        // The branch was kept.
        let output = git(
            &dir,
            &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
        )
        .await
        .unwrap();
        assert!(output.status.success());

        // The project's own checkout is not one to remove.
        assert!(matches!(
            worktrees.remove(&project, &dir, project.id).await,
            Err(WorktreeError::Main)
        ));
    }

    #[tokio::test]
    async fn a_round_goes_to_the_project_and_a_new_one_starts_from_the_base() {
        let (root, _data, store, project, events) = setup().await;
        let dir = root.path().join("demo");
        let worktrees = Worktrees::open(root.path(), store, events).unwrap();
        let made = worktrees
            .create_clone(
                &project,
                &dir,
                NewWorktree {
                    name: Some("rounds".into()),
                    ..NewWorktree::default()
                },
            )
            .await
            .unwrap();
        let first = made.branch.clone().unwrap();
        let path = Path::new(&made.path);
        assert!(first.starts_with("aiddiction/main-"), "{first}");
        assert_eq!(branch_of(path).await.as_deref(), Some(first.as_str()));
        // The clone has no other branch: the round's is the only one.
        let branches = git(
            path,
            &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
        )
        .await
        .unwrap();
        assert_eq!(String::from_utf8_lossy(&branches.stdout).trim(), first);

        std::fs::write(path.join("b.txt"), "two\n").unwrap();
        git_blocking(path, &["add", "."]);
        git_blocking(path, &["commit", "-q", "-m", "round one"]);

        // The branch reaches the project, the clone keeps none of it.
        let (taken, commit) = worktrees
            .get_results(&project, &dir, made.id)
            .await
            .unwrap();
        assert_eq!(taken, first);
        assert_eq!(head_of(path).await.unwrap(), commit);
        assert_eq!(branch_of(path).await, None);
        assert!(
            git(
                &dir,
                &["rev-parse", "--verify", &format!("refs/heads/{first}")]
            )
            .await
            .unwrap()
            .status
            .success(),
            "the project has the round's branch"
        );
        let in_clone = git(
            path,
            &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
        )
        .await
        .unwrap();
        assert!(String::from_utf8_lossy(&in_clone.stdout).trim().is_empty());
        let after = worktrees.get(&project, &dir, made.id).await.unwrap();
        assert_eq!(after.branch, None);
        assert_eq!(after.last_result.as_deref(), Some(first.as_str()));
        // Nothing left to take twice.
        assert!(matches!(
            worktrees.get_results(&project, &dir, made.id).await,
            Err(WorktreeError::Invalid(_))
        ));

        // The orchestrator merges the result; a new round starts from that.
        git_blocking(&dir, &["merge", "--no-ff", "--no-edit", &first]);
        let (second, base, base_commit) = worktrees
            .start_new_work(&project, &dir, made.id, None)
            .await
            .unwrap();
        assert!(second.starts_with("aiddiction/main-"), "{second}");
        assert_ne!(second, first);
        assert_eq!(base, "main");
        assert_eq!(head_of(path).await.unwrap(), base_commit);
        assert_eq!(branch_of(path).await.as_deref(), Some(second.as_str()));
        assert!(path.join("b.txt").is_file(), "the new round has the merge");

        // A round is in progress: neither another one nor removal.
        assert!(matches!(
            worktrees
                .start_new_work(&project, &dir, made.id, None)
                .await,
            Err(WorktreeError::Unfinished)
        ));
        assert!(matches!(
            worktrees.remove(&project, &dir, made.id).await,
            Err(WorktreeError::Unfinished)
        ));
    }

    #[test]
    fn a_tilde_is_the_home_of_whoever_runs_the_server() {
        let home = Path::new("/home/dev");
        assert_eq!(
            expand_home(Path::new("~/worktrees/edi"), Some(home)),
            PathBuf::from("/home/dev/worktrees/edi")
        );
        assert_eq!(
            expand_home(Path::new("~"), Some(home)),
            PathBuf::from("/home/dev")
        );
        // Another user's home, an absolute path and a relative one are left
        // as they are; so is a tilde with no home to put in its place.
        assert_eq!(
            expand_home(Path::new("~dev/edi"), Some(home)),
            PathBuf::from("~dev/edi")
        );
        assert_eq!(
            expand_home(Path::new("/srv/edi"), Some(home)),
            PathBuf::from("/srv/edi")
        );
        assert_eq!(
            expand_home(Path::new("edi/wt"), Some(home)),
            PathBuf::from("edi/wt")
        );
        assert_eq!(
            expand_home(Path::new("~/edi"), None),
            PathBuf::from("~/edi")
        );
    }

    #[tokio::test]
    async fn a_checkout_made_by_hand_is_kept_under_an_id_and_left_alone() {
        let (root, _data, store, project, events) = setup().await;
        let dir = root.path().join("demo");
        let worktrees = Worktrees::open(root.path(), store, events).unwrap();
        let place = tempfile::tempdir().unwrap();
        let path = place.path().join("coder");
        let text = path.to_str().unwrap();
        assert!(
            git(
                &dir,
                &["worktree", "add", "-q", "-b", "coder", text, "HEAD"]
            )
            .await
            .unwrap()
            .status
            .success()
        );

        // The project's own checkout is the main one, whatever path is given
        // for it.
        let main = worktrees
            .adopt(&project, &dir, dir.to_str().unwrap())
            .await
            .unwrap();
        assert_eq!(main.id, project.id);
        assert!(main.main);
        assert!(!main.managed);

        let adopted = worktrees.adopt(&project, &dir, text).await.unwrap();
        assert_eq!(adopted.project_id, project.id);
        assert!(!adopted.main);
        assert!(!adopted.managed);
        assert_eq!(adopted.name, "coder");
        assert_eq!(adopted.branch.as_deref(), Some("coder"));
        assert_eq!(
            std::fs::canonicalize(&path).unwrap().to_string_lossy(),
            adopted.path
        );
        // The same folder is the same checkout, not a second one.
        let again = worktrees.adopt(&project, &dir, text).await.unwrap();
        assert_eq!(again.id, adopted.id);
        assert_eq!(worktrees.known(project.id).len(), 1);
        assert_eq!(worktrees.list_for(&project, &dir).await.len(), 2);

        // A folder that is not a checkout, and a folder inside one, are no
        // checkouts of their own.
        let plain = worktrees
            .adopt(&project, &dir, place.path().to_str().unwrap())
            .await;
        assert!(
            matches!(plain, Err(WorktreeError::Invalid(message)) if message.contains("git checkout"))
        );
        std::fs::create_dir(path.join("inside")).unwrap();
        let inside = worktrees
            .adopt(&project, &dir, path.join("inside").to_str().unwrap())
            .await;
        assert!(
            matches!(inside, Err(WorktreeError::Invalid(message)) if message.contains("of its own"))
        );
        std::fs::remove_dir(path.join("inside")).unwrap();

        // Removing it forgets the id; the folder and the branch stay.
        worktrees.remove(&project, &dir, adopted.id).await.unwrap();
        assert!(path.is_dir());
        assert!(worktrees.known(project.id).is_empty());
        assert!(
            git(&dir, &["rev-parse", "--verify", "refs/heads/coder"])
                .await
                .unwrap()
                .status
                .success()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn two_makes_of_one_name_leave_one_worktree() {
        let (root, _data, store, project, events) = setup().await;
        let dir = root.path().join("demo");
        let worktrees = Worktrees::open(root.path(), store, events).unwrap();
        let make = || {
            worktrees.create(
                &project,
                &dir,
                NewWorktree {
                    name: Some("same".into()),
                    ..NewWorktree::default()
                },
            )
        };
        let (one, two) = tokio::join!(make(), make());
        assert!(one.is_ok() ^ two.is_ok(), "{one:?} {two:?}");
        assert_eq!(worktrees.known(project.id).len(), 1);
        let made = one.or(two).unwrap();
        assert_eq!(made.name, "same");
        worktrees.remove(&project, &dir, made.id).await.unwrap();
    }

    #[tokio::test]
    async fn a_worktree_whose_record_cannot_be_kept_is_taken_away_again() {
        let (root, data, store, project, events) = setup().await;
        let dir = root.path().join("demo");
        let worktrees = Worktrees::open(root.path(), store, events).unwrap();

        // The store cannot write any more: its folder is a file now.
        let data_path = data.path().to_path_buf();
        std::fs::remove_dir_all(&data_path).unwrap();
        std::fs::write(&data_path, b"not a folder").unwrap();

        let made = worktrees
            .create(
                &project,
                &dir,
                NewWorktree {
                    name: Some("doomed".into()),
                    ..NewWorktree::default()
                },
            )
            .await;
        assert!(made.is_err());
        assert!(
            worktrees.known(project.id).is_empty(),
            "no record is left behind"
        );
        let path = root
            .path()
            .join(".aiddiction-worktrees")
            .join(project.id.to_string())
            .join("doomed");
        assert!(!path.exists(), "the checkout is taken away");
        let branch = git(
            &dir,
            &["rev-parse", "--verify", "refs/heads/aiddiction/doomed"],
        )
        .await
        .unwrap();
        assert!(!branch.status.success(), "the branch is taken away too");
    }

    /// A project marked `filter=lfs` in `.gitattributes` needs `git-lfs`;
    /// without it no sound checkout can be made, and nothing is left behind.
    /// (No `git-lfs` here: the attribute alone is enough to see the repo
    /// uses LFS.) Both kinds of checkout refuse.
    #[tokio::test]
    async fn an_lfs_project_without_git_lfs_makes_no_checkout_and_leaves_nothing() {
        let (root, _data, store, project, events) = setup().await;
        let dir = root.path().join("demo");
        // This is the no-`git-lfs` case; skipped where git-lfs is on PATH.
        if git_lfs_available().await {
            eprintln!("git-lfs on PATH; skipping");
            return;
        }
        std::fs::write(
            dir.join(".gitattributes"),
            "*.png filter=lfs diff=lfs merge=lfs -text\n",
        )
        .unwrap();
        // A PNG; without git-lfs it is committed as a raw binary.
        std::fs::write(dir.join("pic.png"), [0x89, b'P', b'N', b'G', 1, 2, 3, 4, 5]).unwrap();
        git_blocking(&dir, &["add", "."]);
        git_blocking(&dir, &["commit", "-q", "-m", "lfs attributes"]);
        let worktrees = Worktrees::open(root.path(), store, events).unwrap();

        assert!(repo_uses_lfs(&dir).await);
        for made in [
            worktrees
                .create(&project, &dir, NewWorktree::default())
                .await,
            worktrees
                .create_clone(&project, &dir, NewWorktree::default())
                .await,
        ] {
            let Err(WorktreeError::Invalid(message)) = made else {
                panic!("an LFS project without git-lfs cannot make a checkout: {made:?}");
            };
            assert!(message.contains("git-lfs"), "{message}");
            assert!(message.contains("Git LFS"), "{message}");
        }
        // Nothing was made: no checkout, no branch, nothing half done.
        assert!(worktrees.known(project.id).is_empty());
        let listed = git(&dir, &["worktree", "list"]).await.unwrap();
        assert_eq!(
            String::from_utf8_lossy(&listed.stdout).lines().count(),
            1,
            "only the main checkout"
        );
        let branches = git(
            &dir,
            &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
        )
        .await
        .unwrap();
        assert_eq!(String::from_utf8_lossy(&branches.stdout).trim(), "main");
        let made_root = root
            .path()
            .join(".aiddiction-worktrees")
            .join(project.id.to_string());
        assert!(!made_root.exists() || std::fs::read_dir(&made_root).unwrap().next().is_none());
    }

    /// With `git-lfs` on PATH, a project using Git LFS gets a checkout with
    /// real image contents (not pointers) and nothing modified, both kinds.
    /// Skipped where `git-lfs` is not installed.
    #[tokio::test]
    async fn an_lfs_project_gets_real_images_and_a_clean_checkout_with_git_lfs() {
        let (root, _data, store, project, events) = setup().await;
        let dir = root.path().join("demo");
        if !git_lfs_available().await {
            eprintln!("git-lfs not on PATH; skipping");
            return;
        }
        git_blocking(&dir, &["lfs", "install", "--local"]);
        std::fs::write(
            dir.join(".gitattributes"),
            "*.png filter=lfs diff=lfs merge=lfs -text\n",
        )
        .unwrap();
        let png = [
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 42, 42, 42, 42,
        ];
        std::fs::write(dir.join("pic.png"), png).unwrap();
        git_blocking(&dir, &["add", "."]);
        git_blocking(&dir, &["commit", "-q", "-m", "lfs image"]);
        assert!(repo_uses_lfs(&dir).await);
        let worktrees = Worktrees::open(root.path(), store, events).unwrap();

        for made in [
            worktrees
                .create(&project, &dir, NewWorktree::default())
                .await
                .unwrap(),
            worktrees
                .create_clone(&project, &dir, NewWorktree::default())
                .await
                .unwrap(),
        ] {
            let path = Path::new(&made.path);
            let image = std::fs::read(path.join("pic.png")).unwrap();
            assert_eq!(
                image, png,
                "real image contents, not an LFS pointer, in {}",
                made.path
            );
            assert!(
                !dirty(path).await.unwrap(),
                "nothing modified in {}",
                made.path
            );
        }
    }

    /// `git worktree add` failing part way (here, a file sits where git wants
    /// its worktree admin directory) leaves no checkout, admin files or
    /// branch behind, so a retry is not blocked.
    #[tokio::test]
    async fn a_worktree_add_that_fails_leaves_nothing_behind() {
        let (root, _data, store, project, events) = setup().await;
        let dir = root.path().join("demo");
        let worktrees = Worktrees::open(root.path(), store, events).unwrap();
        let name = "doomed";
        // A file sits where git wants its `.git/worktrees` admin directory.
        std::fs::write(dir.join(".git").join("worktrees"), b"not a directory").unwrap();

        let made = worktrees
            .create(
                &project,
                &dir,
                NewWorktree {
                    name: Some(name.into()),
                    ..NewWorktree::default()
                },
            )
            .await;
        assert!(made.is_err(), "{made:?}");
        let path = root
            .path()
            .join(".aiddiction-worktrees")
            .join(project.id.to_string())
            .join(name);
        assert!(!path.exists(), "no checkout is left behind");
        let branches = git(
            &dir,
            &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
        )
        .await
        .unwrap();
        let branches = String::from_utf8_lossy(&branches.stdout);
        assert!(
            branches
                .lines()
                .all(|branch| !branch.starts_with("aiddiction/")),
            "no branch is left behind: {branches}"
        );
    }
}
