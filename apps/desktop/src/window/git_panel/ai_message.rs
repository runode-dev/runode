//! 提交说明框里的 ✨ 按钮和挑 agent、改提示词的对话框（`dialog`）：按这个仓库存着的预设直接写，还没存过
//! 预设时打开对话框；对话框里能临时换着试，也能存成预设。

mod dialog;

pub(in crate::window) use dialog::CommitMessageDialog;

use std::path::Path;

use gpui::{Context, Window};
use runode_git::{GitError, Section};

use crate::{
    git_panel::{
        commit_message::{MAX_FILES_BYTES, PromptContext, Recipe, generate, limit, saved_recipe},
        show_git_error,
    },
    host_client::hosts::HostId,
    project_host::ProjectRepo,
    window::WindowView,
};

impl WindowView {
    /// 说明框里的 ✨ 按钮：按根目录是 `root` 的仓库存着的预设写；还没存过预设时打开对话框。
    pub(super) fn generate_commit_message(&mut self, root: &Path, window: &mut Window, cx: &mut Context<Self>) {
        match saved_recipe(root) {
            Ok(Some((recipe, _))) => self.write_commit_message(root, recipe, window, cx),
            Ok(None) => self.open_commit_message_dialog(root.to_path_buf(), window, cx),
            Err(message) => show_git_error(&GitError::Other(message), window, cx),
        }
    }

    /// 按 `recipe` 给根目录是 `root` 的仓库要提交的改动写提交说明，写好了替换提交说明框里的字。经 Git 面板跑，
    /// 和这个仓库别的操作一起排队，写的时候面板写着在写说明。
    pub(in crate::window) fn write_commit_message(
        &mut self,
        root: &Path,
        recipe: Recipe,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project = &self.workspace().project;
        let Some(repo) = project.git.as_ref().and_then(|git| git.iter().find(|repo| repo.root == root)) else {
            return;
        };
        if repo.is_clean() {
            return;
        }
        let staged = !repo.staged.is_empty();
        // 没有暂存的改动时提交会先全部暂存，这时 `unstaged` 就是全部。
        let files: Vec<String> = repo
            .files(if staged { Section::Staged } else { Section::Unstaged })
            .iter()
            .map(|file| format!("{} {}", file.status.letter(), file.path.display()))
            .collect();
        let context = PromptContext {
            staged,
            branch: repo.info.branch.clone().unwrap_or_else(|| "(detached)".to_owned()),
            files: limit(files.join("\n"), MAX_FILES_BYTES),
        };
        // 从访达打开的 app 自己的 PATH 里没有 agent 装的目录，用终端里 shell 报告的 PATH。agent 在这台
        // 机器上跑，只认本机终端的；窗口里一个本机终端都没有时在后台问登录 shell。
        let path = self.terminal_path(Some(&HostId::Local), cx);
        let op = move |repo: &ProjectRepo| generate(repo, &recipe, &context, path).map_err(GitError::Other);
        self.with_git_panel(cx, |panel, cx| panel.write_message(root, op, window, cx));
    }
}
