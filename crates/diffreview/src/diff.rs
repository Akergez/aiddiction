//! Unified diffs, as `git diff` writes them, parsed into files and lines.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileStatus {
    Added,
    Deleted,
    Modified,
    Renamed,
}

impl FileStatus {
    /// One letter, as `git status --short` would say it.
    pub fn letter(self) -> char {
        match self {
            FileStatus::Added => 'A',
            FileStatus::Deleted => 'D',
            FileStatus::Modified => 'M',
            FileStatus::Renamed => 'R',
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LineKind {
    /// `@@ -a,b +c,d @@ …`: where a hunk starts.
    Hunk,
    Context,
    Add,
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: LineKind,
    /// The line's number in the old file, for context and removed lines.
    pub old: Option<u32>,
    /// The line's number in the new file, for context and added lines.
    pub new: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiff {
    /// The path annotations name: the new path, or the old one for a
    /// deleted file.
    pub path: String,
    /// Where it was, for a rename.
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub binary: bool,
    pub lines: Vec<DiffLine>,
    pub added: usize,
    pub removed: usize,
}

impl FileDiff {
    fn new(path: String) -> Self {
        Self { path, old_path: None, status: FileStatus::Modified, binary: false, lines: Vec::new(), added: 0, removed: 0 }
    }
}

/// Every file in `text`, in the order git wrote them. Anything that is not a
/// unified diff is passed over.
pub fn parse(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut old_line = 0u32;
    let mut new_line = 0u32;
    let mut in_hunk = false;

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            in_hunk = false;
            files.push(FileDiff::new(path_from_header(rest)));
            continue;
        }
        let Some(file) = files.last_mut() else { continue };

        if in_hunk {
            if let Some(text) = line.strip_prefix('+') {
                file.lines.push(DiffLine { kind: LineKind::Add, old: None, new: Some(new_line), text: text.to_owned() });
                new_line += 1;
                file.added += 1;
                continue;
            }
            if let Some(text) = line.strip_prefix('-') {
                file.lines.push(DiffLine { kind: LineKind::Remove, old: Some(old_line), new: None, text: text.to_owned() });
                old_line += 1;
                file.removed += 1;
                continue;
            }
            if let Some(text) = line.strip_prefix(' ') {
                file.lines.push(DiffLine { kind: LineKind::Context, old: Some(old_line), new: Some(new_line), text: text.to_owned() });
                old_line += 1;
                new_line += 1;
                continue;
            }
            if line.is_empty() {
                // Some tools drop the space of an empty context line.
                file.lines.push(DiffLine { kind: LineKind::Context, old: Some(old_line), new: Some(new_line), text: String::new() });
                old_line += 1;
                new_line += 1;
                continue;
            }
            if line.starts_with('\\') {
                // "\ No newline at end of file"
                continue;
            }
        }

        if line.starts_with("@@") {
            if let Some((old, new)) = hunk_starts(line) {
                old_line = old;
                new_line = new;
                in_hunk = true;
                file.lines.push(DiffLine { kind: LineKind::Hunk, old: None, new: None, text: line.to_owned() });
            }
        } else if line.starts_with("new file mode") {
            file.status = FileStatus::Added;
        } else if line.starts_with("deleted file mode") {
            file.status = FileStatus::Deleted;
        } else if let Some(from) = line.strip_prefix("rename from ") {
            file.status = FileStatus::Renamed;
            file.old_path = Some(from.to_owned());
        } else if let Some(to) = line.strip_prefix("rename to ") {
            file.path = to.to_owned();
        } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
            file.binary = true;
        } else if let Some(path) = line.strip_prefix("+++ ") {
            if let Some(path) = strip_side(path, "b/") {
                file.path = path;
            }
        } else if let Some(path) = line.strip_prefix("--- ")
            && file.status == FileStatus::Deleted
                && let Some(path) = strip_side(path, "a/") {
                    file.path = path;
                }
    }
    files
}

/// `a/x b/x` → `x`. Paths with spaces are ambiguous in this header; the
/// `+++`/`---` lines or the rename lines that follow settle them.
fn path_from_header(rest: &str) -> String {
    let rest = rest.trim();
    match rest.rfind(" b/") {
        Some(at) => rest[at + 3..].to_owned(),
        None => rest.trim_start_matches("a/").to_owned(),
    }
}

fn strip_side(path: &str, side: &str) -> Option<String> {
    let path = path.split('\t').next().unwrap_or(path).trim();
    if path == "/dev/null" {
        return None;
    }
    let path = path.trim_matches('"');
    Some(path.strip_prefix(side).unwrap_or(path).to_owned())
}

/// `@@ -12,7 +12,9 @@` → (12, 12).
fn hunk_starts(line: &str) -> Option<(u32, u32)> {
    let mut parts = line.split_whitespace().skip(1);
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let start = |range: &str| range.split(',').next()?.parse::<u32>().ok();
    Some((start(old)?, start(new)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "\
diff --git a/src/a.rs b/src/a.rs
index 1111111..2222222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,4 +1,5 @@
 fn main() {
-    old();
+    new();
+    more();
 
 }
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
diff --git a/old name.rs b/new name.rs
similarity index 90%
rename from old name.rs
rename to new name.rs
diff --git a/img.png b/img.png
new file mode 100644
Binary files /dev/null and b/img.png differ
";

    #[test]
    fn files_lines_and_numbers() {
        let files = parse(DIFF);
        assert_eq!(files.len(), 4);
        let a = &files[0];
        assert_eq!((a.path.as_str(), a.status, a.added, a.removed), ("src/a.rs", FileStatus::Modified, 2, 1));
        assert_eq!(a.lines[0].kind, LineKind::Hunk);
        assert_eq!(a.lines[2], DiffLine { kind: LineKind::Remove, old: Some(2), new: None, text: "    old();".into() });
        assert_eq!(a.lines[4], DiffLine { kind: LineKind::Add, old: None, new: Some(3), text: "    more();".into() });
        assert_eq!(a.lines[5], DiffLine { kind: LineKind::Context, old: Some(3), new: Some(4), text: String::new() });

        assert_eq!((files[1].path.as_str(), files[1].status), ("gone.txt", FileStatus::Deleted));
        assert_eq!(files[2].path, "new name.rs");
        assert_eq!(files[2].old_path.as_deref(), Some("old name.rs"));
        assert_eq!(files[2].status, FileStatus::Renamed);
        assert!(files[3].binary);
        assert_eq!(files[3].status, FileStatus::Added);
    }
}
