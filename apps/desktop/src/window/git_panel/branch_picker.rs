//! 分支列表：盖在窗口上面的浮层，列出本地和远端分支，输入文字过滤；输入的是还没有的分支名时
//! 第一行是「新建分支」。上下键选、回车切过去、Esc 关掉。只新建分支时只有新建那一行。列的是
//! 打开时选定的那个仓库（主仓库或子仓库）的分支，切换、新建也在那个仓库里做。

use std::path::{Path, PathBuf};

use gpui::{
    Context, Div, Entity, Focusable, KeyDownEvent, MouseButton, Role, ScrollHandle, SharedString, Subscription, Window,
    div, prelude::*, px, svg,
};
use runode_git::{self as git, Branch};
use runode_shared_types::color::Rgb;

use super::super::{
    WindowView,
    agent_picker::{nav_delta, picker_panel},
};
use crate::ui::a11y::A11yPress;
use crate::{
    assets::{BRANCH_ICON, CHECK_ICON, PLUS_ICON},
    ui::{
        hsla,
        text_field::{TextField, TextFieldEvent},
    },
};

const PICKER_WIDTH: f32 = 520.;
const ROW_HEIGHT: f32 = 28.;
/// 最多显示这么多行，多了滚动。
const VISIBLE_ROWS: f32 = 12.;

/// 开着的分支列表。
pub(in crate::window) struct BranchPicker {
    /// 在哪个仓库里切换、新建分支：它的根目录。
    repo: PathBuf,
    /// 从哪个提交新建分支，为空时从当前提交。只在从图表里新建分支时有。
    start: Option<String>,
    field: Entity<TextField>,
    /// 后台读到的分支；还没读完时为空。
    branches: Option<Vec<Branch>>,
    /// 只新建分支，不列出已有的。
    create_only: bool,
    /// 输入的文字能不能当分支名，文字变了时问一次 git。
    valid_name: bool,
    selected: usize,
    scroll: ScrollHandle,
    _subscriptions: [Subscription; 2],
}

/// 列表里的一行。
enum PickerRow<'a> {
    Create(String),
    Branch(&'a Branch),
}

/// 报给辅助工具的名字，浮层和输入框都用它：只新建分支时是新建分支，否则是切换分支。
fn picker_title(create_only: bool) -> String {
    if create_only { rust_i18n::t!("git.create_branch") } else { rust_i18n::t!("git.checkout") }.into_owned()
}

impl BranchPicker {
    fn rows(&self, cx: &gpui::App) -> Vec<PickerRow<'_>> {
        let query = self.field.read(cx).query().trim();
        let branches = self.branches.as_deref().unwrap_or_default();
        let mut rows = Vec::new();
        let exists = branches.iter().any(|branch| !branch.remote && branch.name == query);
        if !query.is_empty() && self.valid_name && !exists {
            rows.push(PickerRow::Create(query.to_owned()));
        }
        if !self.create_only {
            let needle = query.to_lowercase();
            rows.extend(
                branches.iter().filter(|branch| branch.name.to_lowercase().contains(&needle)).map(PickerRow::Branch),
            );
        }
        rows
    }
}

impl WindowView {
    /// 打开根目录是 `root` 的仓库的分支列表，在后台读分支；已经开着时关掉。`start` 是新建分支时
    /// 的起点提交，为空时从当前提交。
    pub(super) fn open_branch_picker(
        &mut self,
        root: &Path,
        create_only: bool,
        start: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.branch_picker.is_some() {
            self.close_branch_picker(window, cx);
            return;
        }
        let git = self.workspace().project.git.as_ref();
        let Some(repo) = git.and_then(|git| git.iter().find(|repo| repo.root == root)).map(git::Snapshot::repo) else {
            return;
        };
        let placeholder = match &start {
            Some(start) => {
                rust_i18n::t!("git.picker.new_branch_at", id = start.chars().take(7).collect::<String>())
            }
            None if create_only => rust_i18n::t!("git.picker.new_branch"),
            None => rust_i18n::t!("git.picker.placeholder"),
        };
        let field = cx.new(|cx| {
            TextField::new(String::new(), cx)
                .with_placeholder(placeholder.into_owned())
                .with_label(picker_title(create_only))
        });
        // 输入框原本是搜索框：回车是「下一个」，Esc 是「关闭搜索」，在这里分别是确定和关掉。
        let events = cx.subscribe_in(&field, window, |this, _, event: &TextFieldEvent, window, cx| match event {
            TextFieldEvent::Changed(query) => {
                if let Some(picker) = &mut this.branch_picker {
                    let query = query.trim();
                    picker.valid_name = !query.is_empty() && git::valid_branch_name(query);
                    picker.selected = 0;
                    picker.scroll.scroll_to_item(0);
                }
                cx.notify();
            }
            TextFieldEvent::Next => this.confirm_branch_picker(window, cx),
            TextFieldEvent::Dismiss => this.close_branch_picker(window, cx),
            TextFieldEvent::Previous => {}
        });
        let focus = field.focus_handle(cx);
        // 点到别处就关掉；切到别的应用时窗口失去焦点，回来接着用。
        let blur = cx.on_blur(&focus, window, |this, window, cx| {
            if window.is_window_active() {
                this.branch_picker = None;
                cx.notify();
            }
        });
        window.focus(&focus, cx);
        self.branch_picker = Some(BranchPicker {
            repo: root.to_path_buf(),
            start,
            field,
            branches: None,
            create_only,
            valid_name: false,
            selected: 0,
            scroll: ScrollHandle::new(),
            _subscriptions: [events, blur],
        });
        // 读的时候列表可能关掉又为别的仓库打开了，读完只交给这一次打开的列表。
        let opened = self.branch_picker.as_ref().map(|picker| picker.field.entity_id());
        let job = cx.background_spawn(async move { repo.branches() });
        cx.spawn(async move |this, cx| {
            let branches = job.await;
            this.update(cx, |this, cx| {
                if let Some(picker) = &mut this.branch_picker
                    && Some(picker.field.entity_id()) == opened
                {
                    picker.branches = Some(branches);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// 关掉分支列表；焦点还在输入框里时还给当前终端。
    pub(in crate::window) fn close_branch_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = self.branch_picker.take() else {
            return;
        };
        let focused = picker.field.focus_handle(cx).is_focused(window);
        drop(picker);
        if focused {
            window.focus(&self.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// 上下键选中上一行或下一行，到头了绕回另一头。输入框自己的上下键在这之前拦下。
    fn branch_picker_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(delta) = nav_delta(event) else {
            return;
        };
        cx.stop_propagation();
        let Some(picker) = &self.branch_picker else {
            return;
        };
        let count = picker.rows(cx).len();
        if count == 0 {
            return;
        }
        let next = (picker.selected.min(count - 1) as isize + delta).rem_euclid(count as isize) as usize;
        if let Some(picker) = &mut self.branch_picker {
            picker.selected = next;
            picker.scroll.scroll_to_item(next);
        }
        cx.notify();
    }

    /// 回车：切到选中的分支，或者新建分支。
    fn confirm_branch_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (root, start, choice) = {
            let Some(picker) = &self.branch_picker else {
                return;
            };
            let rows = picker.rows(cx);
            let choice = match rows.get(picker.selected.min(rows.len().saturating_sub(1))) {
                Some(PickerRow::Create(name)) => Ok(name.clone()),
                Some(PickerRow::Branch(branch)) => Err((*branch).clone()),
                None => return,
            };
            (picker.repo.clone(), picker.start.clone(), choice)
        };
        self.close_branch_picker(window, cx);
        match choice {
            Ok(name) => self.create_branch(&root, name, start, window, cx),
            Err(branch) => self.checkout_branch(&root, branch, window, cx),
        }
    }

    /// 浮在标题栏下方正中的列表：上面是输入框，下面是各行。
    pub(in crate::window) fn render_branch_picker(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Option<Div> {
        let picker = self.branch_picker.as_ref()?;
        let rows = picker.rows(cx);
        let selected = picker.selected.min(rows.len().saturating_sub(1));
        let panel_bg = hsla(bg.mix(fg, 0.05));
        let selected_bg = hsla(bg.mix(fg, 0.14));
        let hover_bg = hsla(bg.mix(fg, 0.09));
        let fg = hsla(fg);
        let dim = fg.opacity(0.5);
        let empty = rows.is_empty().then(|| {
            let text = match &picker.branches {
                None if !picker.create_only => rust_i18n::t!("git.picker.loading"),
                _ if picker.create_only => rust_i18n::t!("git.picker.type_name"),
                _ => rust_i18n::t!("git.picker.no_matches"),
            };
            div()
                .id("branch-empty")
                .role(Role::Label)
                .aria_label(text.clone().into_owned())
                .px(px(10.))
                .py(px(10.))
                .text_color(dim)
                .child(text.into_owned())
        });
        let items: Vec<_> = rows
            .into_iter()
            .enumerate()
            .map(|(ix, row)| {
                let current = matches!(row, PickerRow::Branch(branch) if branch.current);
                let (icon, name, detail): (_, SharedString, SharedString) = match &row {
                    PickerRow::Create(name) => (
                        PLUS_ICON,
                        rust_i18n::t!("git.picker.create", name = name).into_owned().into(),
                        SharedString::default(),
                    ),
                    PickerRow::Branch(branch) => {
                        let kind = if branch.remote {
                            rust_i18n::t!("git.picker.remote")
                        } else {
                            rust_i18n::t!("git.picker.local")
                        };
                        let detail = [kind.as_ref(), branch.subject.as_str(), branch.date.as_str()]
                            .into_iter()
                            .filter(|part| !part.is_empty())
                            .collect::<Vec<_>>()
                            .join(" · ");
                        (
                            if branch.current { CHECK_ICON } else { BRANCH_ICON },
                            branch.name.clone().into(),
                            detail.into(),
                        )
                    }
                };
                let view = cx.entity().downgrade();
                div()
                    .id(SharedString::from(match &row {
                        PickerRow::Create(_) => "branch-create".to_owned(),
                        PickerRow::Branch(branch) => format!("branch-{}-{}", branch.remote, branch.name),
                    }))
                    .role(Role::ListBoxOption)
                    .aria_label(name.clone())
                    .aria_description(detail.clone())
                    .aria_selected(ix == selected)
                    // 当前分支前面打着勾。
                    .when(current, |item| item.aria_toggled(true.into()))
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(8.))
                    .rounded(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .map(
                        |item| if ix == selected { item.bg(selected_bg) } else { item.hover(|item| item.bg(hover_bg)) },
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            if let Some(picker) = &mut this.branch_picker {
                                picker.selected = ix;
                            }
                            this.confirm_branch_picker(window, cx);
                        }),
                    )
                    // 列表滚动着，滚出去的行辅助工具也要按得到，不靠合成的鼠标点击。
                    .on_a11y_press(view, move |this, window, cx| {
                        if let Some(picker) = &mut this.branch_picker {
                            picker.selected = ix;
                        }
                        this.confirm_branch_picker(window, cx);
                    })
                    .child(svg().flex_none().path(icon).size(px(14.)).text_color(fg.opacity(0.7)))
                    .child(div().flex_none().max_w(px(PICKER_WIDTH * 0.5)).truncate().text_color(fg).child(name))
                    .child(div().flex_1().min_w_0().truncate().text_size(px(11.)).text_color(dim).child(detail))
            })
            .collect();
        let title = picker_title(picker.create_only);
        let panel = div()
            .id("branch-picker")
            .role(Role::Dialog)
            .aria_label(title.clone())
            .w(px(PICKER_WIDTH))
            .capture_key_down(cx.listener(Self::branch_picker_key));
        let list = div()
            .id("branch-list")
            .role(Role::ListBox)
            .aria_label(title)
            .max_h(px(ROW_HEIGHT * VISIBLE_ROWS + 8.))
            .track_scroll(&picker.scroll)
            .children(items)
            .children(empty);
        Some(picker_panel(panel, picker.field.clone(), list, fg, panel_bg))
    }
}
