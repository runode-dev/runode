//! 回答别的进程问 app 里各个终端摆在哪（`ClientMsg::Layout`）：窗口、工作区、标签和分屏，命令行
//! 据此按位置找终端、列出每个会话在哪，手机据此按工作区把会话分组。
//!
//! 窗口按打开的先后从 1 编号，最前面那个标 `front`；工作区、标签按界面上的先后从 1 编号，当前的
//! 标 `active`，工作区还带着侧栏里的名字和它的目录；分屏按 `Node::leaves` 的叶子顺序从 1 编号，有焦点的标 `focused`。分屏的位置按
//! 整个标签区域 0..`PaneRect::EXTENT` 归一化，由分屏树按比例算出，不看实际画出来的像素，所以
//! 后台标签也有。放大着的分屏照常按没放大时的布局给：放大只是暂时把别的分屏挡住，命令行按
//! 左右上下找相邻分屏时要的是分屏之间的位置关系，放大时也不变。

use std::{collections::HashMap, path::Path};

use gpui::{App, EntityId, Global, WindowHandle};
use runode_protocol::{PaneLayout, PaneRect, SessionId, TabLayout, WindowLayout, WorkspaceLayout};
use runode_shared_types::pane::{Node, Rect};

use super::WindowView;

/// 回话里的一个窗口，从 `WindowView` 摘出来的、算布局要的部分。
pub(super) struct WindowInput<'a, P> {
    /// 打开的先后，小的先开。
    pub(super) opened: u64,
    pub(super) front: bool,
    pub(super) active: usize,
    pub(super) workspaces: Vec<WorkspaceInput<'a, P>>,
}

pub(super) struct WorkspaceInput<'a, P> {
    pub(super) name: String,
    pub(super) dir: &'a Path,
    pub(super) active: usize,
    pub(super) tabs: Vec<TabInput<'a, P>>,
}

pub(super) struct TabInput<'a, P> {
    pub(super) root: &'a Node<P>,
    pub(super) focused: P,
}

/// 按 `WindowInput` 生成回话里的布局，`session` 给出分屏里的会话（找不到的分屏不列出，序号仍按
/// 它在叶子里的位置）。
pub(super) fn report<P: Copy + PartialEq>(
    mut windows: Vec<WindowInput<'_, P>>,
    session: impl Fn(P) -> Option<SessionId>,
) -> Vec<WindowLayout> {
    windows.sort_by_key(|window| window.opened);
    windows
        .iter()
        .zip(1..)
        .map(|(window, index)| WindowLayout {
            index,
            front: window.front,
            workspaces: window
                .workspaces
                .iter()
                .enumerate()
                .zip(1..)
                .map(|((wi, workspace), index)| WorkspaceLayout {
                    index,
                    name: Some(workspace.name.clone()),
                    dir: Some(workspace.dir.to_path_buf()),
                    active: wi == window.active,
                    tabs: workspace
                        .tabs
                        .iter()
                        .enumerate()
                        .zip(1..)
                        .map(|((ti, tab), index)| TabLayout {
                            index,
                            active: ti == workspace.active,
                            panes: panes(tab, &session),
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect()
}

fn panes<P: Copy + PartialEq>(tab: &TabInput<'_, P>, session: &impl Fn(P) -> Option<SessionId>) -> Vec<PaneLayout> {
    let extent = f32::from(PaneRect::EXTENT);
    let area = Rect { x: 0., y: 0., width: extent, height: extent };
    tab.root
        .rects(area)
        .into_iter()
        .zip(1..)
        .filter_map(|((pane, rect), index)| {
            Some(PaneLayout { index, id: session(pane)?, rect: pane_rect(rect), focused: pane == tab.focused })
        })
        .collect()
}

/// 取整成 `PaneRect`。两条边各自取整再相减，相邻两块的边取整后仍正好接上。
fn pane_rect(rect: Rect) -> PaneRect {
    let edge = |v: f32| v.round().clamp(0., f32::from(PaneRect::EXTENT)) as u16;
    let (x, y) = (edge(rect.x), edge(rect.y));
    PaneRect { x, y, width: edge(rect.x + rect.width) - x, height: edge(rect.y + rect.height) - y }
}

/// 各个窗口打开的先后：`WindowView` 一建出来就记下序号，窗口关掉时删掉。
#[derive(Default)]
struct OpenOrder {
    next: u64,
    opened: HashMap<EntityId, u64>,
}

impl Global for OpenOrder {}

/// 开始记窗口打开的先后，要在开第一个窗口之前调。
pub(super) fn track_windows(cx: &mut App) {
    cx.set_global(OpenOrder::default());
    cx.observe_new(|_: &mut WindowView, _, cx| {
        let id = cx.entity_id();
        let order = cx.global_mut::<OpenOrder>();
        order.next += 1;
        let next = order.next;
        order.opened.insert(id, next);
        cx.on_release(move |_, cx| {
            cx.global_mut::<OpenOrder>().opened.remove(&id);
        })
        .detach();
    })
    .detach();
}

/// 布局里第 `index` 个窗口，序号和 `current` 编的一样，从 1 数。
pub(super) fn window_at(index: u32, cx: &mut App) -> Option<WindowHandle<WindowView>> {
    let mut windows = Vec::new();
    for handle in super::remote::windows(cx) {
        if let Ok(id) = handle.update(cx, |_, _, cx| cx.entity_id()) {
            windows.push((id, handle));
        }
    }
    let order = cx.try_global::<OpenOrder>();
    let opened = |id: EntityId| order.and_then(|order| order.opened.get(&id).copied()).unwrap_or(u64::MAX);
    windows.sort_by_key(|(id, _)| opened(*id));
    let ix = usize::try_from(index.checked_sub(1)?).ok()?;
    windows.into_iter().nth(ix).map(|(_, handle)| handle)
}

/// app 里所有窗口现在的布局。
pub(super) fn current(cx: &mut App) -> Vec<WindowLayout> {
    let front = super::remote::front_window(cx);
    let handles = super::remote::windows(cx);
    let mut views = Vec::new();
    for handle in handles {
        if let Ok(root) = handle.update(cx, |_, _, cx| cx.entity()) {
            views.push((Some(handle) == front, root));
        }
    }
    let order = cx.try_global::<OpenOrder>();
    let opened = |id: EntityId| order.and_then(|order| order.opened.get(&id).copied()).unwrap_or(u64::MAX);
    let mut inputs = Vec::new();
    for (front, root) in &views {
        let view = root.read(cx);
        inputs.push(WindowInput {
            opened: opened(root.entity_id()),
            front: *front,
            active: view.active,
            workspaces: view
                .workspaces
                .iter()
                .map(|workspace| WorkspaceInput {
                    name: workspace.name.to_string(),
                    dir: &workspace.dir,
                    active: workspace.active,
                    tabs: workspace.tabs.iter().map(|tab| TabInput { root: &tab.root, focused: tab.focused }).collect(),
                })
                .collect(),
        });
    }
    // 分屏到会话：每个窗口的每个标签里各自查。
    let sessions: HashMap<EntityId, SessionId> = views
        .iter()
        .flat_map(|(_, root)| root.read(cx).workspaces.iter())
        .flat_map(|workspace| &workspace.tabs)
        .flat_map(|tab| &tab.panes)
        .filter_map(|(pane, (view, _))| Some((*pane, view.read(cx).session_id()?)))
        .collect();
    report(inputs, |pane| sessions.get(&pane).copied())
}

#[cfg(test)]
mod tests {
    use runode_shared_types::pane::Axis;

    use super::*;

    fn id(n: u32) -> SessionId {
        SessionId(u128::from(n))
    }

    fn rect(x: u16, y: u16, width: u16, height: u16) -> PaneRect {
        PaneRect { x, y, width, height }
    }

    /// 1 在左，右边 2 在上、3 在下；左右三七开。
    fn nested() -> Node<u32> {
        let mut root = Node::Leaf(1);
        root.split(1, 2, Axis::Horizontal, 10);
        root.set_ratio(10, 0.3);
        root.split(2, 3, Axis::Vertical, 11);
        root
    }

    #[test]
    fn panes_follow_the_leaf_order_and_split_the_tab_area() {
        let root = nested();
        let windows = vec![WindowInput {
            opened: 1,
            front: true,
            active: 0,
            workspaces: vec![WorkspaceInput {
                name: "app".into(),
                dir: Path::new("/Users/me/app"),
                active: 0,
                tabs: vec![TabInput { root: &root, focused: 3 }],
            }],
        }];
        let report = report(windows, |pane| Some(id(pane)));
        let panes = &report[0].workspaces[0].tabs[0].panes;
        let got: Vec<_> = panes.iter().map(|pane| (pane.index, pane.id, pane.rect, pane.focused)).collect();
        assert_eq!(
            got,
            [
                (1, id(1), rect(0, 0, 300, 1000), false),
                (2, id(2), rect(300, 0, 700, 500), false),
                (3, id(3), rect(300, 500, 700, 500), true),
            ]
        );
    }

    #[test]
    fn rounded_edges_still_meet() {
        // 三等分：333.33 和 666.67 两条边取整后相邻的两块正好接上，总宽还是 1000。
        let mut root = Node::Leaf(1);
        root.split(1, 2, Axis::Horizontal, 10);
        root.set_ratio(10, 1. / 3.);
        root.split(2, 3, Axis::Horizontal, 11);
        let rects: Vec<_> = root
            .rects(Rect { x: 0., y: 0., width: 1000., height: 1000. })
            .into_iter()
            .map(|(_, r)| pane_rect(r))
            .collect();
        assert_eq!(rects, [rect(0, 0, 333, 1000), rect(333, 0, 334, 1000), rect(667, 0, 333, 1000)]);
    }

    #[test]
    fn windows_are_numbered_by_opening_order_and_mark_the_front() {
        let a = Node::Leaf(1);
        let b = Node::Leaf(2);
        let one_tab =
            |name: &str, root| WorkspaceInput { name: name.into(), dir: Path::new("/"), active: 0, tabs: vec![root] };
        // 后开的窗口排在前面给，序号仍按打开的先后。
        let windows = vec![
            WindowInput {
                opened: 7,
                front: true,
                active: 0,
                workspaces: vec![one_tab("later", TabInput { root: &b, focused: 2 })],
            },
            WindowInput {
                opened: 3,
                front: false,
                active: 0,
                workspaces: vec![one_tab("first", TabInput { root: &a, focused: 1 })],
            },
        ];
        let report = report(windows, |pane| Some(id(pane)));
        let got: Vec<_> = report
            .iter()
            .map(|w| (w.index, w.front, w.workspaces[0].name.clone().unwrap(), w.workspaces[0].tabs[0].panes[0].id))
            .collect();
        assert_eq!(got, [(1, false, "first".to_owned(), id(1)), (2, true, "later".to_owned(), id(2))]);
    }

    #[test]
    fn workspaces_and_tabs_mark_the_active_ones_and_background_tabs_have_rects() {
        let shown = Node::Leaf(1);
        let background = nested();
        let other = Node::Leaf(9);
        let windows = vec![WindowInput {
            opened: 1,
            front: true,
            active: 1,
            workspaces: vec![
                WorkspaceInput {
                    name: "a".into(),
                    dir: Path::new("/Users/me/a"),
                    active: 0,
                    tabs: vec![TabInput { root: &other, focused: 9 }],
                },
                WorkspaceInput {
                    name: "b".into(),
                    dir: Path::new("/Users/me/b"),
                    active: 0,
                    tabs: vec![TabInput { root: &shown, focused: 1 }, TabInput { root: &background, focused: 2 }],
                },
            ],
        }];
        let report = report(windows, |pane| Some(id(pane)));
        let workspaces = &report[0].workspaces;
        assert_eq!(workspaces.iter().map(|w| (w.index, w.active)).collect::<Vec<_>>(), [(1, false), (2, true)]);
        // 每个工作区带着自己的目录。
        let dirs: Vec<_> = workspaces.iter().map(|w| w.dir.clone()).collect();
        assert_eq!(dirs, [Some("/Users/me/a".into()), Some("/Users/me/b".into())]);
        let tabs = &workspaces[1].tabs;
        assert_eq!(tabs.iter().map(|t| (t.index, t.active)).collect::<Vec<_>>(), [(1, true), (2, false)]);
        // 后台标签的分屏也按比例给出位置，焦点照样标出来。
        let back = &tabs[1].panes;
        assert_eq!(back.len(), 3);
        assert_eq!(back[1].rect, rect(300, 0, 700, 500));
        assert!(back[1].focused && !back[0].focused && !back[2].focused);
    }

    #[test]
    fn a_zoomed_pane_keeps_its_place_in_the_split() {
        // 放大不改分屏树，报告里只看树：放大着的分屏仍是它在分屏里的位置，别的分屏也照常列出。
        let root = nested();
        let tab = TabInput { root: &root, focused: 2 };
        let panes = panes(&tab, &|pane| Some(id(pane)));
        assert_eq!(panes.len(), 3);
        assert_eq!(panes[1].rect, rect(300, 0, 700, 500));
        assert!(panes[1].focused);
    }

    #[test]
    fn panes_without_a_session_are_left_out_but_keep_numbering() {
        let root = nested();
        let tab = TabInput { root: &root, focused: 1 };
        let panes = panes(&tab, &|pane| (pane != 2).then(|| id(pane)));
        assert_eq!(panes.iter().map(|pane| pane.index).collect::<Vec<_>>(), [1, 3]);
    }
}
