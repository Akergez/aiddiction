//! git, run in a working tree on this machine: what the standalone tool uses
//! directly and what Aiddiction's server runs in a project for its clients.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::source::{DiffRequest, Refs};

/// A diff larger than this is refused rather than sent to an interface that
/// would have to draw it.
pub const MAX_DIFF_BYTES: usize = 32 << 20;

pub struct LocalRepo {
    root: PathBuf,
}

impl LocalRepo {
    /// The repository `dir` is in.
    pub fn open(dir: &Path) -> Result<Self, String> {
        let output = git(dir, &["rev-parse", "--show-toplevel"])?;
        let root = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !output.status.success() || root.is_empty() {
            return Err(format!("{} is not in a git repository", dir.display()));
        }
        Ok(Self { root: PathBuf::from(root) })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The unified diff `request` asks for.
    pub fn diff(&self, request: &DiffRequest) -> Result<String, String> {
        request.source.validate()?;
        let mut args = vec!["diff".to_owned()];
        args.extend(request.git_args());
        let output = git(&self.root, &args.iter().map(String::as_str).collect::<Vec<_>>())?;
        if !output.status.success() {
            // A repository with no commit yet has no HEAD to diff against:
            // everything there is is new.
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !(stderr.contains("bad revision 'HEAD'") || stderr.contains("ambiguous argument 'HEAD'")) {
                return Err(stderr.trim().to_owned());
            }
        }
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        if request.includes_untracked() {
            for path in self.untracked()? {
                text.push_str(&self.new_file_diff(&path, request.context)?);
                if text.len() > MAX_DIFF_BYTES {
                    break;
                }
            }
        }
        if text.len() > MAX_DIFF_BYTES {
            return Err(format!("the diff is larger than {} MB", MAX_DIFF_BYTES >> 20));
        }
        Ok(text)
    }

    fn untracked(&self) -> Result<Vec<String>, String> {
        let output = git(&self.root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
        Ok(output.stdout.split(|byte| *byte == 0).filter(|path| !path.is_empty()).map(|path| String::from_utf8_lossy(path).into_owned()).collect())
    }

    /// An untracked file as a diff adding it.
    fn new_file_diff(&self, path: &str, context: Option<u32>) -> Result<String, String> {
        let unified = context.map_or("-U1000000".to_owned(), |lines| format!("-U{lines}"));
        let output = git(&self.root, &["diff", "--no-color", "--no-ext-diff", &unified, "--no-index", "--", "/dev/null", path])?;
        // `--no-index` exits 1 when the files differ, which they always do.
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Branches and the last commits, for a picker.
    pub fn refs(&self) -> Result<Refs, String> {
        let lines = |args: &[&str]| -> Result<Vec<String>, String> {
            let output = git(&self.root, args)?;
            Ok(String::from_utf8_lossy(&output.stdout).lines().map(str::to_owned).filter(|line| !line.is_empty()).collect())
        };
        let branches = lines(&["for-each-ref", "--format=%(refname:short)", "refs/heads"])?;
        let current_branch = lines(&["symbolic-ref", "--short", "-q", "HEAD"])?.into_iter().next();
        let default_branch = ["main", "master"].into_iter().find(|name| branches.iter().any(|branch| branch == name)).map(str::to_owned);
        let commits = lines(&["log", "-n", "30", "--format=%h%x09%s"])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|line| line.split_once('\t').map(|(hash, subject)| (hash.to_owned(), subject.to_owned())))
            .collect();
        Ok(Refs { current_branch, default_branch, branches, commits })
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<Output, String> {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        // Never wait for a person, never page, never translate messages that
        // are matched on above.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .env("LC_ALL", "C")
        .output()
        .map_err(|error| format!("running git: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{FileStatus, parse};
    use crate::source::DiffSource;

    fn run(dir: &Path, args: &[&str]) {
        let status = Command::new("git").args(args).current_dir(dir).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").status().unwrap();
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn uncommitted_staged_untracked_and_commits() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        run(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        run(root, &["add", "."]);
        run(root, &["commit", "-q", "-m", "first"]);
        std::fs::write(root.join("a.txt"), "one\n2\nthree\n").unwrap();
        std::fs::write(root.join("new.txt"), "fresh\n").unwrap();

        let repo = LocalRepo::open(root).unwrap();
        let files = parse(&repo.diff(&DiffRequest::default()).unwrap());
        let paths: Vec<_> = files.iter().map(|file| (file.path.as_str(), file.status)).collect();
        assert_eq!(paths, [("a.txt", FileStatus::Modified), ("new.txt", FileStatus::Added)]);
        // The whole file is there by default.
        assert_eq!(files[0].lines.len(), 1 + 4);

        let staged = DiffRequest { source: DiffSource::Staged, ..DiffRequest::default() };
        assert!(parse(&repo.diff(&staged).unwrap()).is_empty());

        run(root, &["add", "a.txt"]);
        run(root, &["commit", "-q", "-m", "second"]);
        let last = DiffRequest { source: DiffSource::Between { from: "HEAD~1".into(), to: "HEAD".into() }, context: Some(0), untracked: false };
        let files = parse(&repo.diff(&last).unwrap());
        assert_eq!(files.len(), 1);
        assert_eq!((files[0].added, files[0].removed), (1, 1));

        let refs = repo.refs().unwrap();
        assert_eq!(refs.current_branch.as_deref(), Some("main"));
        assert_eq!(refs.default_branch.as_deref(), Some("main"));
        assert_eq!(refs.commits.len(), 2);
    }

    #[test]
    fn a_repository_without_commits_shows_its_files_as_new() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &["init", "-q"]);
        std::fs::write(dir.path().join("x"), "x\n").unwrap();
        let repo = LocalRepo::open(dir.path()).unwrap();
        let files = parse(&repo.diff(&DiffRequest::default()).unwrap());
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn a_folder_outside_git_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(LocalRepo::open(dir.path()).is_err());
    }
}
