//! 右侧面板的文件树：当前终端所在仓库或目录下的文件，图标按文件类型，名字按 git 状态着色，行尾标出状态。
//! 单击目录展开收起，双击文件在预览栏里开成固定标签，也可以配置成单击就开成临时标签。
//!
//! 点过文件树后它拿到焦点，方向键移动选中的行，各种快捷键作用在选中的那一项上。右键菜单在
//! `menu`；就地新建、改名，删除、剪切复制粘贴和拖动挪位置在 `edit`，它们落到文件系统上的
//! 操作在 `ops`。上面的搜索框在 `search`。
//!
//! 文件树是一个单独的视图（`FileTree`），一个窗口一个，各 workspace 的树（展开的目录、选中的行、搜索词）
//! 各记一份（`state` 的 `TreeState`），显示的是 app 经 `switch_workspace` 说的那个 workspace 的。目录监听和
//! git 状态由 app 管：app 在后台读 git 状态时顺带用 `list_dirs` 读好要显示的目录（`listing`），经
//! `apply_scan` 交进来；要读哪些目录、哪个目录显示着由 `expanded_dirs`、`is_listed` 告诉 app。workspace 的
//! 名字、目录和单击打不打开文件由 app 每次画之前经 `set_input` 交进来；要 app 办的事（开预览、把路径打进
//! 终端、开新标签、预览跟着改名和删除、重读、焦点交回终端、开关变了要存档）经 `FileTreeEvent` 交出去。
//! 右键菜单存在页自己身上。显示哪些文件、搜索结果的排法和按内容搜的选项整个窗口一份，app 随窗口存档。

mod edit;
mod listing;
mod menu;
mod ops;
mod search;
mod state;

use std::{
    borrow::Cow,
    collections::HashMap,
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::ui::{
    a11y::A11yPress,
    actions::{Copy, Cut, Paste},
    button::{Button, ButtonSize},
    file_icons::{file_icon, folder_icon},
    font_size::{self, FontSize},
    hsla,
    icons::{
        CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON, CODE_ICON, FILTER_ICON, FOLDER_OPEN_ICON, MORE_ICON, REFRESH_ICON,
        VIEW_LIST_ICON, VIEW_TREE_ICON,
    },
    menu::{Menu, MenuHost, MenuItem},
    panel::panel_title,
    scrollbar::scrollbar,
    tabs::drag_chip,
    theme::{ADDED, ActiveTheme as _, MODIFIED, REMOVED, RENAMED},
    tooltip::tooltip,
};
use gpui::{
    AccessibleAction, Action, AnyElement, App, Axis, ClickEvent, Context, Div, ElementId, EventEmitter, FocusHandle,
    Focusable, Hsla, MouseButton, MouseDownEvent, Pixels, Render, Role, ScrollStrategy, SharedString, Stateful, Window,
    actions, div, img, prelude::*, px, svg, uniform_list,
};
use runode_git::{self as git, FileStatus};
use runode_types::color::Rgb;

pub use listing::{Listings, list_dirs};
pub use search::SearchOptions;

use edit::{FileClipboard, FileEdit};
use listing::Decoration;
use search::FileSearch;
use state::{TreeFilter, TreeState};

actions!(
    runode,
    [
        /// 文件树里选中上一项、下一项、第一项和最后一项。
        SelectPreviousFile,
        SelectNextFile,
        SelectFirstFile,
        SelectLastFile,
        /// 收起选中的目录，已经收着或者选中的是文件时跳到上一级目录。
        CollapseSelectedFile,
        /// 展开选中的目录，已经展开时跳到它的第一项。
        ExpandSelectedFile,
        /// 选中的目录展开或收起，文件在预览栏里打开。
        OpenSelectedFile,
        /// 把选中项的路径打进终端。
        InsertFilePath,
        /// 在选中的目录或文件所在的目录里新建文件、文件夹。
        NewFile,
        NewFolder,
        RenameFile,
        /// 把选中的文件或目录移到废纸篓。
        DeleteFile,
        /// 把选中的文件或目录写进它所在仓库根目录的 `.gitignore`。
        AddToGitignore,
        /// 在访达里显示选中的文件或目录。
        RevealInFinder,
        /// 开一个新标签页，终端的目录是选中的目录或文件所在的目录。
        OpenInTerminal,
        /// 把选中项的绝对路径、相对文件树根目录的路径复制到剪贴板。
        CopyPath,
        CopyRelativePath,
        /// 收起文件树里所有展开的目录。
        CollapseAllFiles,
        /// 文件树里显示、隐藏名字以 `.` 开头的文件和被 git 忽略的文件。
        ToggleShowDotfiles,
        ToggleShowIgnored,
        /// 用 VS Code 打开文件树的根目录，在访达里打开它。
        OpenRootInVsCode,
        OpenRootInFinder,
        /// 焦点从文件树交回终端。
        FocusTerminal
    ]
);

/// 行高比字号多出的部分。
const ROW_EXTRA_HEIGHT: f32 = 10.;
/// 每深一层往右缩进的宽度。
const INDENT: f32 = 12.;
/// 行的左边距。
const ROW_PADDING: f32 = 4.;
/// 标题那一行上按钮的边长。
const TOOLBAR_BUTTON_SIZE: f32 = 24.;
/// 剪切下来等着粘贴的行画得淡一些。
const CUT_OPACITY: f32 = 0.5;

/// workspace 的标识，由 app 给，挪动位置后不变。
pub type WorkspaceId = u64;

/// app 每次画文件树、对它做什么之前交进来的东西，见 `FileTree::set_input`。
#[derive(Clone, Debug, Default)]
pub struct Input {
    /// 当前 workspace 的名字：树根就是 workspace 的目录时标题写它。
    pub name: SharedString,
    /// 当前 workspace 的目录：还没读到树根时当树根用。
    pub dir: PathBuf,
    /// 单击文件就在预览栏里开成临时标签（配置的 `PreviewClick::Single`）；否则双击才打开。
    pub single_click: bool,
}

/// app 在后台读到的一份结果，经 `FileTree::apply_scan` 交进来。
pub struct TreeScan {
    /// 文件树的根目录：终端所在的仓库的根，不在仓库里时是终端的目录。
    pub root: PathBuf,
    /// 这个仓库以及它里面的子模块和嵌套仓库的 git 状态；不在仓库里时为空。变了才换一份新的，没变时交回上次那份。
    pub git: Option<Arc<git::Repos>>,
    /// 根目录和展开的目录的内容，见 `list_dirs`。
    pub listings: Listings,
    /// 终端换了目录时是新的目录，文件树展开到它、选中它。
    pub reveal: Option<PathBuf>,
    /// 发起这次读时取的 `read_ticket`。
    pub started: u64,
}

/// 发起一次后台读目录（项目扫描或 `list_queued`）时取的序号，越晚发起越大。读回来的目录按它比先后：
/// 发起更早的读到的是更旧的内容，不管谁先读回来，都不盖掉发起更晚的已经换上的。
pub fn read_ticket() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// 整个窗口一份的开关，app 随窗口存档，见 `FileTree::options`。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// 显示被 git 忽略的文件。
    pub show_ignored: bool,
    /// 显示名字以 `.` 开头的文件。
    pub show_dotfiles: bool,
    /// 搜索结果以树形式查看，否则是列表。
    pub search_tree: bool,
    /// 按内容搜时的开关和要包含、要排除的文件。
    pub search: SearchOptions,
}

/// 要 app 办的事。
pub enum FileTreeEvent {
    /// 在预览栏里打开 `path`：`pin` 时开成固定标签，否则开成临时标签；`line` 不为空时选中这一行（从 0 数）、
    /// 滚到中间。
    Open { path: PathBuf, line: Option<usize>, pin: bool },
    /// 把这个路径打进当前终端，焦点交回终端。
    InsertPath(PathBuf),
    /// 在当前 workspace 里开一个新标签页，终端的目录是这个目录。
    OpenTerminal(PathBuf),
    /// `from` 改了名或挪到了 `to`：各 workspace 里它和它下面的文件的预览标签跟过去。剪贴板整个窗口一份，
    /// 在哪个 workspace 里挪的都一样。
    Moved { from: PathBuf, to: PathBuf },
    /// `path` 移到了废纸篓：关掉各 workspace 里它和它下面的文件的预览标签。
    Removed(PathBuf),
    /// 重读当前 workspace 的 git 状态和目录：按了刷新，或者改了 `.gitignore`。
    Refresh,
    /// 焦点交回终端。
    FocusTerminal,
    /// 显示哪些文件、搜索结果的排法或者按内容搜的选项变了，app 存档；下次经 `set_options` 交回。
    OptionsChanged,
}

/// 文件名按 git 状态上的颜色。
fn status_color(status: FileStatus) -> Rgb {
    match status {
        FileStatus::Modified => MODIFIED,
        FileStatus::Added | FileStatus::Untracked => ADDED,
        FileStatus::Deleted | FileStatus::Conflicted => REMOVED,
        FileStatus::Renamed => RENAMED,
    }
}

/// 用 VS Code 打开 `dir`，按 bundle id 找它，不管装在哪里。没装时只记一条日志。
fn open_in_vscode(dir: PathBuf) {
    std::thread::spawn(move || {
        let status = std::process::Command::new("open").args(["-b", "com.microsoft.VSCode"]).arg(&dir).status();
        if !status.is_ok_and(|status| status.success()) {
            tracing::warn!("failed to open {} in VS Code", dir.display());
        }
    });
}

/// 从文件树拖出来的一项：放到终端上把路径打进去（由 app 接），放到文件树的目录上挪进那个目录。
#[derive(Clone)]
pub struct DraggedFile {
    pub path: PathBuf,
    name: SharedString,
    width: Pixels,
    height: Pixels,
}

impl Render for DraggedFile {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        drag_chip(self.width, self.height, self.name.clone(), cx.theme().mix(0.10), cx)
    }
}

/// 文件树，见模块说明。
pub struct FileTree {
    /// 各 workspace 的树，没显示过文件树的 workspace 不在表里。
    workspaces: HashMap<WorkspaceId, TreeState>,
    /// 显示的是哪个 workspace 的树，见 `switch_workspace`；还没说过时为空。
    ws: Option<WorkspaceId>,
    /// 当前 workspace 还没有树时读到的空树。
    empty: TreeState,
    input: Input,
    /// 文件树的焦点：点了文件树后方向键在里面移动选中的行。搜索框不在它里面。
    focus: FocusHandle,
    /// 开着的右键菜单和「更多」菜单。
    menu: Option<Menu>,
    /// 上面的搜索框和搜到的结果；整个窗口一个，换了 workspace 时换上那个 workspace 的搜索词重搜。
    search: FileSearch,
    /// 正在新建或改名的输入框，以及剪切或复制下来等着粘贴的文件；可以跨 workspace 粘贴。
    edit: Option<FileEdit>,
    clipboard: Option<FileClipboard>,
    /// 显示被 git 忽略的文件、名字以 `.` 开头的文件。整个窗口一份，app 存档。
    show_ignored: bool,
    show_dotfiles: bool,
}

impl EventEmitter<FileTreeEvent> for FileTree {}

impl Focusable for FileTree {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl MenuHost for FileTree {
    fn menu_slot(&mut self) -> &mut Option<Menu> {
        &mut self.menu
    }

    fn menu(&self) -> Option<&Menu> {
        self.menu.as_ref()
    }
}

impl FileTree {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            workspaces: HashMap::new(),
            ws: None,
            empty: TreeState::default(),
            input: Input::default(),
            focus: cx.focus_handle(),
            menu: None,
            search: FileSearch::new(cx),
            edit: None,
            clipboard: None,
            show_ignored: false,
            show_dotfiles: true,
        }
    }

    /// 换上当前 workspace 的名字、目录和单击打不打开文件，见 `Input`。只记下来，不重画：app 在画的时候调。
    pub fn set_input(&mut self, input: Input) {
        self.input = input;
    }

    /// 之后显示的、操作的都是 workspace `ws` 的树：框里的搜索词存回原来那个 workspace，换上这个 workspace 的
    /// 词重搜。和上次是同一个时什么也不做。会重画，不能在画的时候调。
    pub fn switch_workspace(&mut self, ws: WorkspaceId, cx: &mut Context<Self>) {
        let old = self.ws.replace(ws);
        if old == Some(ws) {
            return;
        }
        // 开着的改名、新建框作用在原来那个 workspace 的树上，切走就当取消。
        self.edit = None;
        // 首次扫描还没回来时旧 workspace 可能还没有树，搜索词也要记下；关掉了的（`forget_workspace`）不记。
        if let Some(old) = old {
            self.workspaces.entry(old).or_default().query = self.search.query(cx);
        }
        let query = self.state().query.clone();
        self.search.set_query(query, cx);
        self.sync_file_search(cx);
    }

    /// workspace `ws` 关掉了，丢掉它的树；是当前的那个时，下次切换不再替它存搜索词。
    pub fn forget_workspace(&mut self, ws: WorkspaceId) {
        self.workspaces.remove(&ws);
        if self.ws == Some(ws) {
            self.ws = None;
        }
    }

    /// workspace `ws` 展开着的目录，app 后台读的时候连同根目录一起读。
    pub fn expanded_dirs(&self, ws: WorkspaceId) -> Vec<PathBuf> {
        self.workspaces.get(&ws).map(|state| state.expanded.iter().cloned().collect()).unwrap_or_default()
    }

    /// workspace `ws` 的树里读过 `dir` 的内容（它显示着或者刚收起来）：app 据此判断里面被忽略的文件变了
    /// 要不要重读。
    pub fn is_listed(&self, ws: WorkspaceId, dir: &Path) -> bool {
        self.workspaces.get(&ws).is_some_and(|state| state.listings.contains_key(dir))
    }

    /// 换上 app 给 workspace `ws` 在后台读到的结果。是当前 workspace 时重画，文件可能变了，有搜索词时重搜；
    /// 后台 workspace 的只记下来。
    pub fn apply_scan(&mut self, ws: WorkspaceId, scan: TreeScan, cx: &mut Context<Self>) {
        let filter = self.filter();
        let changed = self.workspaces.entry(ws).or_default().apply_scan(scan, filter);
        // 终端换了目录时定位过去，要的目录可能还没读。
        self.list_queued(Some(ws), cx);
        if self.ws == Some(ws) {
            if changed {
                cx.notify();
            }
            self.refresh_file_search(changed, cx);
        }
    }

    /// 在文件树里展开到 `path` 所在的目录，选中它、滚到能看见：预览栏里切到了这个文件。
    pub fn reveal_file(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.with_tree(cx, |state, root, filter| state.reveal_file(path, root, filter));
        cx.notify();
    }

    /// 整个窗口一份的开关，app 存档用。
    pub fn options(&self, cx: &App) -> Options {
        Options {
            show_ignored: self.show_ignored,
            show_dotfiles: self.show_dotfiles,
            search_tree: self.search.tree,
            search: self.search.options(cx),
        }
    }

    /// 恢复窗口时换上存档里的开关；这时还没有搜索词，不用重搜。
    pub fn set_options(&mut self, options: Options, cx: &mut App) {
        self.show_ignored = options.show_ignored;
        self.show_dotfiles = options.show_dotfiles;
        self.search.set_tree(options.search_tree);
        self.search.set_options(options.search, cx);
        self.rebuild_all_rows();
    }

    /// 当前 workspace 的树。
    fn state(&self) -> &TreeState {
        self.ws.and_then(|ws| self.workspaces.get(&ws)).unwrap_or(&self.empty)
    }

    fn state_mut(&mut self) -> &mut TreeState {
        self.workspaces.entry(self.ws.unwrap_or_default()).or_default()
    }

    /// 文件树的根目录：终端所在的仓库或目录，还没读过时是 workspace 的目录。
    fn files_root(&self) -> PathBuf {
        self.state().root.clone().unwrap_or_else(|| self.input.dir.clone())
    }

    /// 文件树显示哪些文件。
    fn filter(&self) -> TreeFilter {
        TreeFilter { ignored: self.show_ignored, dotfiles: self.show_dotfiles }
    }

    /// 切换文件树里是否显示被 git 忽略的文件、名字以 `.` 开头的文件。
    fn toggle_show_ignored(&mut self, cx: &mut Context<Self>) {
        self.show_ignored = !self.show_ignored;
        self.filter_changed(cx);
    }

    fn toggle_show_dotfiles(&mut self, cx: &mut Context<Self>) {
        self.show_dotfiles = !self.show_dotfiles;
        self.filter_changed(cx);
    }

    /// 文件树显示哪些变了：所有 workspace 的文件树重排，存进窗口存档。
    fn filter_changed(&mut self, cx: &mut Context<Self>) {
        self.rebuild_all_rows();
        cx.emit(FileTreeEvent::OptionsChanged);
        cx.notify();
    }

    fn rebuild_all_rows(&mut self) {
        let filter = self.filter();
        for state in self.workspaces.values_mut() {
            if let Some(root) = state.root.clone() {
                state.refilter(&root, filter);
            }
        }
    }

    /// 标题行「更多」按钮的菜单：新建、全部收起，显示哪些文件，用别的程序打开根目录。
    fn open_files_more_menu(&mut self, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let item = |key: &str, action: Box<dyn Action>| Some(MenuItem::new(rust_i18n::t!(key), action, true, cx));
        let check = |key: &str, checked: bool, action: Box<dyn Action>| {
            Some(MenuItem::checkbox(rust_i18n::t!(key).into_owned(), None, checked, action))
        };
        let items = vec![
            item("files.new_file", Box::new(NewFile)),
            item("files.new_folder", Box::new(NewFolder)),
            item("files.collapse_all", Box::new(CollapseAllFiles)),
            None,
            check("files.show_dotfiles", self.show_dotfiles, Box::new(ToggleShowDotfiles)),
            check("files.show_ignored", self.show_ignored, Box::new(ToggleShowIgnored)),
            None,
            item("files.open_in_vscode", Box::new(OpenRootInVsCode)).map(|item| item.with_icon(CODE_ICON)),
            item(&crate::i18n::os_key("files.open_in_finder"), Box::new(OpenRootInFinder))
                .map(|item| item.with_icon(FOLDER_OPEN_ICON)),
        ];
        let target = self.focus.clone();
        self.open_context_menu(position, items, target, cx);
    }

    /// 选中的那一项和它是不是目录；没选中时为空。搜索时是选中的那一行结果，不是藏起来的树里选中的。
    fn selected_entry(&self, cx: &App) -> Option<(PathBuf, bool)> {
        if self.searching(cx) {
            return self.selected_search_entry();
        }
        let state = self.state();
        let path = state.selected.clone()?;
        let is_dir = match state.selected_row() {
            Some(ix) => state.rows[ix].is_dir,
            None => path.is_dir(),
        };
        Some((path, is_dir))
    }

    /// 新建、粘贴和开终端落在哪个目录：选中的目录，选中文件所在的目录，没选中时是根目录。
    fn target_dir(&self, cx: &App) -> PathBuf {
        match self.selected_entry(cx) {
            Some((path, true)) => path,
            Some((path, false)) => path.parent().map_or_else(|| self.files_root(), Path::to_path_buf),
            None => self.files_root(),
        }
    }

    /// 标题那一行：目录名，右边是搜索结果的排法、刷新和「更多」菜单。
    fn render_header(&self, cx: &mut Context<Self>) -> Div {
        let state = self.state();
        // 显示的是终端所在的仓库或目录，不一定是 workspace 的目录。
        let name = match state.root.as_deref().and_then(Path::file_name) {
            Some(name) if state.root.as_ref() != Some(&self.input.dir) => {
                SharedString::from(name.to_string_lossy().into_owned())
            }
            _ => self.input.name.clone(),
        };
        // 标题行上的图标按钮。按下时不往外传：外层的文件树按下时会把焦点抢回去，新建时刚交给
        // 输入框的焦点就丢了。
        let button = |id, icon, text: Cow<'static, str>| {
            Button::new(id, SharedString::from(text.clone()))
                .icon(icon)
                .with_size(ButtonSize::Icon)
                .size(px(TOOLBAR_BUTTON_SIZE))
                .tooltip(tooltip(text, None))
        };
        // 这几个按钮只有按下的处理，辅助工具按不到，另外登记按下时做的事。
        let view = cx.entity().downgrade();
        // 搜索结果排成树还是列表；没在搜时淡着、点了没反应。
        let searching = self.searching(cx);
        let (icon, text) = if self.search.tree {
            (VIEW_LIST_ICON, rust_i18n::t!("files.view_as_list"))
        } else {
            (VIEW_TREE_ICON, rust_i18n::t!("files.view_as_tree"))
        };
        let view_toggle = button("search-view", icon, text)
            .when(!searching, |button| button.opacity(0.4))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    if this.searching(cx) {
                        this.toggle_search_tree(cx);
                    }
                }),
            )
            .on_a11y_press(view.clone(), |this, _, cx| {
                if this.searching(cx) {
                    this.toggle_search_tree(cx);
                }
            });
        let refresh = button("files-refresh", REFRESH_ICON, rust_i18n::t!("files.refresh"))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| {
                    cx.stop_propagation();
                    cx.emit(FileTreeEvent::Refresh);
                }),
            )
            .on_a11y_press(view.clone(), |_, _, cx| cx.emit(FileTreeEvent::Refresh));
        let more = button("files-more", MORE_ICON, rust_i18n::t!("files.more"))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.open_files_more_menu(event.position, cx);
                }),
            )
            // 辅助工具按下时没有鼠标事件，菜单弹在鼠标所在的地方，贴着窗口边挪进来。
            .on_a11y_press(view, |this, window, cx| this.open_files_more_menu(window.mouse_position(), cx));
        panel_title()
            .gap(px(6.))
            .child(div().flex_initial().min_w_0().truncate().text_color(cx.theme().foreground).child(name))
            .child(div().flex_1())
            .child(div().flex().gap(px(4.)).child(view_toggle).child(refresh).child(more))
    }

    /// 文件树的行列表和滚动条。
    fn render_file_tree(&self, font_size: f32, cx: &mut Context<Self>) -> Stateful<Div> {
        let state = self.state();
        let new_entry = self.new_entry_row();
        let count = state.rows.len() + usize::from(new_entry.is_some());
        let list = uniform_list(
            "files",
            count,
            cx.processor(move |this, range: Range<usize>, window, cx| {
                let focused = this.focus.contains_focused(window, cx);
                this.render_file_rows(range, new_entry, font_size, focused, cx)
            }),
        )
        .track_scroll(&state.scroll)
        .size_full()
        .p(px(4.))
        // 点在行下面的空白处：取消选中，右键弹出对根目录的菜单。
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                this.state_mut().selected = None;
                cx.notify();
            }),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(|this, event: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                window.focus(&this.focus, cx);
                this.state_mut().selected = None;
                this.open_file_menu(event.position, cx);
            }),
        );
        let scroll = state.scroll.0.borrow().base_handle.clone();
        // 列表只画看得见的行，辅助工具也只读到这些行。
        div()
            .id("file-tree")
            .role(Role::Tree)
            .aria_label(rust_i18n::t!("files.tree").into_owned())
            .flex_1()
            .min_h_0()
            .relative()
            .child(list)
            .child(scrollbar("files-scroll", scroll, Axis::Vertical))
    }

    /// 文件树的行。行高、箭头和图标跟着字号 `font_size` 一起缩放。新建时输入框插在
    /// `new_entry`（即 `new_entry_row`）那一行，后面的行往下错一行。
    fn render_file_rows(
        &self,
        range: Range<usize>,
        new_entry: Option<(usize, usize)>,
        font_size: f32,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        range
            .filter_map(|ix| match new_entry {
                Some((at, depth)) if ix == at => Some(self.render_new_entry_row(depth, font_size, cx)),
                Some((at, _)) if ix > at => self.render_file_row(ix - 1, font_size, focused, cx),
                _ => self.render_file_row(ix, font_size, focused, cx),
            })
            .collect()
    }

    /// 缩进：每一层一格，格子中间一条竖线，对准那一层的箭头，上下相邻的行连成一条。
    fn indent_guides(depth: usize, guide: Hsla) -> impl Iterator<Item = Div> {
        (0..depth).map(move |_| {
            div().flex_none().w(px(INDENT)).h_full().flex().justify_center().child(div().w(px(1.)).h_full().bg(guide))
        })
    }

    /// 文件树的一行：外框、缩进和底色，里面放箭头、图标和名字。
    fn file_row_shell(id: impl Into<gpui::ElementId>, depth: usize, font_size: f32, cx: &App) -> Stateful<Div> {
        div()
            .id(id)
            .flex_none()
            .h(px(font_size + ROW_EXTRA_HEIGHT))
            // 占满整行宽度，名字后面的空白处也能点、也有悬停底色。
            .w_full()
            .pl(px(ROW_PADDING))
            .pr(px(6.))
            .rounded(px(4.))
            .flex()
            .items_center()
            .overflow_hidden()
            .children(Self::indent_guides(depth, cx.theme().foreground.opacity(0.2)))
    }

    /// 第 `ix` 个 `FileRow`；`focused` 是文件树有没有焦点，有焦点时选中的行更醒目。
    fn render_file_row(&self, ix: usize, font_size: f32, focused: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let row = self.state().rows.get(ix)?;
        let theme = cx.theme();
        let selected_bg = theme.mix(if focused { 0.16 } else { 0.10 });
        let hover_bg = theme.mix(0.06);
        let drop_bg = theme.mix(0.20);
        let fg = theme.foreground;
        let color = match row.decoration {
            Decoration::None | Decoration::Ignored => fg.opacity(0.85),
            Decoration::Status(status) | Decoration::ContainsChanges(status) => hsla(status_color(status)),
        };
        let selected = self.state().selected.as_ref() == Some(&row.path);
        let cut = self.clipboard.as_ref().is_some_and(|clip| clip.cut && clip.path == row.path);
        let chevron = row.is_dir.then(|| {
            svg()
                .path(if row.expanded { CHEVRON_DOWN_ICON } else { CHEVRON_RIGHT_ICON })
                .size(px(font_size))
                .text_color(fg.opacity(0.5))
        });
        // 并成一行的目录按最里层的名字挑图标。
        let last = row.path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
        let icon = if row.is_dir { folder_icon(&last, row.expanded) } else { file_icon(&last) };
        // 行尾的 git 标记：文件写状态字母，含改动的目录画一个点。
        let badge = match row.decoration {
            Decoration::Status(status) => Some(status.letter()),
            Decoration::ContainsChanges(_) => Some("•"),
            Decoration::None | Decoration::Ignored => None,
        };
        let status = badge;
        let badge = badge.map(|text| div().flex_none().pl(px(6.)).text_color(color).child(text));
        let renaming = self.renaming(&row.path);
        let name: AnyElement = match renaming {
            Some(edit) => edit.render(px(font_size + 6.)).flex_1().min_w_0().into_any_element(),
            None => div().min_w_0().truncate().text_color(color).child(row.name.clone()).into_any_element(),
        };
        // 改名时在输入框里拖选文字，不能把整行拖走。
        let dragged = renaming.is_none().then(|| DraggedFile {
            path: row.path.clone(),
            name: row.name.clone(),
            width: px(180.),
            height: px(font_size + ROW_EXTRA_HEIGHT),
        });
        let path = row.path.clone();
        let is_dir = row.is_dir;
        // 放到目录上挪进这个目录，放到文件上挪进文件所在的目录。
        let drop_dir =
            if is_dir { path.clone() } else { path.parent().map_or_else(|| self.files_root(), Path::to_path_buf) };
        let expanded = row.expanded;
        // 辅助工具按下时打开文件、展开或收起目录；目录还能直接展开、收起。
        let a11y = |want: Option<bool>, cx: &mut Context<Self>| {
            let (view, path) = (cx.entity().downgrade(), path.clone());
            move |_: Option<&gpui::accesskit::ActionData>, window: &mut Window, cx: &mut gpui::App| {
                view.update(cx, |this, cx| match want {
                    // 正在改名的那一行不动。
                    _ if this.renaming(&path).is_some() => {}
                    Some(want) if want == expanded => {}
                    Some(_) => {
                        this.state_mut().selected = Some(path.clone());
                        this.with_tree(cx, |state, root, filter| state.toggle_dir(&path, root, filter));
                        cx.notify();
                    }
                    // 焦点交给文件树。按两下的打开法：预览设成双击才打开时单击什么也不做，而辅助
                    // 工具按下总该打开文件；目录不看按了几下，照样展开、收起。
                    None => {
                        window.focus(&this.focus, cx);
                        this.click_file(&path, is_dir, 2, cx);
                    }
                })
                .ok();
            }
        };
        // id 按路径取，不按行号：展开目录、新建时上面多出几行，辅助工具的节点和焦点仍跟着同一个文件。
        let id = ElementId::Path(Arc::from(row.path.as_path()));
        Some(
            Self::file_row_shell(id, row.depth, font_size, cx)
                .role(Role::TreeItem)
                .aria_label(row.name.clone())
                .aria_level(row.depth + 1)
                .aria_selected(selected)
                .when_some(status, |item, status| item.aria_description(status))
                .when(is_dir, |item| {
                    item.aria_expanded(expanded)
                        .on_a11y_action(AccessibleAction::Expand, a11y(Some(true), cx))
                        .on_a11y_action(AccessibleAction::Collapse, a11y(Some(false), cx))
                })
                .on_a11y_action(AccessibleAction::Click, a11y(None, cx))
                .when(row.decoration == Decoration::Ignored, |item| item.opacity(0.45))
                .when(cut, |item| item.opacity(CUT_OPACITY))
                .map(|item| if selected { item.bg(selected_bg) } else { item.hover(|item| item.bg(hover_bg)) })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .child(div().flex_none().w(px(font_size)).flex().items_center().children(chevron))
                        .child(img(icon).flex_none().size(px(font_size + 2.)))
                        .child(name),
                )
                .children(badge)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener({
                        let path = path.clone();
                        move |this, _: &MouseDownEvent, window, cx| {
                            // 点在改名输入框里时交给输入框挪光标、选字。
                            cx.stop_propagation();
                            if this.renaming(&path).is_some() {
                                return;
                            }
                            window.focus(&this.focus, cx);
                            this.state_mut().selected = Some(path.clone());
                            cx.notify();
                        }
                    }),
                )
                // 按下只选中，松开才展开目录、打开预览：拖动时 GPUI 不发点击，拖走的文件不会被打开。
                .on_click(cx.listener({
                    let path = path.clone();
                    move |this, event: &ClickEvent, _, cx| {
                        // 双击时 GPUI 按一下、两下各发一次点击，目录只认第一下，不然刚展开又收起。
                        if is_dir && event.click_count() >= 2 {
                            return;
                        }
                        if this.renaming(&path).is_none() {
                            this.click_file(&path, is_dir, event.click_count(), cx);
                        }
                    }
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        window.focus(&this.focus, cx);
                        this.state_mut().selected = Some(path.clone());
                        this.open_file_menu(event.position, cx);
                    }),
                )
                .when_some(dragged, |row, dragged| {
                    row.on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
                })
                .drag_over::<DraggedFile>(move |style, _, _, _| style.bg(drop_bg))
                .on_drop(cx.listener(move |this, dragged: &DraggedFile, window, cx| {
                    this.drop_file(&dragged.path, drop_dir.clone(), window, cx);
                }))
                .into_any_element(),
        )
    }

    /// 点击（按下又松开、没拖动）选中，目录同时展开或收起。文件按 app 交来的单击打不打开（`Input::single_click`）
    /// 在预览栏里打开：单击打开时单击开成临时标签，双击固定下来；双击打开时双击开成固定标签。
    fn click_file(&mut self, path: &Path, is_dir: bool, clicks: usize, cx: &mut Context<Self>) {
        self.state_mut().selected = Some(path.to_path_buf());
        if is_dir {
            self.with_tree(cx, |state, root, filter| state.toggle_dir(path, root, filter));
            cx.notify();
            return;
        }
        if clicks >= 2 || self.input.single_click {
            cx.emit(FileTreeEvent::Open { path: path.to_path_buf(), line: None, pin: clicks >= 2 });
        }
        cx.notify();
    }

    /// 拿文件树的根目录和显示哪些文件，对当前 workspace 的树做 `f`；`f` 要读的目录随后在后台读。
    fn with_tree<R>(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut TreeState, &Path, TreeFilter) -> R) -> R {
        let (root, filter) = (self.files_root(), self.filter());
        let result = f(self.state_mut(), &root, filter);
        self.list_queued(self.ws, cx);
        result
    }

    /// 和 `with_tree` 一样，但对的是 workspace `ws` 的树：后台改文件的操作做完时可能已经切到了别的 workspace。
    /// 那个 workspace 关掉了或者还没读到根目录时不做。
    fn with_tree_of(
        &mut self,
        ws: Option<WorkspaceId>,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut TreeState, &Path, TreeFilter),
    ) {
        if ws == self.ws {
            return self.with_tree(cx, f);
        }
        let filter = self.filter();
        if let Some(state) = ws.and_then(|ws| self.workspaces.get_mut(&ws))
            && let Some(root) = state.root.clone()
        {
            f(state, &root, filter);
            self.list_queued(ws, cx);
        }
    }

    /// 在后台读 workspace `ws` 的树记下要读的目录（`TreeState::to_list`），读完换上那个 workspace 的树：
    /// 展开大目录、慢的卷时不卡住界面。
    fn list_queued(&mut self, ws: Option<WorkspaceId>, cx: &mut Context<Self>) {
        let Some(state) = ws.and_then(|ws| self.workspaces.get_mut(&ws)) else {
            return;
        };
        if state.to_list.is_empty() {
            return;
        }
        let (dirs, git, since) = (std::mem::take(&mut state.to_list), state.git.clone(), read_ticket());
        let job = cx.background_spawn(async move { list_dirs(dirs, git.as_deref()) });
        cx.spawn(async move |this, cx| {
            let listings = job.await;
            this.update(cx, |this, cx| {
                this.with_tree_of(ws, cx, |state, root, filter| state.apply_listings(listings, since, root, filter));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 键盘移动选中的行：`f` 返回新选中的位置，滚到那里，关掉右键菜单。
    fn move_selection(
        &mut self,
        f: impl FnOnce(&mut TreeState, &Path, TreeFilter) -> Option<usize>,
        cx: &mut Context<Self>,
    ) {
        self.with_tree(cx, |state, root, filter| {
            if let Some(ix) = f(state, root, filter) {
                state.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
            }
        });
        self.menu = None;
        cx.notify();
    }

    fn select_previous_file(&mut self, _: &SelectPreviousFile, _: &mut Window, cx: &mut Context<Self>) {
        if self.searching(cx) {
            return self.step_search_selection(-1, cx);
        }
        self.move_selection(|state, _, _| state.select_step(-1), cx);
    }

    fn select_next_file(&mut self, _: &SelectNextFile, _: &mut Window, cx: &mut Context<Self>) {
        if self.searching(cx) {
            return self.step_search_selection(1, cx);
        }
        self.move_selection(|state, _, _| state.select_step(1), cx);
    }

    fn select_first_file(&mut self, _: &SelectFirstFile, _: &mut Window, cx: &mut Context<Self>) {
        if self.searching(cx) {
            return self.move_search_selection(|_, _| Some(0), cx);
        }
        self.move_selection(|state, _, _| state.select_row(0), cx);
    }

    fn select_last_file(&mut self, _: &SelectLastFile, _: &mut Window, cx: &mut Context<Self>) {
        if self.searching(cx) {
            return self.move_search_selection(|len, _| len.checked_sub(1), cx);
        }
        self.move_selection(|state, _, _| state.select_row(state.rows.len().checked_sub(1)?), cx);
    }

    fn collapse_selected_file(&mut self, _: &CollapseSelectedFile, _: &mut Window, cx: &mut Context<Self>) {
        // 搜索时树藏着，不去展开、收起看不见的目录。
        if self.searching(cx) {
            return;
        }
        self.move_selection(TreeState::select_out, cx);
    }

    fn expand_selected_file(&mut self, _: &ExpandSelectedFile, _: &mut Window, cx: &mut Context<Self>) {
        // 搜索时树藏着，不去展开、收起看不见的目录。
        if self.searching(cx) {
            return;
        }
        self.move_selection(TreeState::select_in, cx);
    }

    fn open_selected_file(&mut self, _: &OpenSelectedFile, _: &mut Window, cx: &mut Context<Self>) {
        if self.searching(cx) {
            return self.open_selected_search_row(cx);
        }
        let Some((path, is_dir)) = self.selected_entry(cx) else {
            return;
        };
        if is_dir {
            self.move_selection(
                |state, root, filter| {
                    state.toggle_dir(&path, root, filter);
                    None
                },
                cx,
            );
        } else {
            cx.emit(FileTreeEvent::Open { path, line: None, pin: false });
            // 预览栏刚打开时文件树变窄，选中的行可能被挤出视野。
            self.move_selection(|state, _, _| state.selected_row(), cx);
        }
    }

    fn collapse_all_files(&mut self, _: &CollapseAllFiles, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(
            |state, root, filter| {
                state.collapse_all(root, filter);
                None
            },
            cx,
        );
    }

    /// Esc：开着菜单时先关菜单，否则焦点交回终端。
    fn focus_terminal(&mut self, _: &FocusTerminal, _: &mut Window, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
            return;
        }
        cx.emit(FileTreeEvent::FocusTerminal);
    }
}

impl Render for FileTree {
    /// 当前 workspace 的文件树：标题行、搜索框，下面是树，有搜索词时换成搜索结果。外框和顶上的标签由 app 画。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.focus_menu(window, cx);
        let font_size = FontSize::Medium.points() * font_size::scale(cx);
        let header = self.render_header(cx);
        let search_box = self.render_file_search_box(cx);
        let content = match self.render_file_search_results(font_size, cx) {
            Some(results) => results,
            None => self.render_file_tree(font_size, cx).into_any_element(),
        };
        // 搜索框不在文件树的按键上下文里：文件树绑的方向键、删除这些不能抢输入框的键。
        let tree = div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            // 新建或改名时输入框在文件树里面，方向键这些归输入框。
            .key_context(if self.edit.is_some() { "FileTree editing" } else { "FileTree" })
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::select_previous_file))
            .on_action(cx.listener(Self::select_next_file))
            .on_action(cx.listener(Self::select_first_file))
            .on_action(cx.listener(Self::select_last_file))
            .on_action(cx.listener(Self::collapse_selected_file))
            .on_action(cx.listener(Self::expand_selected_file))
            .on_action(cx.listener(Self::open_selected_file))
            .on_action(cx.listener(Self::collapse_all_files))
            .on_action(cx.listener(|this, _: &ToggleShowDotfiles, _, cx| this.toggle_show_dotfiles(cx)))
            .on_action(cx.listener(|this, _: &ToggleShowIgnored, _, cx| this.toggle_show_ignored(cx)))
            .on_action(cx.listener(|this, _: &OpenRootInVsCode, _, _| open_in_vscode(this.files_root())))
            .on_action(cx.listener(|this, _: &OpenRootInFinder, _, cx| cx.open_with_system(&this.files_root())))
            .on_action(cx.listener(Self::focus_terminal))
            .on_action(cx.listener(Self::new_file))
            .on_action(cx.listener(Self::new_folder))
            .on_action(cx.listener(Self::rename_file))
            .on_action(cx.listener(Self::delete_file))
            .on_action(cx.listener(Self::add_to_gitignore))
            .on_action(cx.listener(Self::reveal_in_finder))
            .on_action(cx.listener(Self::insert_file_path))
            .on_action(cx.listener(Self::open_in_terminal))
            .on_action(cx.listener(Self::copy_path))
            .on_action(cx.listener(Self::copy_relative_path))
            .on_action(cx.listener(|this, _: &Copy, _, cx| this.copy_file(false, cx)))
            .on_action(cx.listener(|this, _: &Cut, _, cx| this.copy_file(true, cx)))
            .on_action(cx.listener(|this, _: &Paste, window, cx| this.paste_file(window, cx)))
            // 拖到空白处：挪到根目录。
            .on_drop(cx.listener(|this, dragged: &DraggedFile, window, cx| {
                let root = this.files_root();
                this.drop_file(&dragged.path, root, window, cx);
            }))
            .child(content);
        // 外框由 app 画，这里自己写满宽高。
        div()
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(font_size))
            .child(header)
            .child(search_box)
            .child(tree)
            .children(self.render_context_menu(window, cx))
    }
}
