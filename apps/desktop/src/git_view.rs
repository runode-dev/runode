//! 文件树、Git 面板和预览栏都要画的 git 状态：按状态上的颜色，以及认出 app 交来的是不是同一份状态。

use std::sync::Arc;

use runode_git::{FileStatus, Repos};
use runode_types::color::Rgb;

use crate::ui::theme::{ADDED, MODIFIED, REMOVED, RENAMED};

/// 文件名、标签名和状态字母按 git 状态上的颜色。
pub(crate) fn status_color(status: FileStatus) -> Rgb {
    match status {
        FileStatus::Modified => MODIFIED,
        FileStatus::Added | FileStatus::Untracked => ADDED,
        FileStatus::Deleted | FileStatus::Conflicted => REMOVED,
        FileStatus::Renamed => RENAMED,
    }
}

/// 两次交来的 git 状态是同一份：app 只在状态变了时才换一份新的，所以按是不是同一个 `Arc` 比。
pub(crate) fn same_repos(old: &Option<Arc<Repos>>, new: &Option<Arc<Repos>>) -> bool {
    match (old, new) {
        (Some(old), Some(new)) => Arc::ptr_eq(old, new),
        (old, new) => old.is_none() && new.is_none(),
    }
}
