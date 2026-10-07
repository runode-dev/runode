//! 预览栏里的 diff 标签，仿 VSCode 的内联差异编辑器：从 Git 面板点开改动的文件（工作区、已暂存或
//! 某个提交里的），整篇显示，没改的行照常排着，删掉的行红底插在原来的位置，加的行绿底，左边两栏是
//! 旧行号和新行号，按文件类型高亮。每块改动前面一行块头，工作区和暂存区的块头上有按块暂存、丢弃、
//! 取消暂存的按钮。顶上一条能跳到上一处、下一处改动，打开时先滚到第一处。
//!
//! 整篇 diff 由 `runode_git::Repo::file_view` 读；工作区和暂存区的 diff 跟着扫描结果重读，
//! 滚动位置不动，文件已经没有这种改动时写一句「没有改动」，标签留着。

use std::{
    ops::Range,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use gpui::{
    AnyElement, Axis, Context, ListHorizontalSizingBehavior, MouseButton, MouseDownEvent, PromptLevel, ScrollStrategy,
    SharedString, StyledText, Window, div, prelude::*, px, svg, uniform_list,
};
use runode_config::PreviewClick;
use runode_git::{self as git, DiffRow, DiffSide, DiffView, FileStatus, HunkAction, LineKind, hunk_actionable};
use runode_preview::Span;
use runode_shared_types::color::Rgb;

use super::{
    BODY_PADDING, Loaded, MAX_COLUMNS, Note, ROW_EXTRA_HEIGHT,
    body::{ansi_palette, highlight_style},
    right_fade,
};
use crate::{
    assets::{ARROW_DOWN_ICON, ARROW_UP_ICON, DISCARD_ICON, MINUS_ICON, PLUS_ICON},
    config::AppConfig,
    ui::{hsla, scrollbar::scrollbar, tooltip::tooltip},
    window::{
        WindowView, divider_color,
        git_panel::Busy,
        project::{ADDED, REMOVED, added_label, removed_label},
    },
};

/// 顶上那一条的高度。
const TOOLBAR_HEIGHT: f32 = 26.;

/// 预览栏里看哪个文件的哪种 diff。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::window) struct DiffTarget {
    /// 文件所在仓库的根目录。
    pub root: PathBuf,
    /// 相对仓库根的路径；改名的文件 `old_rel` 是原来的路径。
    pub rel: PathBuf,
    pub old_rel: Option<PathBuf>,
    pub side: DiffSide,
}

/// diff 标签读到的东西。
pub(super) struct DiffContent {
    view: Arc<DiffView>,
    /// 删掉的行的原文，按出现的先后，给高亮用；`removed_at` 和 `view.rows` 一一对应，删掉的行是它在
    /// `removed` 里的下标。
    removed: Arc<Vec<String>>,
    removed_at: Vec<Option<usize>>,
    /// 新文件各行和删掉的各行的高亮，后台做完之前为空；重读时留着上一次的，行对不上也只是颜色暂时不准。
    new_spans: Option<Arc<Vec<Vec<Span>>>>,
    removed_spans: Option<Arc<Vec<Vec<Span>>>>,
    /// 各块块头在 `view.rows` 里的位置，跳到上一处、下一处改动用。
    headers: Vec<usize>,
    /// 最长的一行在 `view.rows` 里的位置，列表按它的宽度横向滚动。
    widest: usize,
}

impl DiffContent {
    fn new(view: DiffView) -> Self {
        let mut removed = Vec::new();
        let mut removed_at = Vec::with_capacity(view.rows.len());
        let mut headers = Vec::new();
        let (mut widest, mut widest_len) = (0, 0);
        for (ix, row) in view.rows.iter().enumerate() {
            let mut at = None;
            if let DiffRow::Header(_) = row {
                headers.push(ix);
            }
            if let DiffRow::Hunk { hunk, line } = *row {
                let line = &view.file.hunks[hunk].lines[line];
                if line.kind == LineKind::Removed {
                    at = Some(removed.len());
                    removed.push(line.text.clone());
                }
            }
            removed_at.push(at);
            let len = row_text(&view, row, &removed, at).chars().count();
            if len > widest_len {
                (widest, widest_len) = (ix, len);
            }
        }
        Self {
            view: Arc::new(view),
            removed: Arc::new(removed),
            removed_at,
            new_spans: None,
            removed_spans: None,
            headers,
            widest,
        }
    }
}

/// 一行的原文：块外和块里没改的、加的行用新文件的全文（没读全文时用 diff 里的）；删掉的行用 diff
/// 里的；块头是块头。
fn row_text<'a>(view: &'a DiffView, row: &DiffRow, removed: &'a [String], at: Option<usize>) -> &'a str {
    match *row {
        DiffRow::Header(hunk) => &view.file.hunks[hunk].header,
        DiffRow::Context { new, .. } => view.new_line(new).unwrap_or_default(),
        DiffRow::Hunk { hunk, line } => {
            let line = &view.file.hunks[hunk].lines[line];
            match (at, line.new) {
                (Some(at), _) => &removed[at],
                (None, Some(new)) => view.new_line(new).unwrap_or(&line.text),
                (None, None) => &line.text,
            }
        }
    }
}

impl WindowView {
    /// Git 面板里点了改动的文件：按配置的 `PreviewClick` 在预览栏里打开它的 diff，单击打开时单击开成
    /// 临时标签、双击固定；双击打开时双击开成固定标签。
    pub(in crate::window) fn click_diff(&mut self, target: DiffTarget, clicks: usize, cx: &mut Context<Self>) {
        let single = cx.global::<AppConfig>().0.file_tree_preview_click == PreviewClick::Single;
        if clicks >= 2 {
            self.open_diff(target, true, cx);
        } else if single {
            self.open_diff(target, false, cx);
        }
        cx.notify();
    }

    fn open_diff(&mut self, target: DiffTarget, pin: bool, cx: &mut Context<Self>) {
        let path = target.root.join(&target.rel);
        self.open_tab(&path, Some(target), pin, cx);
    }

    /// 扫描结果换上以后，当前显示的 diff 标签对应的 git 状态变了就重读；别的标签等切过去时再读。
    pub(in crate::window) fn reload_stale_diff(&mut self, cx: &mut Context<Self>) {
        if self.preview().is_some_and(|preview| preview.diff.is_some() && preview.diff_stale) {
            self.load_diff(cx);
        }
    }

    /// 在后台读当前 diff 标签的整篇 diff，读完换上，接着在后台高亮。第一次读时滚到第一处改动，
    /// 重读时滚动位置不动。
    pub(super) fn load_diff(&mut self, cx: &mut Context<Self>) {
        let id = self.workspace().id;
        let project = &self.workspace().project;
        let Some(target) = project.previews.active().and_then(|preview| preview.diff.clone()) else {
            return;
        };
        let repo = project
            .git
            .as_ref()
            .and_then(|git| git.iter().find(|repo| repo.root == target.root))
            .map(git::Snapshot::repo);
        let Some(preview) = self.preview_mut() else {
            return;
        };
        preview.diff_stale = false;
        preview.cancel.store(true, Ordering::Relaxed);
        preview.cancel = Arc::new(AtomicBool::new(false));
        let cancel = preview.cancel.clone();
        let path = preview.path.clone();
        let job = cx.background_spawn(async move {
            repo.and_then(|repo| repo.file_view(&target.rel, target.old_rel.as_deref(), &target.side).ok().flatten())
        });
        cx.spawn(async move |this, cx| {
            let view = job.await;
            let texts = this
                .update(cx, |this, cx| {
                    let (preview, git) = this.preview_for(id, &cancel)?;
                    // 和上次读到的一样（比如按块暂存后马上读了一次，扫描后又读一次）就留着，不重新高亮。
                    if let (Some(Loaded::Diff(old)), Some(view)) = (&preview.content, &view)
                        && *old.view == *view
                    {
                        return None;
                    }
                    let first = !matches!(preview.content, Some(Loaded::Diff(_)));
                    let (old_new, old_removed) = match preview.content.take() {
                        Some(Loaded::Diff(old)) => (old.new_spans, old.removed_spans),
                        _ => (None, None),
                    };
                    let content = match view {
                        None => Loaded::Note(Note::NoChanges),
                        Some(view) if view.file.binary => Loaded::Note(Note::Binary),
                        Some(view) if view.file.truncated => Loaded::Note(Note::DiffTooLarge),
                        // 没有块又没读到全文（空文件只改了权限这类）才说内容没变；只改了名的读得到全文，照常显示。
                        Some(view) if view.file.hunks.is_empty() && view.new_lines.is_empty() => {
                            Loaded::Note(Note::NoContent)
                        }
                        Some(view) => {
                            let mut content = DiffContent::new(view);
                            (content.new_spans, content.removed_spans) = (old_new, old_removed);
                            if first && let Some(&header) = content.headers.first() {
                                preview.scroll.scroll_to_item(header, ScrollStrategy::Top);
                            }
                            Loaded::Diff(content)
                        }
                    };
                    let texts = match &content {
                        Loaded::Diff(content) => {
                            Some((Arc::new(content.view.new_lines.clone()), content.removed.clone()))
                        }
                        _ => None,
                    };
                    preview.selection = None;
                    preview.content = Some(content);
                    preview.refresh_marks(git);
                    cx.notify();
                    texts
                })
                .ok()
                .flatten();
            let Some((new_lines, removed)) = texts else {
                return;
            };
            let (new_spans, removed_spans) = cx
                .background_spawn({
                    let cancel = cancel.clone();
                    async move {
                        let new = runode_preview::highlight(&path, &new_lines, &cancel);
                        let removed = runode_preview::highlight(&path, &removed, &cancel);
                        (new, removed)
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                if let Some((preview, _)) = this.preview_for(id, &cancel)
                    && let Some(Loaded::Diff(content)) = &mut preview.content
                {
                    content.new_spans = new_spans.map(Arc::new);
                    content.removed_spans = removed_spans.map(Arc::new);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// diff 标签的正文：顶上一条写着加减了多少行、能跳到上一处和下一处改动，下面是各行。
    pub(super) fn render_diff_body(
        &self,
        content: &DiffContent,
        font: SharedString,
        font_size: f32,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(preview) = self.preview() else {
            return div().into_any_element();
        };
        let view = &content.view;
        let lines = view.new_lines.len().max(view.file.hunks.iter().map(|hunk| hunk.lines.len()).sum());
        // 行号两栏按位数定宽，等宽字体一个数字大约 0.6 个字号宽。
        let number = (lines.max(1).to_string().len() as f32 * font_size * 0.62 + 10.).ceil();
        let row_height = font_size + ROW_EXTRA_HEIGHT;
        let list = uniform_list(
            "preview-diff",
            view.rows.len(),
            cx.processor(move |this, range: Range<usize>, _, cx| {
                this.render_diff_rows(range, row_height, number, fg, bg, cx)
            }),
        )
        .with_width_from_item(Some(content.widest))
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
        .track_scroll(&preview.scroll)
        .size_full()
        .py(px(BODY_PADDING))
        .font_family(font);
        let handle = preview.scroll.0.borrow().base_handle.clone();
        let fade = right_fade(&handle, bg);
        let jump = |id: &'static str, icon: &'static str, key: &'static str, forward: bool| {
            div()
                .id(id)
                .flex_none()
                .size(px(20.))
                .rounded(px(3.))
                .flex()
                .items_center()
                .justify_center()
                .hover(|button| button.bg(hsla(bg.mix(fg, 0.12))))
                .tooltip(tooltip(rust_i18n::t!(key), None, fg, bg))
                .child(svg().path(icon).size(px(14.)).text_color(hsla(fg).opacity(0.75)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.jump_to_change(forward, row_height, cx);
                    }),
                )
        };
        let toolbar = div()
            .flex_none()
            .h(px(TOOLBAR_HEIGHT))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(12.))
            .border_b_1()
            .border_color(divider_color(hsla(fg)))
            .child(added_label(view.file.added))
            .child(removed_label(view.file.removed))
            .child(div().flex_1())
            .child(jump("diff-previous", ARROW_UP_ICON, "preview.diff.previous_change", false))
            .child(jump("diff-next", ARROW_DOWN_ICON, "preview.diff.next_change", true));
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(list)
                    .children(fade)
                    .child(scrollbar("preview-diff-scroll-y", handle.clone(), Axis::Vertical, hsla(fg)))
                    .child(scrollbar("preview-diff-scroll-x", handle, Axis::Horizontal, hsla(fg))),
            )
            .into_any_element()
    }

    /// 滚到下一处（`forward`）或上一处改动的块头。
    fn jump_to_change(&mut self, forward: bool, row_height: f32, cx: &mut Context<Self>) {
        let Some(preview) = self.preview() else {
            return;
        };
        let Some(Loaded::Diff(content)) = &preview.content else {
            return;
        };
        let offset = -f32::from(preview.scroll.0.borrow().base_handle.offset().y) - BODY_PADDING;
        let top = (offset.max(0.) / row_height).round() as usize;
        let target = if forward {
            content.headers.iter().find(|&&ix| ix > top)
        } else {
            content.headers.iter().rev().find(|&&ix| ix < top)
        };
        if let Some(&ix) = target {
            preview.scroll.scroll_to_item(ix, ScrollStrategy::Top);
            cx.notify();
        }
    }

    /// 块头上的按钮：暂存、丢掉或取消暂存这一块。丢掉先问一下。用的是这次读到的块，文件在这之后又
    /// 改过时 git 认不出来，报错后跟着扫描重读即可。
    fn diff_hunk_action(&mut self, hunk: usize, action: HunkAction, window: &mut Window, cx: &mut Context<Self>) {
        let Some(preview) = self.preview() else {
            return;
        };
        let (Some(target), Some(Loaded::Diff(content))) = (&preview.diff, &preview.content) else {
            return;
        };
        let (root, file) = (target.root.clone(), content.view.file.clone());
        let busy = if action == HunkAction::Discard { Busy::Discard } else { Busy::Stage };
        let run = move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
            // 做完马上重读，不等扫描，接着点下一块时块还对得上。
            let op = move |repo: &git::Repo| repo.apply_hunk(&file, hunk, action);
            this.run_git(&root, busy, window, cx, op, |this, (), _, cx| this.load_diff(cx));
        };
        if action != HunkAction::Discard {
            run(self, window, cx);
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            &rust_i18n::t!("preview.diff.discard_hunk_title"),
            Some(&rust_i18n::t!("git.irreversible")),
            &[&*rust_i18n::t!("git.discard_confirm"), &*rust_i18n::t!("git.cancel")],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() == Some(0) {
                this.update_in(cx, run).ok();
            }
        })
        .detach();
    }

    /// 这个 diff 的块能不能按块操作：工作区和暂存区里普通文本文件的修改可以；冲突的文件、gitlink、
    /// 新增删除改名的文件和提交里的不行。
    fn diff_hunks_actionable(&self, target: &DiffTarget, view: &DiffView) -> bool {
        if matches!(target.side, DiffSide::Commit { .. }) || !hunk_actionable(&view.file) {
            return false;
        }
        let git = self.workspace().project.git.as_ref();
        let repo = git.and_then(|git| git.iter().find(|repo| repo.root == target.root));
        let conflicted = repo.is_some_and(|repo| {
            repo.unstaged.iter().any(|file| file.path == target.rel && file.status == FileStatus::Conflicted)
        });
        !conflicted
    }

    /// diff 的各行：块头带按块操作的按钮；改动的行垫上红绿底色，左边旧行号、新行号和正负号。双击
    /// 工作区 diff 里没删掉的行，把「路径:新行号」打进终端。
    fn render_diff_rows(
        &self,
        range: Range<usize>,
        row_height: f32,
        number: f32,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(preview) = self.preview() else {
            return Vec::new();
        };
        let (Some(target), Some(Loaded::Diff(content))) = (&preview.diff, &preview.content) else {
            return Vec::new();
        };
        let view = &content.view;
        let actionable = self.diff_hunks_actionable(target, view);
        let palette = ansi_palette(cx);
        let dim = hsla(fg).opacity(0.4);
        let worktree = target.side == DiffSide::Worktree;
        let full = preview.path.clone();
        range
            .filter_map(|ix| view.rows.get(ix).map(|row| (ix, *row)))
            .map(|(ix, row)| {
                let base = div().flex_none().h(px(row_height)).w_full().flex().items_center().whitespace_nowrap();
                if let DiffRow::Header(hunk) = row {
                    return self.render_diff_header(ix, hunk, actionable, target, base, fg, bg, cx);
                }
                let (kind, old, new) = match row {
                    DiffRow::Context { old, new } => (LineKind::Context, Some(old), Some(new)),
                    DiffRow::Hunk { hunk, line } => {
                        let line = &view.file.hunks[hunk].lines[line];
                        (line.kind, line.old, line.new)
                    }
                    DiffRow::Header(_) => unreachable!("块头在上面画了"),
                };
                let at = content.removed_at[ix];
                let text = row_text(view, &row, &content.removed, at);
                // 新文件的行用全文的高亮；删掉的行用它们连起来的高亮；没读全文时不高亮。
                let spans = match (at, new) {
                    (Some(at), _) => content.removed_spans.as_ref().and_then(|spans| spans.get(at)),
                    (None, Some(new)) if view.new_line(new).is_some() => content
                        .new_spans
                        .as_ref()
                        .and_then(|spans| spans.get(usize::try_from(new).ok()?.checked_sub(1)?)),
                    _ => None,
                };
                let shown = runode_preview::display_line(text, spans.map_or(&[][..], Vec::as_slice), MAX_COLUMNS);
                let runs: Vec<_> = shown
                    .spans
                    .iter()
                    .map(|span| (span.range.clone(), highlight_style(span.style, fg, &palette)))
                    .collect();
                let mut text = shown.text;
                if shown.cut {
                    text.push('…');
                }
                let (sign, tint) = match kind {
                    LineKind::Added => ("+", Some(bg.mix(ADDED, 0.16))),
                    LineKind::Removed => ("-", Some(bg.mix(REMOVED, 0.16))),
                    LineKind::Context => (" ", None),
                };
                let number_cell = |n: Option<u32>| {
                    div()
                        .flex_none()
                        .w(px(number))
                        .pr(px(6.))
                        .flex()
                        .justify_end()
                        .text_color(dim)
                        .children(n.map(|n| n.to_string()))
                };
                let jump = (worktree && kind != LineKind::Removed).then_some(new).flatten();
                let full = full.clone();
                base.id(("preview-diff-line", ix))
                    .when_some(tint, |row, tint| row.bg(hsla(tint)))
                    .child(number_cell(old))
                    .child(number_cell(new))
                    .child(div().flex_none().w(px(14.)).text_color(dim).child(sign))
                    .child(
                        div()
                            .flex_none()
                            .pr(px(12.))
                            .text_color(hsla(fg))
                            .child(StyledText::new(text).with_highlights(runs)),
                    )
                    .when_some(jump, |row, line| {
                        row.on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                if event.click_count >= 2 {
                                    this.insert_path(&full, Some(line), window, cx);
                                }
                            }),
                        )
                    })
                    .into_any_element()
            })
            .collect()
    }

    /// 一块改动的块头，能按块操作时右边有按钮：工作区的是丢弃和暂存这一块，暂存区的是取消暂存。
    #[allow(clippy::too_many_arguments)]
    fn render_diff_header(
        &self,
        ix: usize,
        hunk: usize,
        actionable: bool,
        target: &DiffTarget,
        row: gpui::Div,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(Loaded::Diff(content)) = self.preview().and_then(|preview| preview.content.as_ref()) else {
            return row.into_any_element();
        };
        let header = content.view.file.hunks[hunk].header.clone();
        let buttons: Vec<(&'static str, &'static str, HunkAction)> = match (&target.side, actionable) {
            (DiffSide::Worktree, true) => vec![
                (DISCARD_ICON, "git.discard_hunk", HunkAction::Discard),
                (PLUS_ICON, "git.stage_hunk", HunkAction::Stage),
            ],
            (DiffSide::Index, true) => vec![(MINUS_ICON, "git.unstage_hunk", HunkAction::Unstage)],
            _ => Vec::new(),
        };
        let fg_hsla = hsla(fg);
        row.id(("preview-diff-header", ix))
            .px(px(8.))
            .gap(px(6.))
            .bg(hsla(bg.mix(fg, 0.05)))
            .child(div().flex_1().min_w_0().truncate().text_color(fg_hsla.opacity(0.5)).child(header))
            .children(buttons.into_iter().enumerate().map(|(bi, (icon, key, action))| {
                div()
                    .id(("preview-diff-hunk-button", ix * 4 + bi))
                    .flex_none()
                    .px(px(6.))
                    .h(px(18.))
                    .rounded(px(3.))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .text_size(px(11.))
                    .text_color(fg_hsla.opacity(0.8))
                    .hover(|button| button.bg(hsla(bg.mix(fg, 0.12))))
                    .child(svg().path(icon).size(px(12.)).text_color(fg_hsla.opacity(0.75)))
                    .child(rust_i18n::t!(key).into_owned())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.diff_hunk_action(hunk, action, window, cx);
                        }),
                    )
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window::preview::PreviewTabs;
    use runode_git::{FileDiff, Hunk, Line};
    use std::path::Path;

    fn view() -> DiffView {
        let line = |kind, old, new, text: &str| Line { kind, old, new, text: text.into() };
        let file = FileDiff {
            path: "a.rs".into(),
            old_path: None,
            status: FileStatus::Modified,
            added: 1,
            removed: 1,
            hunks: vec![Hunk {
                header: "@@ -1,2 +1,2 @@".into(),
                lines: vec![
                    line(LineKind::Removed, Some(1), None, "old"),
                    line(LineKind::Added, None, Some(1), "a much longer new line"),
                    line(LineKind::Context, Some(2), Some(2), "b"),
                ],
            }],
            binary: false,
            truncated: false,
            gitlink: false,
        };
        let new_lines = vec!["a much longer new line".to_owned(), "b".to_owned(), "c".to_owned()];
        let rows = git::merge_rows(&file, &new_lines).unwrap();
        DiffView { file, new_lines, rows }
    }

    #[test]
    fn indexes_removed_lines_and_headers() {
        let content = DiffContent::new(view());
        assert_eq!(content.headers, [0]);
        assert_eq!(content.removed.as_slice(), ["old"]);
        assert_eq!(content.removed_at, [None, Some(0), None, None, None]);
        assert_eq!(content.widest, 2);
        let view = &content.view;
        assert_eq!(row_text(view, &view.rows[4], &content.removed, None), "c");
    }

    #[test]
    fn only_this_files_changes_make_the_tab_stale() {
        let diff = |path: &str, added| FileDiff { path: path.into(), added, ..view().file };
        let repos = |files: Vec<FileDiff>| {
            git::Repos::new(git::Snapshot {
                root: "/r".into(),
                git_dir: "/r/.git".into(),
                prefix: PathBuf::new(),
                kind: git::RepoKind::Main,
                staged: Vec::new(),
                unstaged: files,
                statuses: Default::default(),
                ignored: Default::default(),
                info: Default::default(),
            })
        };
        let target = DiffTarget { root: "/r".into(), rel: "a.rs".into(), old_rel: None, side: DiffSide::Worktree };
        let mut tab = super::super::Preview::new("/r/a.rs".into(), Some(target), true);
        let old = repos(vec![diff("a.rs", 1), diff("b.rs", 1)]);
        tab.git_changed(Some(&old), Some(&repos(vec![diff("a.rs", 1), diff("b.rs", 2)])));
        assert!(!tab.diff_stale, "别的文件变了不重读");
        tab.git_changed(Some(&old), Some(&repos(vec![diff("a.rs", 2), diff("b.rs", 1)])));
        assert!(tab.diff_stale);
        // diff 标签不跟着文件事件重读，等扫描。
        assert!(!tab.affected_by(&["/r/a.rs".into()]));
    }

    #[test]
    fn diff_tabs_are_separate_from_file_tabs() {
        let mut previews = PreviewTabs::default();
        let target = DiffTarget { root: "/r".into(), rel: "a.rs".into(), old_rel: None, side: DiffSide::Worktree };
        let staged = DiffTarget { side: DiffSide::Index, ..target.clone() };
        previews.open(Path::new("/r/a.rs"), None, true);
        previews.open(Path::new("/r/a.rs"), Some(target.clone()), true);
        previews.open(Path::new("/r/a.rs"), Some(staged), true);
        assert_eq!(previews.tabs.len(), 3);
        previews.open(Path::new("/r/a.rs"), Some(target), false);
        assert_eq!((previews.tabs.len(), previews.active), (3, 1));
    }
}
