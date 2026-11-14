//! 「看改动并提交」那一页（Review 页）：盖在终端区上，把当前项目主仓库和子仓库里的改动按文件排成卡片，
//! 说明和提交在右侧面板的改动页。
//!
//! 每个文件一张卡片：卡头是复选框（勾上的是整个暂存了的，半勾是暂存了一部分）、路径、增删的行数、打开文件、
//! 放弃整个文件和收起；下面是这个文件暂存区和工作区两段的块合在一起排（`review_hunks`）。暂存区的块勾着，
//! 取消是撤回暂存；工作区的块没勾，勾上是暂存；放弃只对工作区的块。不能按块操作的文件（`by_hunks` 为假）
//! 只有卡头，点了在预览栏里看。
//!
//! 列表的一项是一张卡片的卡头或卡片里的一块，卡片的边框由各项拼起来。刚提交好的仓库顶上一条横幅说提交了几个
//! 文件，没提交的照旧排着。↑↓ 跳到上一块、下一块，Esc 请窗口收起这一页（`ReviewEvent::Close`）。
//!
//! 这一页是单独的视图：git 状态、在忙的仓库和刚提交好的那几次经 `set_input` 交进来，要窗口办的事经
//! `ReviewEvent` 交出去。

use std::{
    collections::{BTreeMap, HashSet},
    iter,
    path::{Path, PathBuf},
    sync::Arc,
};

use gpui::{
    AnyElement, App, Context, Div, EventEmitter, FocusHandle, Focusable, FontWeight, KeyDownEvent, ListAlignment,
    ListOffset, ListState, Render, Role, SharedString, Stateful, Toggled, Window, div, list, prelude::*, px, svg,
};
use runode_git::{self as git, DiffSide, FileDiff, FileStatus, HunkAction, LineKind, Section, hunk_actionable};

use crate::{
    git_panel::{Committed, FileOp, GitFileAction, changed_range, file_checked},
    git_view::{repo_change_summary, same_repos, status_tint},
    i18n::action_for,
    preview_panel::{DiffTarget, MAX_COLUMNS, ask_discard_hunk, hunk_name, hunk_quote, hunk_title},
    ui::{
        a11y::{Disable as _, Press as _, PressDown as _, StaticText as _},
        base_name,
        button::{Button, ButtonSize, ButtonVariant},
        checkbox::Checkbox,
        font,
        font_size::{FontSize, scaled},
        hsla,
        icons::{
            ARROW_DOWN_ICON, ARROW_UP_ICON, CHECK_ICON, CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON, DISCARD_ICON,
            OPEN_FILE_ICON, SPARKLE_ICON,
        },
        kbd::Kbd,
        panel::{added_label, panel_message, removed_label},
        scrollbar::list_scrollbar,
        theme::{ADDED, ActiveTheme as _, REMOVED},
        tooltip::tooltip,
    },
};

/// 列表多画出视口的高度，滚动时不露白。
const LIST_OVERDRAW: f32 = 400.;
/// 顶上一条、卡头、块头和代码各行的高度，按基准字号的点数。
const TOOLBAR_HEIGHT: f32 = 38.;
const CARD_HEADER_HEIGHT: f32 = 36.;
const HUNK_HEADER_HEIGHT: f32 = 28.;
const LINE_HEIGHT: f32 = 20.;
/// 旧行号、新行号两栏各多宽，按基准字号的点数。
const NUMBER_WIDTH: f32 = 40.;
/// 卡片之间、卡片和页边的空隙。
const GAP: f32 = 10.;
const CARD_RADIUS: f32 = 10.;

/// 窗口每次画这一页之前交进来的东西。
#[derive(Clone, Default)]
pub(crate) struct Input {
    /// 当前项目最近读到的 git 状态，不在仓库里时为空。
    pub git: Option<Arc<git::Repos>>,
    /// 有操作在跑的仓库的根目录：这些仓库的复选框和按钮按不动。
    pub busy: Vec<PathBuf>,
    /// 刚提交好、还没有新改动的那几次，带着各自仓库的根目录。
    pub committed: Vec<(PathBuf, Committed)>,
}

impl Input {
    fn same(&self, other: &Self) -> bool {
        same_repos(&self.git, &other.git) && self.busy == other.busy && self.committed == other.committed
    }
}

/// 要窗口办的事。
pub(crate) enum ReviewEvent {
    /// 收起这一页，回到终端。
    Close,
    /// 在预览栏里打开这个文件。
    OpenFile(PathBuf),
    /// 在预览栏里看这个 diff：不能按块操作的文件点了卡头。
    OpenDiff(DiffTarget),
    /// 暂存、撤回或放弃整个文件。
    File(GitFileAction),
    /// 对根目录是 `root` 的仓库里 `file` 的第 `hunk` 块做 `action`；放弃的已经问过了。
    ApplyHunk { root: PathBuf, file: FileDiff, hunk: usize, action: HunkAction },
    /// 把一块改动连同路径和行号交给 agent。
    Quote(String),
}

/// 卡片里的一块：`staged` 时是暂存区那段的，否则是工作区那段的；`index` 是它在那段 `FileDiff::hunks` 里的下标。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReviewHunk {
    pub staged: bool,
    pub index: usize,
}

/// 一个文件暂存区和工作区两段的块合在一起，按块在新文件里的起始行排，同一行起的暂存区的在前。两段的行号各按
/// 自己那边的新文件数，排出来的先后是大致的。
pub(crate) fn review_hunks(staged: Option<&FileDiff>, unstaged: Option<&FileDiff>) -> Vec<ReviewHunk> {
    let side = |file: Option<&FileDiff>, staged: bool| -> Vec<(u32, ReviewHunk)> {
        file.into_iter()
            .flat_map(|file| file.hunks.iter().enumerate())
            .map(|(index, hunk)| (hunk_start(hunk), ReviewHunk { staged, index }))
            .collect()
    };
    let mut hunks = side(staged, true);
    hunks.extend(side(unstaged, false));
    hunks.sort_by_key(|(start, hunk)| (*start, !hunk.staged));
    hunks.into_iter().map(|(_, hunk)| hunk).collect()
}

/// 块从新文件的第几行起；只删了行、没有上下文的块用旧文件的。
fn hunk_start(hunk: &git::Hunk) -> u32 {
    let new = hunk.lines.iter().find_map(|line| line.new);
    new.or_else(|| hunk.lines.iter().find_map(|line| line.old)).unwrap_or(0)
}

/// 这个文件能不能按块操作：两段里有的都要是 `hunk_actionable` 的，也没被截断。
fn by_hunks(staged: Option<&FileDiff>, unstaged: Option<&FileDiff>) -> bool {
    let sides = [staged, unstaged];
    sides.iter().any(Option::is_some)
        && sides.into_iter().flatten().all(|file| hunk_actionable(file) && !file.truncated)
}

/// 不能按块操作的文件卡头上写的原因（文案键），按暂存区、工作区的先后取第一个有的。
fn file_note(staged: Option<&FileDiff>, unstaged: Option<&FileDiff>) -> Option<&'static str> {
    [staged, unstaged].into_iter().flatten().find_map(|file| match file.status {
        FileStatus::Conflicted => Some("review.conflicted"),
        FileStatus::Added | FileStatus::Untracked => Some("review.new_file"),
        FileStatus::Deleted => Some("review.deleted"),
        FileStatus::Renamed => Some("review.renamed"),
        FileStatus::Modified if file.binary => Some("review.binary"),
        FileStatus::Modified if file.truncated => Some("review.too_large"),
        FileStatus::Modified => None,
    })
}

/// 主仓库和子仓库，主仓库在前；其他工作树不算。
fn repos(git: &git::Repos) -> impl Iterator<Item = &git::Snapshot> {
    iter::once(&git.main).chain(&git.subs)
}

/// 列表里的一个文件：`repo` 是 `repos` 里的第几个，`staged`、`unstaged` 是它在那个仓库两段里的下标。
#[derive(Clone, Debug, PartialEq, Eq)]
struct FileRef {
    repo: usize,
    path: PathBuf,
    staged: Option<usize>,
    unstaged: Option<usize>,
}

impl FileRef {
    fn sides<'a>(&self, repo: &'a git::Snapshot) -> (Option<&'a FileDiff>, Option<&'a FileDiff>) {
        (self.staged.and_then(|ix| repo.staged.get(ix)), self.unstaged.and_then(|ix| repo.unstaged.get(ix)))
    }
}

/// 仓库 `repo`（`repos` 里的第 `ri` 个）里有改动的文件，按路径排，两段里都有的只算一个。
fn repo_files(ri: usize, repo: &git::Snapshot) -> Vec<FileRef> {
    let mut files: BTreeMap<&Path, (Option<usize>, Option<usize>)> = BTreeMap::new();
    for (ix, file) in repo.staged.iter().enumerate() {
        files.entry(&file.path).or_default().0 = Some(ix);
    }
    for (ix, file) in repo.unstaged.iter().enumerate() {
        files.entry(&file.path).or_default().1 = Some(ix);
    }
    files
        .into_iter()
        .map(|(path, (staged, unstaged))| FileRef { repo: ri, path: path.to_owned(), staged, unstaged })
        .collect()
}

/// 列表的一项。
#[derive(Clone, Debug, PartialEq, Eq)]
enum Item {
    /// 卡头；`open` 时下面跟着块，卡片的底边由最后一块画。
    File { file: FileRef, open: bool },
    /// 卡片里的一块，`last` 是卡片的最后一块。
    Hunk { file: FileRef, hunk: ReviewHunk, last: bool },
}

/// Review 页。根元素铺满，不画底色。
pub(crate) struct Review {
    focus: FocusHandle,
    input: Input,
    /// 列表的各项，和 `list` 一一对应。
    items: Vec<Item>,
    list: ListState,
    /// 收起的文件，按（仓库的根目录，相对它的路径）。
    collapsed: HashSet<(PathBuf, PathBuf)>,
}

impl EventEmitter<ReviewEvent> for Review {}

impl Focusable for Review {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Review {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            input: Input::default(),
            items: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, px(LIST_OVERDRAW)),
            collapsed: HashSet::new(),
        }
    }

    /// 换上窗口交来的东西，变了时返回真，git 状态变了时重排列表。只记下来，不重画：窗口在画的时候调。
    pub(crate) fn set_input(&mut self, input: Input) -> bool {
        if self.input.same(&input) {
            return false;
        }
        let relayout = !same_repos(&self.input.git, &input.git);
        self.input = input;
        if relayout {
            self.relayout();
        }
        true
    }

    /// 界面字号变了：列表记着量过的高度，要重新量。
    pub(crate) fn remeasure(&self) {
        self.list.remeasure();
    }

    /// 按 git 状态和收起的文件重排列表的项：只换掉前后相同部分中间的那一段，滚动位置留着。
    fn relayout(&mut self) {
        let items = self.build_items();
        let changed = changed_range(&self.items, &items);
        self.list.splice(changed.old, changed.new_len);
        self.items = items;
        // 同一块里改了一行时项没变、高度可能变了，都重新量。
        self.list.remeasure_items(0..self.items.len());
    }

    fn build_items(&self) -> Vec<Item> {
        let Some(git) = &self.input.git else { return Vec::new() };
        let mut items = Vec::new();
        for (ri, repo) in repos(git).enumerate() {
            for file in repo_files(ri, repo) {
                let (staged, unstaged) = file.sides(repo);
                let collapsed = self.collapsed.contains(&(repo.root.clone(), file.path.clone()));
                let hunks =
                    if by_hunks(staged, unstaged) && !collapsed { review_hunks(staged, unstaged) } else { Vec::new() };
                let count = hunks.len();
                items.push(Item::File { file: file.clone(), open: count > 0 });
                items.extend(hunks.into_iter().enumerate().map(|(ix, hunk)| Item::Hunk {
                    file: file.clone(),
                    hunk,
                    last: ix + 1 == count,
                }));
            }
        }
        items
    }

    /// `file` 所在的仓库。
    fn repo_of(&self, file: &FileRef) -> Option<&git::Snapshot> {
        repos(self.input.git.as_deref()?).nth(file.repo)
    }

    fn busy(&self, root: &Path) -> bool {
        self.input.busy.iter().any(|busy| busy == root)
    }

    fn toggle_collapsed(&mut self, file: &FileRef, cx: &mut Context<Self>) {
        let Some(root) = self.repo_of(file).map(|repo| repo.root.clone()) else { return };
        let key = (root, file.path.clone());
        if !self.collapsed.remove(&key) {
            self.collapsed.insert(key);
        }
        self.relayout();
        cx.notify();
    }

    /// 滚到下一块（`forward`）或上一块的块头。
    fn jump(&mut self, forward: bool, cx: &mut Context<Self>) {
        let top = self.list.logical_scroll_top();
        let mut hunks = self.items.iter().enumerate().filter(|(_, item)| matches!(item, Item::Hunk { .. }));
        let target = if forward {
            hunks.find(|&(ix, _)| ix > top.item_ix)
        } else {
            // 顶上那块已经滚上去了一截时，上一块是它自己。
            hunks.rev().find(|&(ix, _)| ix < top.item_ix || (ix == top.item_ix && top.offset_in_item > px(0.)))
        };
        if let Some((ix, _)) = target {
            self.list.scroll_to(ListOffset { item_ix: ix, offset_in_item: px(0.) });
            cx.notify();
        }
    }

    /// 对 `file` 里的块 `hunk` 做 `action`；放弃先问一下。用的是这次读到的块，文件在这之后又改过时 git 认不出来，
    /// 报错后跟着扫描重读即可。
    fn hunk_action(
        &mut self,
        file: &FileRef,
        hunk: ReviewHunk,
        action: HunkAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(repo) = self.repo_of(file) else { return };
        let (staged, unstaged) = file.sides(repo);
        let Some(diff) = (if hunk.staged { staged } else { unstaged }) else { return };
        let event = ReviewEvent::ApplyHunk { root: repo.root.clone(), file: diff.clone(), hunk: hunk.index, action };
        if action != HunkAction::Discard {
            cx.emit(event);
            return;
        }
        let answer = ask_discard_hunk(window, cx);
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() == Some(0) {
                this.update(cx, |_, cx| cx.emit(event)).ok();
            }
        })
        .detach();
    }

    /// 「交给 agent」：这一块连同路径和行号。交给的 agent 可能在远端，路径照那边的写法。
    fn quote(&mut self, file: &FileRef, hunk: ReviewHunk, cx: &mut Context<Self>) {
        let Some(repo) = self.repo_of(file) else { return };
        let (staged, unstaged) = file.sides(repo);
        let diff = if hunk.staged { staged } else { unstaged };
        if let Some(found) = diff.and_then(|diff| diff.hunks.get(hunk.index)) {
            let path = crate::project_host::remote_path(&repo.root.join(&file.path));
            cx.emit(ReviewEvent::Quote(hunk_quote(&path, found)));
        }
    }
}

impl Render for Review {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let fg = theme.foreground;
        let title = rust_i18n::t!("review.title").into_owned();
        let toolbar = self.render_toolbar(cx);
        let banners = self.render_banners(cx);
        let body = if self.items.is_empty() {
            panel_message(rust_i18n::t!("review.none_left").into_owned(), cx).into_any_element()
        } else {
            div()
                .id("review-files")
                .role(Role::Group)
                .aria_label(title.clone())
                .flex_1()
                .min_h_0()
                .relative()
                .child(
                    list(
                        self.list.clone(),
                        cx.processor(|this, ix: usize, window, cx| this.render_item(ix, window, cx)),
                    )
                    .size_full(),
                )
                .child(list_scrollbar("review-files-scroll", self.list.clone()))
                .into_any_element()
        };
        div()
            .id("review-page")
            .role(Role::Group)
            .aria_label(title)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.modifiers.modified() {
                    return;
                }
                match event.keystroke.key.as_str() {
                    "escape" => cx.emit(ReviewEvent::Close),
                    "up" => this.jump(false, cx),
                    "down" => this.jump(true, cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .size_full()
            .flex()
            .flex_col()
            .text_color(fg)
            .text_size(FontSize::Body)
            .child(toolbar)
            .children(banners)
            .child(body)
    }
}

impl Review {
    /// 顶上一条：改动了几个文件、一共增删几行，右边是跳到上一块、下一块和回到终端。
    fn render_toolbar(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let (border, muted) = (theme.border, theme.muted_foreground);
        let (files, added, removed) = self.input.git.as_deref().map_or((0, 0, 0), |git| {
            repos(git)
                .map(repo_change_summary)
                .fold((0, 0, 0), |sum, repo| (sum.0 + repo.0, sum.1 + repo.1, sum.2 + repo.2))
        });
        let files_text = rust_i18n::t!("review.files", count = files).into_owned();
        let mono = font::mono(cx);
        let summary = div()
            .id("review-summary")
            .aria_static_text(format!("{files_text} +{added} −{removed}"))
            .flex()
            .items_center()
            .gap(px(10.))
            .child(div().text_size(FontSize::Small).text_color(muted).child(files_text))
            .child(
                div()
                    .flex()
                    .gap(px(7.))
                    .text_size(FontSize::Small)
                    .when_some(mono, |stats, family| stats.font_family(family))
                    .child(added_label(added))
                    .child(removed_label(removed)),
            );
        let mut jump = |id: &'static str, icon: &'static str, key: &'static str, forward: bool| {
            Button::new(id, rust_i18n::t!(key).into_owned())
                .icon(icon)
                .with_size(ButtonSize::Icon)
                .tooltip(tooltip(rust_i18n::t!(key), None))
                .on_press(cx, move |this, _, cx| this.jump(forward, cx))
        };
        let previous = jump("review-previous", ARROW_UP_ICON, "preview.diff.previous_change", false);
        let next = jump("review-next", ARROW_DOWN_ICON, "preview.diff.next_change", true);
        let back_label = rust_i18n::t!("review.back").into_owned();
        let back = Button::new("review-back", back_label.clone())
            .variant(ButtonVariant::Ghost)
            .h(scaled(26.))
            .ml(px(6.))
            .pl(px(9.))
            .pr(px(6.))
            .gap(px(7.))
            .child(back_label)
            .child(Kbd::new("esc"))
            .on_press(cx, |_, _, cx| cx.emit(ReviewEvent::Close));
        div()
            .flex_none()
            .h(scaled(TOOLBAR_HEIGHT))
            .pl(px(14.))
            .pr(px(8.))
            .flex()
            .items_center()
            .gap(px(10.))
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .id("review-title")
                    .role(Role::Heading)
                    .aria_label(rust_i18n::t!("review.title").into_owned())
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(rust_i18n::t!("review.title").into_owned()),
            )
            .child(summary)
            .child(div().flex_1())
            .child(previous)
            .child(next)
            .child(back)
    }

    /// 刚提交好的仓库各一条横幅：提交了几个文件、说明的第一行和短哈希。
    fn render_banners(&self, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        let theme = cx.theme();
        let (success, muted, subtle) = (theme.success, theme.muted_foreground, theme.subtle_foreground);
        let mono = font::mono(cx);
        let separator = rust_i18n::t!("a11y.separator").into_owned();
        (self.input.committed.iter().filter(|(_, committed)| committed.files > 0))
            .map(|(root, committed)| {
                let title = rust_i18n::t!("review.committed", count = committed.files).into_owned();
                let hash = committed.head.clone().unwrap_or_default();
                let status = [title.clone(), committed.subject.clone(), hash.clone()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(&separator);
                div()
                    .id(SharedString::from(format!("review-committed-{}", root.display())))
                    .role(Role::Status)
                    .aria_label(status)
                    .flex_none()
                    .mx(px(GAP))
                    .mt(px(GAP))
                    .px(px(12.))
                    .py(px(10.))
                    .rounded(px(CARD_RADIUS))
                    .border_1()
                    .border_color(success.opacity(0.28))
                    .bg(success.opacity(0.07))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(svg().flex_none().path(CHECK_ICON).size(px(14.)).text_color(success))
                    .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).child(title))
                    .child(div().min_w_0().truncate().text_color(muted).child(committed.subject.clone()))
                    .child(
                        div()
                            .ml_auto()
                            .flex_none()
                            .text_size(FontSize::Small)
                            .text_color(subtle)
                            .when_some(mono.clone(), |hash, family| hash.font_family(family))
                            .child(hash),
                    )
            })
            .collect()
    }

    fn render_item(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(item) = self.items.get(ix).cloned() else { return div().into_any_element() };
        let last = match &item {
            Item::File { open, .. } => !open,
            Item::Hunk { last, .. } => *last,
        };
        let content = match item {
            Item::File { file, open } => self.render_card_header(&file, open, cx),
            Item::Hunk { file, hunk, last } => self.render_hunk(&file, hunk, last, window, cx),
        };
        div()
            .w_full()
            .px(px(GAP))
            .when(ix == 0, |item| item.pt(px(GAP)))
            .when(last, |item| item.pb(px(GAP)))
            .child(content)
            .into_any_element()
    }

    /// 卡头：复选框、路径、不能按块操作的原因、增删的行数，右边打开文件、放弃整个文件和收起；不能按块操作的文件
    /// 最右边是在预览栏里看。`open` 时下面跟着块，底边由最后一块画。
    fn render_card_header(&self, file: &FileRef, open: bool, cx: &mut Context<Self>) -> AnyElement {
        let Some(repo) = self.repo_of(file) else { return div().into_any_element() };
        let (staged, unstaged) = file.sides(repo);
        let Some(shown) = unstaged.or(staged) else { return div().into_any_element() };
        let theme = cx.theme().clone();
        let root = repo.root.clone();
        let busy = self.busy(&root);
        let path_text = file.path.display().to_string();
        let name = base_name(&file.path);
        let dir = file.path.parent().map(|dir| dir.display().to_string()).filter(|dir| !dir.is_empty());
        let actionable = by_hunks(staged, unstaged);
        let mono = font::mono(cx);
        let action = |op, section| GitFileAction { repo: root.clone(), op, section, path: file.path.clone() };

        let state = if staged.is_some() { file_checked(repo, Section::Staged, &file.path) } else { Toggled::False };
        let conflicted = shown.status == FileStatus::Conflicted;
        let check_key = if conflicted { "git.mark_resolved" } else { "git.stage" };
        let check_action = if state == Toggled::True {
            action(FileOp::Unstage, Section::Staged)
        } else {
            action(FileOp::Stage, Section::Unstaged)
        };
        let check = Checkbox::new("check", action_for(&rust_i18n::t!(check_key), &path_text), state)
            .mixed(rust_i18n::t!("git.partly_checked"))
            .aria_disabled(busy)
            .when(busy, |check| check.opacity(0.5))
            .when(!busy, |check| {
                check
                    .cursor_pointer()
                    .on_press_down(cx, move |_, _, cx| cx.emit(ReviewEvent::File(check_action.clone())))
            });

        let diff_target = DiffTarget {
            root: root.clone(),
            rel: file.path.clone(),
            old_rel: shown.old_path.clone(),
            side: if unstaged.is_some() { DiffSide::Worktree } else { DiffSide::Index },
        };
        let path = div()
            .id("path")
            .flex_initial()
            .min_w_0()
            .truncate()
            .when_some(mono.clone(), |path, family| path.font_family(family))
            .children(dir.map(|dir| div().flex_none().text_color(theme.muted_foreground).child(format!("{dir}/"))))
            .child(name.clone())
            .flex()
            .when(!actionable, |path| {
                let target = diff_target.clone();
                path.cursor_pointer()
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(ReviewEvent::OpenDiff(target.clone()))))
            });
        let note = (!actionable).then(|| file_note(staged, unstaged)).flatten().map(|key| {
            div()
                .flex_none()
                .text_size(FontSize::Small)
                .text_color(status_tint(shown.status, &theme))
                .child(rust_i18n::t!(key).into_owned())
        });
        let (added, removed) = repo.head_stat(&file.path).unwrap_or_else(|| {
            [staged, unstaged]
                .into_iter()
                .flatten()
                .fold((0, 0), |sum, file| (sum.0 + file.added, sum.1 + file.removed))
        });
        let stats = div()
            .flex_none()
            .flex()
            .gap(px(7.))
            .text_size(FontSize::Small)
            .when_some(mono, |stats, family| stats.font_family(family))
            .when(added > 0, |stats| stats.child(added_label(added)))
            .when(removed > 0, |stats| stats.child(removed_label(removed)));

        let icon_button = |id: &'static str, icon: &'static str, label: String| {
            Button::new(id, label.clone()).icon(icon).with_size(ButtonSize::Icon).tooltip(tooltip(label, None))
        };
        let open_path = root.join(&file.path);
        let open_file = icon_button("open", OPEN_FILE_ICON, action_for(&rust_i18n::t!("git.open_file"), &path_text))
            .on_press(cx, move |_, _, cx| cx.emit(ReviewEvent::OpenFile(open_path.clone())));
        // 子模块那一条丢不掉，暂存区里的要先撤回，都不给放弃的按钮。
        let discard = unstaged.filter(|file| !file.gitlink).map(|_| {
            let discard = action(FileOp::Discard, Section::Unstaged);
            icon_button("discard", DISCARD_ICON, action_for(&rust_i18n::t!("git.discard"), &path_text))
                .disabled(busy)
                .when(!busy, |button| button.on_press(cx, move |_, _, cx| cx.emit(ReviewEvent::File(discard.clone()))))
        });
        let toggle = if actionable {
            let key = if open { "review.collapse" } else { "review.expand" };
            let toggled = file.clone();
            icon_button(
                "toggle",
                if open { CHEVRON_DOWN_ICON } else { CHEVRON_RIGHT_ICON },
                action_for(&rust_i18n::t!(key), &name),
            )
            .aria_expanded(open)
            .on_press(cx, move |this, _, cx| this.toggle_collapsed(&toggled, cx))
        } else {
            icon_button("toggle", CHEVRON_RIGHT_ICON, action_for(&rust_i18n::t!("review.open_diff"), &name))
                .on_press(cx, move |_, _, cx| cx.emit(ReviewEvent::OpenDiff(diff_target.clone())))
        };

        let id = format!("review-file-{}-{}", root.display(), path_text);
        div()
            .id(SharedString::from(id))
            .role(Role::Group)
            .aria_label(path_text)
            .h(scaled(CARD_HEADER_HEIGHT))
            .pl(px(11.))
            .pr(px(6.))
            .flex()
            .items_center()
            .gap(px(10.))
            .bg(theme.mix(0.06))
            .border_color(theme.border)
            .map(|header| {
                if open {
                    header.border_t_1().border_x_1().rounded_t(px(CARD_RADIUS))
                } else {
                    header.border_1().rounded(px(CARD_RADIUS))
                }
            })
            .child(check)
            .child(path)
            .children(note)
            .child(stats)
            .child(div().ml_auto().flex_none().flex().gap(px(2.)).child(open_file).children(discard).child(toggle))
            .into_any_element()
    }

    /// 卡片里的一块：块头是复选框、所在的函数（`hunk_title`）、放弃这一块和交给 agent，下面是块里的各行。
    fn render_hunk(
        &self,
        file: &FileRef,
        hunk: ReviewHunk,
        last: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(repo) = self.repo_of(file) else { return div().into_any_element() };
        let (staged, unstaged) = file.sides(repo);
        let Some(found) = (if hunk.staged { staged } else { unstaged }).and_then(|diff| diff.hunks.get(hunk.index))
        else {
            return div().into_any_element();
        };
        let theme = cx.theme().clone();
        let root = repo.root.clone();
        let busy = self.busy(&root);
        let (title, name) = (hunk_title(&found.header), hunk_name(&found.header));
        let mono = font::mono(cx);

        let (state, toggle) =
            if hunk.staged { (Toggled::True, HunkAction::Unstage) } else { (Toggled::False, HunkAction::Stage) };
        let checked = file.clone();
        let check = Checkbox::new("check", action_for(&rust_i18n::t!("git.stage_hunk"), &name), state)
            .aria_disabled(busy)
            .when(busy, |check| check.opacity(0.5))
            .when(!busy, |check| {
                check
                    .cursor_pointer()
                    .on_press_down(cx, move |this, window, cx| this.hunk_action(&checked, hunk, toggle, window, cx))
            });
        let discard = if hunk.staged {
            Button::new("discard", rust_i18n::t!("review.unstage_first").into_owned())
                .variant(ButtonVariant::Ghost)
                .with_size(ButtonSize::Sm)
                .disabled(true)
        } else {
            let discarded = file.clone();
            Button::new("discard", action_for(&rust_i18n::t!("git.discard_hunk"), &name))
                .variant(ButtonVariant::Ghost)
                .with_size(ButtonSize::Sm)
                .child(rust_i18n::t!("git.discard_hunk").into_owned())
                .disabled(busy)
                .when(!busy, |button| {
                    button.on_press(cx, move |this, window, cx| {
                        this.hunk_action(&discarded, hunk, HunkAction::Discard, window, cx)
                    })
                })
        };
        let quoted = file.clone();
        let quote = Button::new("quote", action_for(&rust_i18n::t!("git.quote_hunk"), &name))
            .variant(ButtonVariant::Ghost)
            .with_size(ButtonSize::Sm)
            .icon(SPARKLE_ICON)
            .icon_size(11.)
            .child(rust_i18n::t!("git.quote_hunk").into_owned())
            .on_press(cx, move |this, _, cx| this.quote(&quoted, hunk, cx));
        let header = div()
            .h(scaled(HUNK_HEADER_HEIGHT))
            .pl(px(12.))
            .pr(px(6.))
            .flex()
            .items_center()
            .gap(px(10.))
            .bg(theme.mix(0.03))
            .border_t_1()
            .border_color(theme.border)
            .text_size(FontSize::Small)
            .text_color(theme.muted_foreground)
            .child(check)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when_some(mono.clone(), |title_el, family| title_el.font_family(family))
                    .child(title.clone()),
            )
            .child(discard)
            .child(quote);

        let a11y = window.is_a11y_active();
        let bg = theme.bg_rgb();
        let dim = theme.foreground.opacity(0.4);
        let number = |n: Option<u32>| {
            div()
                .flex_none()
                .w(scaled(NUMBER_WIDTH))
                .pr(px(8.))
                .flex()
                .justify_end()
                .text_color(dim)
                .children(n.map(|n| n.to_string()))
        };
        let lines = found.lines.iter().enumerate().map(|(ix, line)| {
            let (sign, tint) = match line.kind {
                LineKind::Added => ("+", Some(bg.mix(ADDED, 0.16))),
                LineKind::Removed => ("-", Some(bg.mix(REMOVED, 0.16))),
                LineKind::Context => (" ", None),
            };
            let shown = runode_preview::display_line(&line.text, &[], MAX_COLUMNS);
            let mut text = shown.text;
            if shown.cut {
                text.push('…');
            }
            let row = div()
                .id(("line", ix))
                .min_h(scaled(LINE_HEIGHT))
                .line_height(scaled(LINE_HEIGHT))
                .flex()
                .items_start()
                .when_some(tint, |row, tint| row.bg(hsla(tint)))
                .child(number(line.old))
                .child(number(line.new))
                .child(div().flex_none().w(px(18.)).flex().justify_center().text_color(dim).child(sign));
            // 每行报成一段文字，前面带着正负号；只在辅助工具开着时拼。
            let row = if a11y {
                let label = if sign.trim().is_empty() { text.clone() } else { format!("{sign} {text}") };
                row.aria_static_text(label)
            } else {
                row
            };
            // 一行放不下的代码折到下一行，行号留在第一行。
            row.child(div().flex_1().min_w_0().child(text))
        });

        let side = if hunk.staged { "staged" } else { "worktree" };
        let id = format!("review-hunk-{}-{}-{side}-{}", root.display(), file.path.display(), found.header);
        div()
            .id(SharedString::from(id))
            .role(Role::Group)
            .aria_label(title)
            .border_x_1()
            .border_color(theme.border)
            .overflow_hidden()
            .when(last, |card| card.border_b_1().rounded_b(px(CARD_RADIUS)).pb(px(4.)))
            .child(header)
            .child(div().py(px(4.)).when_some(mono, |code, family| code.font_family(family)).children(lines))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runode_git::{Hunk, Line};

    fn line(kind: LineKind, old: Option<u32>, new: Option<u32>) -> Line {
        Line { kind, old, new, text: String::new() }
    }

    /// 从新文件第 `start` 行起的一块：一行上下文、一行加的。
    fn hunk(start: u32) -> Hunk {
        Hunk {
            header: format!("@@ -{start},1 +{start},2 @@"),
            lines: vec![
                line(LineKind::Context, Some(start), Some(start)),
                line(LineKind::Added, None, Some(start + 1)),
            ],
        }
    }

    fn file(path: &str, status: FileStatus, hunks: Vec<Hunk>) -> FileDiff {
        FileDiff {
            path: path.into(),
            old_path: None,
            status,
            added: hunks.len(),
            removed: 0,
            hunks,
            binary: false,
            truncated: false,
            gitlink: false,
        }
    }

    /// 两段的块按新文件里的起始行交错排，同一行起的暂存区的在前；只有一段时照那段的先后。
    #[test]
    fn hunks_of_both_sections_are_merged_by_line() {
        let staged = file("a.rs", FileStatus::Modified, vec![hunk(10), hunk(40)]);
        let unstaged = file("a.rs", FileStatus::Modified, vec![hunk(1), hunk(10), hunk(90)]);
        let order: Vec<(bool, usize)> =
            review_hunks(Some(&staged), Some(&unstaged)).iter().map(|hunk| (hunk.staged, hunk.index)).collect();
        assert_eq!(order, [(false, 0), (true, 0), (false, 1), (true, 1), (false, 2)]);
        let only: Vec<usize> = review_hunks(None, Some(&unstaged)).iter().map(|hunk| hunk.index).collect();
        assert_eq!(only, [0, 1, 2]);
        assert!(review_hunks(None, None).is_empty());
    }

    /// 只删了行、没有上下文的块按旧文件的行号排。
    #[test]
    fn removed_only_hunks_sort_by_their_old_line() {
        let removed = Hunk { header: "@@ -20 +19,0 @@".into(), lines: vec![line(LineKind::Removed, Some(20), None)] };
        let unstaged = file("a.rs", FileStatus::Modified, vec![removed]);
        let staged = file("a.rs", FileStatus::Modified, vec![hunk(30)]);
        let order: Vec<bool> = review_hunks(Some(&staged), Some(&unstaged)).iter().map(|hunk| hunk.staged).collect();
        assert_eq!(order, [false, true]);
    }

    /// 两段里有一段不能按块操作，整个文件就只有卡头，卡头上写原因。
    #[test]
    fn files_that_cannot_be_split_say_why() {
        let modified = file("a.rs", FileStatus::Modified, vec![hunk(1)]);
        let added = file("a.rs", FileStatus::Added, vec![hunk(1)]);
        assert!(by_hunks(Some(&modified), Some(&modified)));
        assert!(!by_hunks(Some(&added), Some(&modified)));
        assert!(!by_hunks(None, None));
        assert_eq!(file_note(Some(&added), Some(&modified)), Some("review.new_file"));
        let truncated = FileDiff { truncated: true, ..modified.clone() };
        assert!(!by_hunks(None, Some(&truncated)));
        assert_eq!(file_note(None, Some(&truncated)), Some("review.too_large"));
        assert_eq!(file_note(Some(&modified), None), None);
    }

    /// 部分暂存的文件两段里都有，只排一张卡片，按路径排。
    #[test]
    fn partly_staged_files_get_one_card() {
        let mut repo = git::Snapshot {
            root: "/r".into(),
            git_dir: "/r/.git".into(),
            prefix: PathBuf::new(),
            kind: git::RepoKind::Main,
            staged: vec![file("b.rs", FileStatus::Modified, vec![hunk(1)])],
            unstaged: vec![file("c.rs", FileStatus::Modified, vec![hunk(1)])],
            statuses: Default::default(),
            ignored: Default::default(),
            head_stats: Default::default(),
            info: Default::default(),
        };
        repo.unstaged.insert(0, file("b.rs", FileStatus::Modified, vec![hunk(5)]));
        let files = repo_files(0, &repo);
        let shown: Vec<(&Path, Option<usize>, Option<usize>)> =
            files.iter().map(|file| (file.path.as_path(), file.staged, file.unstaged)).collect();
        assert_eq!(shown, [(Path::new("b.rs"), Some(0), Some(0)), (Path::new("c.rs"), None, Some(1))]);
    }
}
