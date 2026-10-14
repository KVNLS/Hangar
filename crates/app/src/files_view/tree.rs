//! Flattened, collapsible file tree built from a sorted list of worktree-relative paths.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeRow {
    pub depth: usize,
    pub name: String,
    /// Directory or file path, relative to the worktree.
    pub path: PathBuf,
    pub dir: bool,
    pub expanded: bool,
}

/// Visible rows: directories first within each level, then files, both by name. Children of a
/// directory appear only when it is in `expanded`.
pub fn tree_rows(paths: &[PathBuf], expanded: &HashSet<PathBuf>) -> Vec<TreeRow> {
    #[derive(Default)]
    struct Node {
        dirs: std::collections::BTreeMap<String, Node>,
        files: Vec<String>,
    }
    let mut root = Node::default();
    for path in paths {
        let parts: Vec<String> = path
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let Some((file, dirs)) = parts.split_last() else {
            continue;
        };
        let mut node = &mut root;
        for dir in dirs {
            node = node.dirs.entry(dir.clone()).or_default();
        }
        node.files.push(file.clone());
    }
    fn walk(
        node: &Node,
        prefix: &Path,
        depth: usize,
        expanded: &HashSet<PathBuf>,
        out: &mut Vec<TreeRow>,
    ) {
        for (name, child) in &node.dirs {
            let path = prefix.join(name);
            let open = expanded.contains(&path);
            out.push(TreeRow {
                depth,
                name: name.clone(),
                path: path.clone(),
                dir: true,
                expanded: open,
            });
            if open {
                walk(child, &path, depth + 1, expanded, out);
            }
        }
        let mut files = node.files.clone();
        files.sort();
        for name in files {
            out.push(TreeRow {
                depth,
                path: prefix.join(&name),
                name,
                dir: false,
                expanded: false,
            });
        }
    }
    let mut out = Vec::new();
    walk(&root, Path::new(""), 0, expanded, &mut out);
    out
}

/// Every ancestor directory of `path` (to reveal it in the tree).
pub fn ancestors(path: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    path.ancestors()
        .skip(1)
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    fn names(rows: &[TreeRow]) -> Vec<(usize, &str, bool)> {
        rows.iter()
            .map(|r| (r.depth, r.name.as_str(), r.dir))
            .collect()
    }

    #[test]
    fn collapsed_shows_top_level_dirs_first() {
        let files = paths(&["b.txt", "src/main.rs", "a.txt", "docs/x.md"]);
        let rows = tree_rows(&files, &HashSet::new());
        assert_eq!(
            names(&rows),
            [
                (0, "docs", true),
                (0, "src", true),
                (0, "a.txt", false),
                (0, "b.txt", false)
            ]
        );
    }

    #[test]
    fn expanded_dirs_show_children() {
        let files = paths(&[
            "src/ui/button.rs",
            "src/main.rs",
            "src/ui/mod.rs",
            "README.md",
        ]);
        let expanded: HashSet<PathBuf> = paths(&["src", "src/ui"]).into_iter().collect();
        let rows = tree_rows(&files, &expanded);
        assert_eq!(
            names(&rows),
            [
                (0, "src", true),
                (1, "ui", true),
                (2, "button.rs", false),
                (2, "mod.rs", false),
                (1, "main.rs", false),
                (0, "README.md", false),
            ]
        );
        assert_eq!(rows[2].path, PathBuf::from("src/ui/button.rs"));
        // Expanded but under a collapsed parent: hidden.
        let only_child: HashSet<PathBuf> = paths(&["src/ui"]).into_iter().collect();
        assert_eq!(
            names(&tree_rows(&files, &only_child)),
            [(0, "src", true), (0, "README.md", false)]
        );
    }

    #[test]
    fn ancestors_of_a_path() {
        let got: Vec<PathBuf> = ancestors(Path::new("a/b/c.rs")).collect();
        assert_eq!(got, paths(&["a/b", "a"]));
    }
}
