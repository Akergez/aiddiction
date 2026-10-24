//! The changed files as a tree of folders, the way revdiff's file pane shows
//! them: folders before files, each sorted by name, and a chain of folders
//! that hold nothing but one folder drawn as one (`crates/diffreview/src`).

use crate::diff::FileDiff;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// What the row says: a file's name, or a folder's path below its parent.
    pub name: String,
    /// The folder's full path, or the file's.
    pub path: String,
    /// The file's index in the list given to [`build`]; `None` for a folder.
    pub file: Option<usize>,
    pub children: Vec<Node>,
}

/// The tree of `files`' paths.
pub fn build(files: &[FileDiff]) -> Vec<Node> {
    let mut root: Vec<Node> = Vec::new();
    for (index, file) in files.iter().enumerate() {
        let parts: Vec<&str> = file.path.split('/').filter(|part| !part.is_empty()).collect();
        let Some((name, folders)) = parts.split_last() else { continue };
        let mut level = &mut root;
        let mut path = String::new();
        for folder in folders {
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(folder);
            let at = match level.iter().position(|node| node.file.is_none() && node.name == *folder) {
                Some(at) => at,
                None => {
                    level.push(Node { name: (*folder).to_owned(), path: path.clone(), file: None, children: Vec::new() });
                    level.len() - 1
                }
            };
            level = &mut level[at].children;
        }
        level.push(Node { name: (*name).to_owned(), path: file.path.clone(), file: Some(index), children: Vec::new() });
    }
    for node in &mut root {
        compress(node);
    }
    sort(&mut root);
    root
}

/// A folder whose one child is a folder becomes one row.
fn compress(node: &mut Node) {
    while node.file.is_none() && node.children.len() == 1 && node.children[0].file.is_none() {
        let child = node.children.remove(0);
        node.name = format!("{}/{}", node.name, child.name);
        node.path = child.path;
        node.children = child.children;
    }
    for child in &mut node.children {
        compress(child);
    }
}

fn sort(nodes: &mut [Node]) {
    nodes.sort_by(|a, b| a.file.is_some().cmp(&b.file.is_some()).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    for node in nodes {
        sort(&mut node.children);
    }
}

/// The files in the order the tree shows them, for moving to the next file.
pub fn file_order(nodes: &[Node]) -> Vec<usize> {
    let mut order = Vec::new();
    for node in nodes {
        order.extend(node.file);
        order.extend(file_order(&node.children));
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(paths: &[&str]) -> Vec<FileDiff> {
        let text: String = paths.iter().map(|path| format!("diff --git a/{path} b/{path}\n")).collect();
        crate::diff::parse(&text)
    }

    fn names(nodes: &[Node]) -> Vec<String> {
        nodes.iter().map(|node| node.name.clone()).collect()
    }

    #[test]
    fn folders_first_and_single_child_chains_folded() {
        let files = files(&["README.md", "crates/a/src/lib.rs", "crates/a/src/view.rs", "crates/b/Cargo.toml", "build.rs"]);
        let tree = build(&files);
        assert_eq!(names(&tree), ["crates", "build.rs", "README.md"]);
        let crates = &tree[0].children;
        assert_eq!(names(crates), ["a/src", "b"]);
        assert_eq!(crates[0].path, "crates/a/src");
        assert_eq!(names(&crates[0].children), ["lib.rs", "view.rs"]);
        assert_eq!(crates[0].children[1].file, Some(2));
        assert_eq!(file_order(&tree), [1, 2, 3, 4, 0]);
    }
}
