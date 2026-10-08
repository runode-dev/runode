//! Git 面板和文件树的搜索结果以树形式查看时，把一组文件按目录分层排成行：目录在前、文件在后，各自按名字
//! 排（不分大小写）；只有一个子目录、没有文件的目录和子目录并成一行，名字写成 `a/b/c`。
//! 只管数据，不碰界面。

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// 排成树以后的一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::window) enum TreeItem {
    /// 目录：`path` 是相对根目录的路径，并成一行的是链条最深的那个；`name` 是显示的名字。
    Dir { path: PathBuf, name: String, depth: usize, expanded: bool },
    /// 文件：`index` 是调用方给的下标。
    File { index: usize, depth: usize },
}

#[derive(Default)]
struct Node {
    /// 子目录，按名字排；键是不分大小写的名字加原名，同名不同大小写的也分得开。
    dirs: BTreeMap<(String, String), Node>,
    files: Vec<(String, usize)>,
}

impl Node {
    fn child(&mut self, name: &str) -> &mut Self {
        self.dirs.entry((name.to_lowercase(), name.to_owned())).or_default()
    }

    /// 只有一个子目录、没有文件时是那个子目录。
    fn single_dir(&self) -> Option<(&str, &Self)> {
        match (self.dirs.len(), self.files.is_empty()) {
            (1, true) => self.dirs.iter().next().map(|((_, name), node)| (name.as_str(), node)),
            _ => None,
        }
    }
}

/// 把 `files`（下标和相对根目录的路径）排成树。`expanded` 说某个目录（并成一行的按链条最深的
/// 那个）展开着没有，收起的目录下面的不排。
pub(in crate::window) fn file_tree(files: &[(usize, &Path)], expanded: impl Fn(&Path) -> bool) -> Vec<TreeItem> {
    let mut root = Node::default();
    for &(index, path) in files {
        let mut node = &mut root;
        let mut parts: Vec<_> = path.iter().map(|part| part.to_string_lossy().into_owned()).collect();
        let Some(name) = parts.pop() else {
            continue;
        };
        for part in &parts {
            node = node.child(part);
        }
        node.files.push((name, index));
    }
    let mut items = Vec::new();
    push_node(&root, Path::new(""), 0, &expanded, &mut items);
    items
}

fn push_node(node: &Node, dir: &Path, depth: usize, expanded: &impl Fn(&Path) -> bool, items: &mut Vec<TreeItem>) {
    for ((_, name), child) in &node.dirs {
        let mut path = dir.join(name);
        let mut label = name.clone();
        let mut child = child;
        while let Some((next, node)) = child.single_dir() {
            path.push(next);
            label = format!("{label}/{next}");
            child = node;
        }
        let open = expanded(&path);
        items.push(TreeItem::Dir { path: path.clone(), name: label, depth, expanded: open });
        if open {
            push_node(child, &path, depth + 1, expanded, items);
        }
    }
    let mut files: Vec<_> = node.files.iter().collect();
    files.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()).then_with(|| a.0.cmp(&b.0)));
    items.extend(files.into_iter().map(|&(_, index)| TreeItem::File { index, depth }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(path: &str, name: &str, depth: usize) -> TreeItem {
        TreeItem::Dir { path: path.into(), name: name.into(), depth, expanded: true }
    }

    fn file(index: usize, depth: usize) -> TreeItem {
        TreeItem::File { index, depth }
    }

    #[test]
    fn groups_files_by_directory() {
        let paths = ["src/main.rs", "README.md", "src/a/b.rs", "Cargo.toml", "src/A.rs", "docs/x.md"];
        let files: Vec<_> = paths.iter().enumerate().map(|(ix, path)| (ix, Path::new(*path))).collect();
        let items = file_tree(&files, |_| true);
        // 目录在前、文件在后，各自按名字排，不分大小写；只有一个文件的目录不并。
        assert_eq!(
            items,
            [
                dir("docs", "docs", 0),
                file(5, 1),
                dir("src", "src", 0),
                dir("src/a", "a", 1),
                file(2, 2),
                file(4, 1),
                file(0, 1),
                file(3, 0),
                file(1, 0),
            ]
        );
    }

    #[test]
    fn compacts_single_child_directories() {
        let paths = ["crates/desktop/src/a.rs", "crates/desktop/src/b/c.rs", "crates/desktop/src/b/d.rs"];
        let files: Vec<_> = paths.iter().enumerate().map(|(ix, path)| (ix, Path::new(*path))).collect();
        let items = file_tree(&files, |_| true);
        assert_eq!(
            items,
            [
                dir("crates/desktop/src", "crates/desktop/src", 0),
                dir("crates/desktop/src/b", "b", 1),
                file(1, 2),
                file(2, 2),
                file(0, 1)
            ]
        );
        // 收起的目录下面的不排；展开与否按并成一行后最深的那个目录认。
        let items = file_tree(&files, |path| path != Path::new("crates/desktop/src"));
        assert_eq!(
            items,
            [TreeItem::Dir {
                path: "crates/desktop/src".into(),
                name: "crates/desktop/src".into(),
                depth: 0,
                expanded: false
            }]
        );
    }

    #[test]
    fn files_at_the_root_stay_flat() {
        let files = [(0, Path::new("b.txt")), (1, Path::new("a.txt"))];
        assert_eq!(file_tree(&files, |_| true), [file(1, 0), file(0, 0)]);
        assert!(file_tree(&[], |_| true).is_empty());
    }
}
