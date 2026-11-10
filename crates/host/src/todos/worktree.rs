//! 任务的工作树：开始任务时在仓库的 `.worktrees/` 下新开一个，名字取自标题、跳过被占了的；任务完成后连同
//! 分支一起清掉，不丢没提交的改动和没合并的提交。

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use runode_git::Repo;
use runode_protocol::todo::Todo;

/// 分支名和工作树目录名里取自标题的部分最长这么多个字符。
const MAX_SLUG: usize = 40;

/// 给任务开工作树时最多拿这么多个没被占的名字去试。
const WORKTREE_ATTEMPTS: usize = 2;

/// 给任务新开工作树：仓库的 `.worktrees/<名字>`，分支 `runode/<名字>`，名字取自标题；已经有了时名字后面
/// 加上任务的标识，还有的话再加上序号。已经被占了的名字（`taken`）直接跳过，不用 git 去试；
/// 真去试的最多 `WORKTREE_ATTEMPTS` 个。返回工作树的根目录和分支。
pub(super) fn add_worktree(todo: &Todo) -> Result<(PathBuf, String), String> {
    let repo =
        Repo::open(&todo.spec.dir).ok_or_else(|| format!("{} is not in a git repository", todo.spec.dir.display()))?;
    let slug = slug(&todo.spec.title).unwrap_or_else(|| todo.id.clone());
    let worktrees = repo.root.join(runode_git::WORKTREES_DIR);
    let branches: HashSet<String> =
        repo.branches().into_iter().filter(|branch| !branch.remote).map(|branch| branch.name).collect();
    let names = [slug.clone(), format!("{slug}-{}", todo.id)]
        .into_iter()
        .chain((2..).map(|n| format!("{slug}-{}-{n}", todo.id)));
    // 跳过的名字不是分支已经有了（同名或者上下一级，一个分支最多占一个候选；叫 `runode` 的分支占掉全部，
    // 那样本来就开不出来）、就是 `.worktrees` 下已经有了这一项，最多这么多个；只算分支的话，只占了目录的几个
    // 名字就把候选用完了，一次都没去试。
    let occupied = std::fs::read_dir(&worktrees).map_or(0, Iterator::count);
    let mut last = String::new();
    let mut attempts = 0;
    for name in names.take(branches.len() + occupied + WORKTREE_ATTEMPTS) {
        let (root, branch) = (worktrees.join(&name), format!("{}{name}", runode_git::TASK_BRANCH_PREFIX));
        if taken(&root, &branch, &branches) {
            last = format!("{branch} or {} is taken", root.display());
            continue;
        }
        match repo.add_worktree(&root, &branch, None) {
            Ok(()) => return Ok((root, branch)),
            Err(err) => last = err.to_string(),
        }
        attempts += 1;
        if attempts == WORKTREE_ATTEMPTS {
            break;
        }
    }
    Err(format!("cannot make a worktree for todo {}: {last}", todo.id))
}

/// 工作树 `root` 或者分支 `branch` 已经被占了，git 开不出来。`root` 上有什么都算，悬空的符号链接也算（git
/// 不跟着链接看）。分支和已有的分支（`branches`）同名，或者一个是另一个往下的一级（`runode/a` 和
/// `runode/a/b`）都算：git 的分支名也是路径，两个不能并存。
fn taken(root: &Path, branch: &str, branches: &HashSet<String>) -> bool {
    root.symlink_metadata().is_ok()
        || branches.iter().any(|other| {
            other == branch
                || other.strip_prefix(branch).is_some_and(|rest| rest.starts_with('/'))
                || branch.strip_prefix(other.as_str()).is_some_and(|rest| rest.starts_with('/'))
        })
}

/// 删掉任务的工作树（有改动时 git 拒绝，留着）和分支（只删已经合并、或者没有新提交的），返回各删掉了
/// 没有。不会丢东西：没提交的改动留在工作树里，没合并的提交留在分支上。
pub(super) fn clean_worktree(todo: &Todo) -> (bool, bool) {
    let Some(worktree) = &todo.worktree_dir else { return (false, false) };
    let Some(repo) = Repo::open(&todo.spec.dir) else { return (false, false) };
    let worktree_gone = match repo.remove_worktree(worktree, false) {
        Ok(()) => true,
        Err(err) => {
            tracing::info!("kept the worktree {} of todo {}: {err}", worktree.display(), todo.id);
            !worktree.exists()
        }
    };
    let branch_gone = worktree_gone
        && todo.branch.as_deref().is_some_and(|branch| match repo.delete_merged_branch(branch) {
            Ok(()) => true,
            Err(err) => {
                tracing::info!("kept the branch {branch} of todo {}: {err}", todo.id);
                false
            }
        });
    (worktree_gone, branch_gone)
}

/// `dir` 在工作树 `root` 里对应的目录：任务的目录在仓库的子目录里时，工作树里也进到同一个子目录。
pub(super) fn in_worktree(root: &Path, dir: &Path) -> PathBuf {
    let relative = Repo::open(dir).and_then(|repo| dir.strip_prefix(&repo.root).ok().map(Path::to_path_buf));
    let wanted = root.join(relative.unwrap_or_default());
    if wanted.is_dir() { wanted } else { root.to_path_buf() }
}

/// 标题里的 ASCII 字母和数字转成小写，其余的连成一个 `-`，最多 `MAX_SLUG` 个字符；什么都不剩时为空。
fn slug(title: &str) -> Option<String> {
    let mut slug = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
        if slug.len() >= MAX_SLUG {
            break;
        }
    }
    let slug = slug.trim_end_matches('-');
    (!slug.is_empty()).then(|| slug.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_keep_ascii_words() {
        assert_eq!(slug("Fix the login redirect!").as_deref(), Some("fix-the-login-redirect"));
        assert_eq!(slug("  修复 Login 页  ").as_deref(), Some("login"));
        assert_eq!(slug("修复登录"), None);
        assert_eq!(slug(&"a".repeat(100)).map(|slug| slug.len()), Some(MAX_SLUG));
        assert_eq!(slug("ab-------------------------------------cd x").as_deref(), Some("ab-cd-x"));
    }

    /// 候选的工作树目录上有悬空的符号链接、候选分支和已有的分支一个在另一个下面时都算占了：git 在那开不出来，
    /// 不跳过的话白白用掉一次尝试。
    #[cfg(unix)]
    #[test]
    fn names_git_would_refuse_are_taken() {
        let dir = std::env::temp_dir().join(format!("rnh-unit-{}-todo-taken", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dangling = dir.join("dangling");
        std::os::unix::fs::symlink(dir.join("nowhere"), &dangling).unwrap();
        let free = dir.join("free");
        let branches: HashSet<String> =
            ["main", "runode/a/old", "runode/b", "runode/c-d"].into_iter().map(String::from).collect();

        assert!(taken(&dangling, "runode/x", &branches));
        assert!(!taken(&free, "runode/x", &branches));
        assert!(taken(&free, "runode/b", &branches));
        // 已有的分支在它下面、它在已有的分支下面。
        assert!(taken(&free, "runode/a", &branches));
        assert!(taken(&free, "runode/b/new", &branches));
        // 只是名字开头一样的不冲突。
        assert!(!taken(&free, "runode/c", &branches));
        assert!(!taken(&free, "runode/a-2", &branches));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
