//! 一个 workspace 的文件树状态：展开收起、排成行，以及换上 app 在后台读到的结果。只管数据，不碰界面。

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use gpui::{ScrollStrategy, SharedString, UniformListScrollHandle};
use runode_git as git;

use crate::file_tree::{
    TreeScan,
    listing::{Decoration, Decorator, DirEntry, GitChanges, Listings, MAX_COMPACT, single_dir},
};

/// 文件树里显示哪些：被 git 忽略的、名字以 `.` 开头的。整个窗口一个设置，见 `FileTree::filter`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TreeFilter {
    pub ignored: bool,
    pub dotfiles: bool,
}

/// 文件树里的一行。
#[derive(Clone, Debug)]
pub(crate) struct FileRow {
    /// 并成一行的目录是链条最深的那个。
    pub path: PathBuf,
    /// 并成一行的目录是「a / b / c」。
    pub name: SharedString,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
    pub decoration: Decoration,
}

/// 一个 workspace 的文件树。
#[derive(Default)]
pub(crate) struct TreeState {
    /// 文件树的根目录，跟着当前终端的目录变；还没读过时为空，读到过一次之后就一直有。
    pub root: Option<PathBuf>,
    /// 最近一次读到的 git 状态，由 app 交进来，行按它着色；不在 git 仓库里时为空。
    pub git: Option<Arc<git::Repos>>,
    /// 按 `git` 那一份快照算好的改动表，git 换了才重算，免得每次重排都在主线程上把所有改动过一遍。
    /// 留着那份 `Arc`，快照就不会释放、地址不会被新快照占用，按指针比不会认错。
    git_changes: Option<(Arc<git::Repos>, GitChanges)>,
    pub expanded: HashSet<PathBuf>,
    pub listings: HashMap<PathBuf, Vec<DirEntry>>,
    /// 文件树里选中的路径。
    pub selected: Option<PathBuf>,
    pub rows: Vec<FileRow>,
    /// 搜索框的搜索词：搜索框整个窗口一个，切走时把框里的词存在这里，切回来时放回去。
    pub query: String,
    pub scroll: UniformListScrollHandle,
    /// 等着在后台读的目录，由 `FileTree` 取走去读，读完经 `apply_listings` 换上。
    pub to_list: Vec<PathBuf>,
    /// 每个目录换上的内容是哪一次读的（`read_ticket`，读不了的也算）。扫描和后台列目录谁先读回来不一定，
    /// 发起更早的读到的是更旧的内容，不能盖掉发起更晚的已经换上的，按这个比先后。
    read_at: HashMap<PathBuf, u64>,
    /// 等目录读回来再定位一次的 `reveal`：要定位的路径所在的目录还没读到，读到以后才排得出那一行。
    pending_reveal: Option<(PathBuf, bool, ScrollStrategy)>,
}

impl TreeState {
    pub(crate) fn rebuild_rows(&mut self, root: &Path, filter: TreeFilter) {
        match &self.git {
            Some(git) if !self.git_changes.as_ref().is_some_and(|(cached, _)| Arc::ptr_eq(git, cached)) => {
                self.git_changes = Some((git.clone(), GitChanges::new(Some(git))));
            }
            Some(_) => {}
            None => self.git_changes = None,
        }
        let none = GitChanges::default();
        let changes = self.git_changes.as_ref().map_or(&none, |(_, changes)| changes);
        let decorator = Decorator::new(self.git.as_deref(), changes);
        let mut rows = Vec::new();
        self.push_dir(root, 0, &decorator, filter, &mut rows);
        self.rows = rows;
    }

    fn push_dir(&self, dir: &Path, depth: usize, decorator: &Decorator, filter: TreeFilter, rows: &mut Vec<FileRow>) {
        let Some(entries) = self.listings.get(dir) else {
            return;
        };
        for entry in entries {
            let mut path = dir.join(&entry.name);
            let hidden = (!filter.ignored && decorator.of(&path, entry.is_dir) == Decoration::Ignored)
                || (!filter.dotfiles && entry.name.as_encoded_bytes().starts_with(b"."));
            if hidden {
                continue;
            }
            let mut name = entry.name.to_string_lossy().into_owned();
            if entry.is_dir {
                for _ in 0..MAX_COMPACT {
                    let Some(child) = self.listings.get(&path).and_then(|entries| single_dir(entries)) else {
                        break;
                    };
                    // 藏着点文件时点开头的子目录也不并进来，不然隐藏的目录跟着上级露出来。
                    if !filter.dotfiles && child.as_encoded_bytes().starts_with(b".") {
                        break;
                    }
                    // 被忽略的子目录不并进没被忽略的目录：隐藏时要单独藏起来，显示时也不该连累上级变暗。
                    if decorator.of(&path.join(child), true) == Decoration::Ignored
                        && decorator.of(&path, true) != Decoration::Ignored
                    {
                        break;
                    }
                    name = format!("{name} / {}", child.to_string_lossy());
                    path = path.join(child);
                }
            }
            let expanded = entry.is_dir && self.expanded.contains(&path);
            rows.push(FileRow {
                name: name.into(),
                depth,
                is_dir: entry.is_dir,
                expanded,
                decoration: decorator.of(&path, entry.is_dir),
                path: path.clone(),
            });
            if expanded {
                self.push_dir(&path, depth + 1, decorator, filter, rows);
            }
        }
    }

    /// `dir` 换上的内容是 `started` 之后发起的读读到的。
    fn newer_than(&self, dir: &Path, started: u64) -> bool {
        self.read_at.get(dir).is_some_and(|&at| at > started)
    }

    /// 记下要在后台读的 `dirs`（连同往下多看的一层，见 `list_dirs`）：大目录、慢的卷读起来会卡住界面。
    fn list_later(&mut self, dirs: Vec<PathBuf>) {
        self.to_list.extend(dirs);
    }

    /// 换上后台读回来的目录；读不了的（删掉了、改了名或者没权限）丢掉记着的旧列表、收起来，
    /// 不再把旧的内容当真（`apply_scan` 同）。根目录已经换了时不是这里的目录，不要；`since`（发起时的 `read_ticket`）
    /// 之后发起的读已经换上的目录，这里读到的是旧内容，也不要。有等着的 `reveal` 时再定位一次。
    pub(crate) fn apply_listings(&mut self, listings: Listings, since: u64, root: &Path, filter: TreeFilter) {
        let mut read = HashSet::new();
        let mut collapsed = Vec::new();
        for (dir, listing) in listings {
            if dir.starts_with(root) && !self.newer_than(&dir, since) {
                self.read_at.insert(dir.clone(), since);
                match listing {
                    Some(listing) => {
                        self.listings.insert(dir.clone(), listing);
                    }
                    None => {
                        self.listings.remove(&dir);
                        self.expanded.remove(&dir);
                        collapsed.push(dir.clone());
                    }
                }
            }
            read.insert(dir);
        }
        self.rebuild_rows(root, filter);
        // 选中的在收起的目录里面时改成这个目录（见 `toggle_dir`；它的行也没了时不留选中），免得看不见的
        // 选中项还被删除、改名用上。套着的几层都读不了时，里层的行跟着外层收起没了，改成还有行的那一层，
        // 不看读回来的先后。等着的 `reveal` 随后照样定位。
        let holding: Vec<_> = collapsed
            .iter()
            .filter(|dir| {
                self.selected.as_deref().is_some_and(|selected| selected != dir.as_path() && selected.starts_with(dir))
            })
            .collect();
        if !holding.is_empty() {
            self.selected = holding.into_iter().find(|dir| self.rows.iter().any(|row| row.path == **dir)).cloned();
        }
        let Some((path, expand, strategy)) = self.pending_reveal.take() else {
            return;
        };
        // shortcut: 不管读的这段时间里用户有没有选了别的，慢的卷上可能把选中拉回 `path`；碰到了再记下发起时的
        // 选中项比一比。
        self.reveal(&path, expand, root, filter, strategy);
        // 刚读过还是读不了的目录不再读，不然定位一个读不了的目录会一直读下去。
        self.to_list.retain(|dir| !read.contains(dir));
        // 读不了的目录在 `path` 的上级链条里时，定位又把它展开了；它不会再读，留着展开就是一个展开着却
        // 什么都没有的目录，收回去。
        let mut reopened = false;
        for dir in &collapsed {
            reopened |= self.expanded.remove(dir);
        }
        if reopened {
            self.rebuild_rows(root, filter);
        }
        if self.to_list.is_empty() {
            self.pending_reveal = None;
        }
    }

    /// 展开或收起文件树里的目录；展开时重读它的内容（在后台），收起期间可能变过，读回来之前先显示
    /// 记着的旧列表。
    pub(crate) fn toggle_dir(&mut self, path: &Path, root: &Path, filter: TreeFilter) {
        if self.expanded.remove(path) {
            // 选中的在收起的目录里面时，选中改成这个目录，免得选中项看不见还被删除、新建用上。
            if self.selected.as_deref().is_some_and(|selected| selected != path && selected.starts_with(path)) {
                self.selected = Some(path.to_path_buf());
            }
        } else {
            self.expanded.insert(path.to_path_buf());
            self.list_later(vec![path.to_path_buf()]);
        }
        self.rebuild_rows(root, filter);
    }

    /// 终端换了目录：在文件树里展开到 `dir`，连它本身也展开，选中它、滚到中间。
    fn reveal_dir(&mut self, dir: &Path, root: &Path, filter: TreeFilter) {
        self.reveal(dir, true, root, filter, ScrollStrategy::Center);
    }

    /// 在文件树里展开到 `path` 所在的目录，选中它、滚到能看见。已经显示着时只选中，不重排。
    pub(crate) fn reveal_file(&mut self, path: &Path, root: &Path, filter: TreeFilter) {
        if let Some(ix) = self.rows.iter().position(|row| row.path == path) {
            self.pending_reveal = None;
            self.select_row(ix);
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
            return;
        }
        self.reveal(path, false, root, filter, ScrollStrategy::Nearest);
    }

    /// 在文件树里展开到 `path` 并选中它，按 `strategy` 滚到能看见的地方；`expand` 时连 `path`
    /// 这个目录本身也展开。`path` 就是根目录或者不在根下面时不动。
    fn reveal(&mut self, path: &Path, expand: bool, root: &Path, filter: TreeFilter, strategy: ScrollStrategy) {
        let Ok(rel) = path.strip_prefix(root) else {
            return;
        };
        if rel.as_os_str().is_empty() {
            return;
        }
        let mut chain: Vec<_> = rel
            .ancestors()
            .skip(usize::from(!expand))
            .filter(|rel| !rel.as_os_str().is_empty())
            .map(|rel| root.join(rel))
            .collect();
        chain.reverse();
        let missing = chain.iter().filter(|dir| !self.listings.contains_key(*dir)).cloned().collect();
        self.list_later(missing);
        // `path` 和它唯一的子目录并成一行时，这一行按链条最深的那个展开，一路展开下去。
        if expand {
            for _ in 0..MAX_COMPACT {
                let last = &chain[chain.len() - 1];
                let Some(child) =
                    self.listings.get(last).and_then(|entries| single_dir(entries)).map(|name| last.join(name))
                else {
                    break;
                };
                if !self.listings.contains_key(&child) {
                    self.list_later(vec![child.clone()]);
                }
                chain.push(child);
            }
        }
        self.expanded.extend(chain);
        self.rebuild_rows(root, filter);
        // 要读的目录还没读回来时，读回来以后再定位一次：那时才排得出 `path` 那一行。
        self.pending_reveal = (!self.to_list.is_empty()).then(|| (path.to_path_buf(), expand, strategy));
        // 并成一行的目录，行的路径是链条最深的那个，`path` 在链条中间时找不到原样的路径。
        let rows = &self.rows;
        let Some(ix) = rows
            .iter()
            .position(|row| row.path == path)
            .or_else(|| rows.iter().position(|row| row.path.starts_with(path)))
        else {
            return;
        };
        self.selected = Some(rows[ix].path.clone());
        self.scroll.scroll_to_item(ix, strategy);
    }

    /// 显示哪些变了：重排行。选中的跟着藏起来时不留着，免得删除、改名还作用在看不见的文件上。
    pub(crate) fn refilter(&mut self, root: &Path, filter: TreeFilter) {
        self.rebuild_rows(root, filter);
        if self.selected_row().is_none() {
            self.selected = None;
        }
    }

    /// 收起根目录下所有展开的目录。
    pub(crate) fn collapse_all(&mut self, root: &Path, filter: TreeFilter) {
        self.expanded.retain(|dir| !dir.starts_with(root));
        self.rebuild_rows(root, filter);
        // 选中的行跟着收起来看不见了，不留着。
        if self.selected_row().is_none() {
            self.selected = None;
        }
    }

    /// 改了文件之后马上重读 `dirs`（在后台），不等监听到改动；app 的重读随后照常进行。
    pub(crate) fn relist(&mut self, dirs: Vec<PathBuf>, root: &Path, filter: TreeFilter) {
        self.list_later(dirs);
        self.rebuild_rows(root, filter);
    }

    /// `from` 改名或挪到了 `to`：展开的目录和选中的路径跟过去，旧路径下读过的目录不再要。
    pub(crate) fn moved(&mut self, from: &Path, to: &Path) {
        let follow = |path: &Path| crate::ui::follow_move(path, from, to);
        self.expanded = self.expanded.drain().map(|dir| follow(&dir).unwrap_or(dir)).collect();
        self.listings.retain(|dir, _| !dir.starts_with(from));
        if let Some(selected) = self.selected.as_deref().and_then(follow) {
            self.selected = Some(selected);
        }
    }

    /// `path` 被删掉了：它和下面的目录不再展开，上级目录的列表里马上去掉它，不等重读回来。选中的是它或在它
    /// 下面时不留选中，免得删除、改名作用在不存在的路径上。
    pub(crate) fn removed(&mut self, path: &Path) {
        if self.selected.as_deref().is_some_and(|selected| selected.starts_with(path)) {
            self.selected = None;
        }
        self.expanded.retain(|dir| !dir.starts_with(path));
        self.listings.retain(|dir, _| !dir.starts_with(path));
        if let Some((parent, name)) = path.parent().zip(path.file_name())
            && let Some(entries) = self.listings.get_mut(parent)
        {
            entries.retain(|entry| entry.name != name);
        }
    }

    /// 选中的行在 `rows` 里的位置。
    pub(crate) fn selected_row(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.rows.iter().position(|row| row.path == *selected)
    }

    /// 键盘上下移动选中的行，`step` 为负往上；没选中时往下从第一行开始，往上从最后一行开始。
    /// 返回新选中的位置。
    pub(crate) fn select_step(&mut self, step: isize) -> Option<usize> {
        let last = self.rows.len().checked_sub(1)?;
        let ix = match self.selected_row() {
            Some(ix) => ix.saturating_add_signed(step).min(last),
            None if step > 0 => 0,
            None => last,
        };
        self.selected = Some(self.rows[ix].path.clone());
        Some(ix)
    }

    /// 选中第 `ix` 行。
    pub(crate) fn select_row(&mut self, ix: usize) -> Option<usize> {
        let row = self.rows.get(ix)?;
        self.selected = Some(row.path.clone());
        Some(ix)
    }

    /// 键盘往左：展开着的目录收起，否则跳到上一级目录。返回新选中的位置。
    pub(crate) fn select_out(&mut self, root: &Path, filter: TreeFilter) -> Option<usize> {
        let ix = self.selected_row()?;
        let row = &self.rows[ix];
        if row.is_dir && row.expanded {
            let path = row.path.clone();
            self.toggle_dir(&path, root, filter);
            return Some(ix);
        }
        let depth = row.depth;
        let parent = self.rows[..ix].iter().rposition(|row| row.depth < depth)?;
        self.select_row(parent)
    }

    /// 键盘往右：收着的目录展开，展开着的跳到它的第一项。返回新选中的位置。
    pub(crate) fn select_in(&mut self, root: &Path, filter: TreeFilter) -> Option<usize> {
        let ix = self.selected_row()?;
        let row = &self.rows[ix];
        if !row.is_dir {
            return None;
        }
        if !row.expanded {
            let path = row.path.clone();
            self.toggle_dir(&path, root, filter);
            return Some(ix);
        }
        let depth = row.depth;
        self.rows.get(ix + 1).filter(|child| child.depth > depth)?;
        self.select_row(ix + 1)
    }

    /// 换上 app 在后台读到的结果，有变化时返回真。
    pub(crate) fn apply_scan(&mut self, scan: TreeScan, filter: TreeFilter) -> bool {
        let TreeScan { root, git, listings, reveal, started } = scan;
        // 第一次读时 `root` 还是空的，下面换上根目录时一定算作有变化。
        let mut changed = false;
        // 终端换到了别的仓库或目录：上一处的目录列表和选中的行不再相干，选中项留着的话删除、改名
        // 还会作用在看不见的旧路径上。展开的目录是绝对路径，留着，回到原处时还是展开的。
        if self.root.as_ref() != Some(&root) {
            self.root = Some(root.clone());
            self.listings.clear();
            self.read_at.clear();
            self.selected = None;
            self.scroll.scroll_to_item(0, ScrollStrategy::Top);
            changed = true;
        }
        // app 只在 git 状态变了时才换一份新的，按是不是同一份比。
        let same_git = match (&self.git, &git) {
            (Some(old), Some(new)) => Arc::ptr_eq(old, new),
            (old, new) => old.is_none() && new.is_none(),
        };
        if !same_git {
            self.git = git;
            changed = true;
        }
        let mut collapsed = Vec::new();
        for (dir, listing) in listings {
            // 扫描发起之后又列过的目录（比如新建、改名后重读的），扫描读到的是旧内容。
            if self.newer_than(&dir, started) {
                continue;
            }
            self.read_at.insert(dir.clone(), started);
            match listing {
                Some(entries) => {
                    if self.listings.get(&dir) != Some(&entries) {
                        self.listings.insert(dir, entries);
                        changed = true;
                    }
                }
                // 目录被删掉或者改了名，收起来。
                None => {
                    changed |= self.listings.remove(&dir).is_some();
                    if self.expanded.remove(&dir) {
                        changed = true;
                        collapsed.push(dir);
                    }
                }
            }
        }
        if changed {
            self.rebuild_rows(&root, filter);
            // 选中的那一项没了行（删掉了，或者在上面收起的目录里）时改成还有行的收起的目录（见
            // `apply_listings`），没有的话不留选中，免得看不见的选中项还被删除、改名、新建用上。
            if self.selected_row().is_none() {
                let selected = self.selected.take();
                self.selected = collapsed.into_iter().find(|dir| {
                    selected.as_deref().is_some_and(|selected| selected.starts_with(dir))
                        && self.rows.iter().any(|row| row.path == *dir)
                });
            }
        }
        // 终端换了目录，文件树跟过去。
        if let Some(dir) = reveal {
            self.reveal_dir(&dir, &root, filter);
            changed = true;
        }
        changed
    }
}

#[cfg(test)]
mod tests;
