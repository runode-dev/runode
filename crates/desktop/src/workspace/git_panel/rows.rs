//! Git 面板排成的行：冲突、已暂存、未暂存三段改动和储藏，各段可以收起；展开的文件下面跟着
//! 改动的块和行。只管数据，不碰界面。

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use gpui::{Entity, Subscription, UniformListScrollHandle};
use runode_git_status::{self as git, FileStatus, Section};

use crate::text_area::TextArea;

/// Git 面板里的一段。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(in crate::workspace) enum GitSection {
    /// 有冲突、等着解决的文件，从未暂存的改动里挑出来放在最前面。
    Merge,
    Staged,
    Unstaged,
    Stashes,
}

impl GitSection {
    /// 这一段里的文件来自 `git::Snapshot` 的哪一段。
    pub fn source(self) -> Section {
        match self {
            Self::Staged => Section::Staged,
            Self::Merge | Self::Unstaged | Self::Stashes => Section::Unstaged,
        }
    }
}

/// 文件在 Git 面板里归哪一段：未暂存的冲突文件单独成段。
pub(in crate::workspace) fn section_of(file: &git::FileDiff, section: Section) -> GitSection {
    match section {
        Section::Staged => GitSection::Staged,
        Section::Unstaged if file.status == FileStatus::Conflicted => GitSection::Merge,
        Section::Unstaged => GitSection::Unstaged,
    }
}

/// Git 面板里的一行；下标指向 `git::Snapshot` 那一段里的文件、块和行，或者储藏列表。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::workspace) enum GitRow {
    Section(GitSection),
    File(Section, usize),
    Hunk(Section, usize, usize),
    Line(Section, usize, usize, usize),
    Note(Section, usize, DiffNote),
    Stash(usize),
}

/// 展开的文件下面不显示行时的说明。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::workspace) enum DiffNote {
    Binary,
    Truncated,
    /// 只改了权限或者只改了名。
    NoContent,
}

/// 正在跑的 git 操作，面板顶上写着它；跑完之前不接新的操作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::workspace) enum Busy {
    Stage,
    Discard,
    Commit,
    Checkout,
    Fetch,
    Pull,
    Push,
    Sync,
    Stash,
}

impl Busy {
    pub fn label(self) -> String {
        match self {
            Self::Stage => rust_i18n::t!("git.busy.stage"),
            Self::Discard => rust_i18n::t!("git.busy.discard"),
            Self::Commit => rust_i18n::t!("git.busy.commit"),
            Self::Checkout => rust_i18n::t!("git.busy.checkout"),
            Self::Fetch => rust_i18n::t!("git.busy.fetch"),
            Self::Pull => rust_i18n::t!("git.busy.pull"),
            Self::Push => rust_i18n::t!("git.busy.push"),
            Self::Sync => rust_i18n::t!("git.busy.sync"),
            Self::Stash => rust_i18n::t!("git.busy.stash"),
        }
        .into_owned()
    }
}

/// 一个 workspace 的 Git 面板。
#[derive(Default)]
pub(in crate::workspace) struct GitPanel {
    pub rows: Vec<GitRow>,
    /// 展开看改动的文件，相对仓库根；默认都收着。
    expanded: HashSet<(Section, PathBuf)>,
    collapsed: HashSet<GitSection>,
    pub scroll: UniformListScrollHandle,
    /// 提交说明框，第一次显示面板时建出来；以及它的事件订阅。
    pub commit_box: Option<Entity<TextArea>>,
    pub commit_events: Option<Subscription>,
    /// 提交说明框的提示里写的分支，变了才换提示。
    pub placeholder_branch: Option<Option<String>>,
    pub busy: Option<Busy>,
}

impl GitPanel {
    /// 终端换到了别的仓库：展开过的文件不再相干。
    pub fn forget_expanded(&mut self) {
        self.expanded.clear();
    }

    pub fn file_expanded(&self, section: Section, path: &Path) -> bool {
        self.expanded.contains(&(section, path.to_path_buf()))
    }

    pub fn section_expanded(&self, section: GitSection) -> bool {
        !self.collapsed.contains(&section)
    }

    pub fn toggle_file(&mut self, section: Section, path: &Path, git: Option<&git::Snapshot>) {
        let key = (section, path.to_path_buf());
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
        self.rebuild(git);
    }

    pub fn toggle_section(&mut self, section: GitSection, git: Option<&git::Snapshot>) {
        if !self.collapsed.remove(&section) {
            self.collapsed.insert(section);
        }
        self.rebuild(git);
    }

    /// 这一段里的文件在 `git::Snapshot` 那一段里的下标，按路径排。
    pub fn files(git: &git::Snapshot, section: GitSection) -> Vec<usize> {
        let source = section.source();
        let files = git.files(source);
        (0..files.len()).filter(|&fi| section_of(&files[fi], source) == section).collect()
    }

    /// 按 `git` 重新排行；展开过、已经不在改动里的文件不再记着。
    pub fn rebuild(&mut self, git: Option<&git::Snapshot>) {
        let mut rows = Vec::new();
        let Some(git) = git else {
            self.rows = rows;
            return;
        };
        self.expanded.retain(|(section, path)| git.files(*section).iter().any(|file| &file.path == path));
        for section in [GitSection::Merge, GitSection::Staged, GitSection::Unstaged] {
            let files = Self::files(git, section);
            if files.is_empty() {
                continue;
            }
            rows.push(GitRow::Section(section));
            if !self.section_expanded(section) {
                continue;
            }
            let source = section.source();
            for fi in files {
                let file = &git.files(source)[fi];
                rows.push(GitRow::File(source, fi));
                if self.file_expanded(source, &file.path) {
                    push_diff(&mut rows, source, fi, file);
                }
            }
        }
        if !git.info.stashes.is_empty() {
            rows.push(GitRow::Section(GitSection::Stashes));
            if self.section_expanded(GitSection::Stashes) {
                rows.extend((0..git.info.stashes.len()).map(GitRow::Stash));
            }
        }
        self.rows = rows;
    }
}

/// 展开的文件下面的块和行，读不了内容时是一句说明。
fn push_diff(rows: &mut Vec<GitRow>, section: Section, fi: usize, file: &git::FileDiff) {
    if file.binary {
        rows.push(GitRow::Note(section, fi, DiffNote::Binary));
        return;
    }
    if file.hunks.is_empty() && !file.truncated {
        rows.push(GitRow::Note(section, fi, DiffNote::NoContent));
    }
    for (hi, hunk) in file.hunks.iter().enumerate() {
        rows.push(GitRow::Hunk(section, fi, hi));
        rows.extend((0..hunk.lines.len()).map(|li| GitRow::Line(section, fi, hi, li)));
    }
    if file.truncated {
        rows.push(GitRow::Note(section, fi, DiffNote::Truncated));
    }
}
