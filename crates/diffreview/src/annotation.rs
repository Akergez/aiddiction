//! Annotations and revdiff's text format for them.
//!
//! ```text
//! ## handler.go (file-level)
//! consider splitting this file
//!
//! ## handler.go:43 (+)
//! use errors.Is() instead
//!
//! ## handler.go:43-67 (+)
//! refactor this hunk to reduce nesting
//!
//! ## store.go:18 (-)
//! don't remove this validation
//! ```
//!
//! The same rules as revdiff's `annotation.Store`: a line is named by its new
//! number, a removed line by its old one; one annotation per file, line and
//! kind of change; files in alphabetical order, the file-level note first;
//! a body line that would read as a header gets one leading space.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::diff::{DiffLine, LineKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ChangeType {
    Add,
    Remove,
    Context,
}

impl ChangeType {
    pub fn marker(self) -> char {
        match self {
            ChangeType::Add => '+',
            ChangeType::Remove => '-',
            ChangeType::Context => ' ',
        }
    }

    fn parse(marker: &str) -> Option<Self> {
        match marker {
            "+" => Some(ChangeType::Add),
            "-" => Some(ChangeType::Remove),
            " " => Some(ChangeType::Context),
            _ => None,
        }
    }
}

/// Where an annotation is: a whole file, or one line of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Anchor {
    File,
    Line { line: u32, change: ChangeType },
}

impl Anchor {
    /// The anchor of a diff line, `None` for a hunk header.
    pub fn of(line: &DiffLine) -> Option<Self> {
        let (line, change) = match line.kind {
            LineKind::Add => (line.new?, ChangeType::Add),
            LineKind::Remove => (line.old?, ChangeType::Remove),
            LineKind::Context => (line.new?, ChangeType::Context),
            LineKind::Hunk => return None,
        };
        Some(Anchor::Line { line, change })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Annotation {
    pub file: String,
    pub anchor: Anchor,
    /// The last line of the hunk, when the comment speaks of the hunk.
    pub end_line: Option<u32>,
    pub comment: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Store {
    by_file: BTreeMap<String, BTreeMap<Anchor, Annotation>>,
}

impl Store {
    /// Adds or replaces the annotation at its file and anchor.
    pub fn add(&mut self, annotation: Annotation) {
        self.by_file.entry(annotation.file.clone()).or_default().insert(annotation.anchor, annotation);
    }

    pub fn remove(&mut self, file: &str, anchor: Anchor) -> Option<Annotation> {
        let annotations = self.by_file.get_mut(file)?;
        let removed = annotations.remove(&anchor);
        if annotations.is_empty() {
            self.by_file.remove(file);
        }
        removed
    }

    pub fn get(&self, file: &str, anchor: Anchor) -> Option<&Annotation> {
        self.by_file.get(file)?.get(&anchor)
    }

    /// One file's annotations: the file-level one first, then by line.
    pub fn in_file(&self, file: &str) -> impl Iterator<Item = &Annotation> {
        self.by_file.get(file).into_iter().flat_map(|annotations| annotations.values())
    }

    pub fn count(&self) -> usize {
        self.by_file.values().map(BTreeMap::len).sum()
    }

    pub fn count_in(&self, file: &str) -> usize {
        self.by_file.get(file).map_or(0, BTreeMap::len)
    }

    pub fn is_empty(&self) -> bool {
        self.by_file.is_empty()
    }

    pub fn clear(&mut self) {
        self.by_file.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = &Annotation> {
        self.by_file.values().flat_map(|annotations| annotations.values())
    }

    /// revdiff's output: empty when there is nothing.
    pub fn format(&self) -> String {
        let mut out = String::new();
        for annotation in self.iter() {
            if !out.is_empty() {
                out.push('\n');
            }
            let body = escape_headers(&annotation.comment);
            match (annotation.anchor, annotation.end_line) {
                (Anchor::File, _) => out.push_str(&format!("## {} (file-level)\n{body}\n", annotation.file)),
                (Anchor::Line { line, change }, Some(end)) if end > line => {
                    out.push_str(&format!("## {}:{line}-{end} ({})\n{body}\n", annotation.file, change.marker()))
                }
                (Anchor::Line { line, change }, _) => {
                    out.push_str(&format!("## {}:{line} ({})\n{body}\n", annotation.file, change.marker()))
                }
            }
        }
        out
    }

    /// Reads what [`Store::format`] wrote (or revdiff did), the last of a
    /// repeated anchor winning.
    pub fn parse(text: &str) -> Self {
        let mut store = Store::default();
        let mut current: Option<(String, Anchor, Option<u32>)> = None;
        let mut body: Vec<&str> = Vec::new();
        let finish = |current: &mut Option<(String, Anchor, Option<u32>)>, body: &mut Vec<&str>, store: &mut Store| {
            if let Some((file, anchor, end_line)) = current.take() {
                while body.last().is_some_and(|line| line.is_empty()) {
                    body.pop();
                }
                let comment = body.iter().map(|line| unescape_header(line)).collect::<Vec<_>>().join("\n");
                store.add(Annotation { file, anchor, end_line, comment });
            }
            body.clear();
        };
        for line in text.lines() {
            if let Some(header) = line.strip_prefix("## ").and_then(parse_header) {
                finish(&mut current, &mut body, &mut store);
                current = Some(header);
            } else if current.is_some() {
                body.push(line);
            }
        }
        finish(&mut current, &mut body, &mut store);
        store
    }
}

/// `file (file-level)`, `file:12 (+)`, `file:12-30 (-)`.
fn parse_header(header: &str) -> Option<(String, Anchor, Option<u32>)> {
    if let Some(file) = header.strip_suffix(" (file-level)") {
        return Some((file.to_owned(), Anchor::File, None));
    }
    let open = header.rfind(" (")?;
    let change = ChangeType::parse(header[open + 2..].strip_suffix(')')?)?;
    let location = &header[..open];
    let colon = location.rfind(':')?;
    let (file, lines) = (&location[..colon], &location[colon + 1..]);
    let (line, end) = match lines.split_once('-') {
        Some((start, end)) => (start.parse().ok()?, Some(end.parse().ok()?)),
        None => (lines.parse().ok()?, None),
    };
    Some((file.to_owned(), Anchor::Line { line, change }, end))
}

fn escape_headers(body: &str) -> String {
    if !body.contains("## ") {
        return body.to_owned();
    }
    body.lines()
        .map(|line| if line.trim_start_matches(' ').starts_with("## ") { format!(" {line}") } else { line.to_owned() })
        .collect::<Vec<_>>()
        .join("\n")
}

fn unescape_header(line: &str) -> &str {
    if line.trim_start_matches(' ').starts_with("## ") { line.strip_prefix(' ').unwrap_or(line) } else { line }
}

/// Whether a comment speaks of the hunk around its line ("refactor this
/// hunk"), which revdiff writes as a line range.
pub fn mentions_hunk(comment: &str) -> bool {
    comment
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| word.eq_ignore_ascii_case("hunk"))
}

/// The last line of the run of added or removed lines `index` is in, as the
/// annotation would name it; `None` outside such a run.
pub fn hunk_end(lines: &[DiffLine], index: usize) -> Option<u32> {
    let kind = lines.get(index)?.kind;
    if !matches!(kind, LineKind::Add | LineKind::Remove) {
        return None;
    }
    let last = lines[index..].iter().take_while(|line| line.kind == kind).last()?;
    match kind {
        LineKind::Add => last.new,
        _ => last.old,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(file: &str, line: u32, change: ChangeType, comment: &str) -> Annotation {
        Annotation { file: file.into(), anchor: Anchor::Line { line, change }, end_line: None, comment: comment.into() }
    }

    #[test]
    fn output_is_revdiffs() {
        let mut store = Store::default();
        store.add(at("store.go", 18, ChangeType::Remove, "don't remove this validation"));
        store.add(at("handler.go", 43, ChangeType::Add, "use errors.Is() instead of direct comparison"));
        store.add(Annotation { file: "handler.go".into(), anchor: Anchor::File, end_line: None, comment: "consider splitting this file into smaller modules".into() });
        store.add(Annotation { end_line: Some(67), ..at("handler.go", 44, ChangeType::Add, "refactor this hunk to reduce nesting") });
        assert_eq!(
            store.format(),
            "## handler.go (file-level)\nconsider splitting this file into smaller modules\n\n\
             ## handler.go:43 (+)\nuse errors.Is() instead of direct comparison\n\n\
             ## handler.go:44-67 (+)\nrefactor this hunk to reduce nesting\n\n\
             ## store.go:18 (-)\ndon't remove this validation\n"
        );
    }

    #[test]
    fn the_format_reads_back_including_escaped_headers() {
        let mut store = Store::default();
        store.add(at("a.rs", 3, ChangeType::Context, "line one\n## not a header\n\nlast"));
        store.add(Annotation { end_line: Some(9), ..at("b c.rs", 5, ChangeType::Remove, "this hunk") });
        let text = store.format();
        assert!(text.contains("\n ## not a header\n"));
        assert_eq!(Store::parse(&text), store);
    }

    #[test]
    fn one_annotation_per_line_and_kind() {
        let mut store = Store::default();
        store.add(at("a", 1, ChangeType::Add, "first"));
        store.add(at("a", 1, ChangeType::Add, "second"));
        store.add(at("a", 1, ChangeType::Remove, "other side"));
        assert_eq!(store.count(), 2);
        assert_eq!(store.get("a", Anchor::Line { line: 1, change: ChangeType::Add }).unwrap().comment, "second");
        store.remove("a", Anchor::Line { line: 1, change: ChangeType::Add });
        store.remove("a", Anchor::Line { line: 1, change: ChangeType::Remove });
        assert!(store.is_empty());
    }

    #[test]
    fn a_hunk_is_the_run_of_its_kind() {
        let lines = crate::diff::parse("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +1,3 @@\n a\n+b\n+c\n-d\n");
        let lines = &lines[0].lines;
        assert_eq!(hunk_end(lines, 2), Some(3));
        assert_eq!(hunk_end(lines, 4), Some(2));
        assert_eq!(hunk_end(lines, 1), None);
        assert!(mentions_hunk("Refactor this Hunk, please"));
        assert!(!mentions_hunk("chunky"));
    }
}
