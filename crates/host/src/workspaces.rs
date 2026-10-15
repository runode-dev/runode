//! 宿主手里的布局：界面（桌面）的布局一变就连同新布局报来（`ClientMsg::LayoutChanged`），宿主记在
//! `Peers::layout`。没有界面连着时（app 没开，宿主单独跑着），手机、命令行要的布局（`Layout`）照它回，
//! 在工作区里开终端（`Open`）、新建工作区（`OpenWorkspace`）、给工作区改名（`RenameWorkspace`）也由
//! 宿主自己办：改这份布局，新开的会话放进去。界面再连上时，恢复窗口前要回这份布局（界面的连接发
//! `Layout`，宿主自己回），把多出来的会话和改过的名字接进自己的存档。
//!
//! 没有窗口可显示：开终端一律开成工作区里排在最后的新标签（要分屏的也是），不切过去；切到终端
//! （`Reveal`）回 `Error`。已经不在宿主里的会话每次用这份布局前去掉，见 `tidy`。

use std::{fs, path::PathBuf, sync::mpsc};

use runode_protocol::{HostMsg, PaneLayout, PaneRect, SessionId, TabLayout, WindowLayout, WorkspaceLayout};
use runode_types::{grid::GridSize, shell::IntegrationMode};

use crate::{Shared, SpawnOptions, server::REPLY_TIMEOUT, session::Inbox};

/// 宿主自己开的会话一开始的尺寸：没有窗口，等有前端带着尺寸连上来时再改。
const SPAWN_SIZE: GridSize = GridSize { cols: 80, rows: 24, cell_width_px: 8, cell_height_px: 16 };

impl Shared {
    /// 宿主手里的布局，去掉了已经不在的会话。
    pub(crate) fn held_layout(&self) -> Vec<WindowLayout> {
        let mut peers = self.peers();
        self.tidy(&mut peers.layout);
        peers.layout.clone()
    }

    /// 没有界面时办 `OpenWorkspace`：最前面的窗口里已经有 `dir` 这个目录的工作区时回它当前标签里有焦点
    /// 的分屏，它一个标签都没有时在里面开一个；没有时在 `dir` 开一个会话，放进排在最后的新工作区。
    pub(crate) fn open_workspace(&self, req: u32, dir: PathBuf, name: Option<String>) -> Result<HostMsg, String> {
        if !dir.is_absolute() || !dir.is_dir() {
            return Err(format!("{} is not a directory", dir.display()));
        }
        let canonical = fs::canonicalize(&dir).unwrap_or(dir);
        // 布局里的目录照界面记的写法，可能经过软链接：和界面一样两边都规范化了再比。规范化要 stat，在锁外
        // 做，卡住的网络挂载不能占着锁；回到锁里时那个工作区的目录没变才算，布局这期间换过时按规范化的写法
        // 再找一遍。
        let known: Vec<(Option<PathBuf>, Option<SessionId>)> = {
            let mut peers = self.peers();
            self.tidy(&mut peers.layout);
            let front = front(&peers.layout);
            peers.layout[front].workspaces.iter().map(|workspace| (workspace.dir.clone(), anchor(workspace))).collect()
        };
        let wanted = known.iter().position(|(known, _)| {
            known.as_ref().is_some_and(|known| fs::canonicalize(known).unwrap_or_else(|_| known.clone()) == canonical)
        });
        let find = |workspaces: &[WorkspaceLayout]| {
            wanted
                .filter(|&ix| workspaces.get(ix).is_some_and(|workspace| workspace.dir == known[ix].0))
                .or_else(|| workspaces.iter().position(|workspace| workspace.dir.as_ref() == Some(&canonical)))
        };
        // 那个工作区已经有终端：规范化期间会话可能被结束、布局可能整个换过，回到锁里按现在的布局再认一遍，
        // 还在才回它，不然照没有终端往下开。
        if wanted.is_some_and(|ix| known[ix].1.is_some()) {
            let mut peers = self.peers();
            self.tidy(&mut peers.layout);
            let workspaces = &peers.layout[front(&peers.layout)].workspaces;
            if let Some(id) = find(workspaces).and_then(|ix| anchor(&workspaces[ix])) {
                return Ok(HostMsg::Opened { req, id });
            }
        }
        let id = self.spawn(spawn_options(Some(canonical.clone()))).map_err(|err| format!("{err:#}"))?;
        let mut peers = self.peers();
        self.tidy(&mut peers.layout);
        let front = front(&peers.layout);
        let workspaces = &mut peers.layout[front].workspaces;
        let ix = find(workspaces);
        // 同一个工作区的另一个请求先开好了：回它开的，这边刚开的结束掉，和工作区已经有终端时一样。
        if let Some(existing) = ix.and_then(|ix| anchor(&workspaces[ix])) {
            self.kill(id);
            return Ok(HostMsg::Opened { req, id: existing });
        }
        match ix {
            Some(ix) => workspaces[ix].tabs.push(tab(id)),
            None => {
                let name = name.map(|name| name.trim().to_owned()).filter(|name| !name.is_empty());
                let tabs = vec![tab(id)];
                workspaces.push(WorkspaceLayout { index: 0, name, dir: Some(canonical), active: false, tabs });
            }
        }
        self.tidy(&mut peers.layout);
        peers.layout_changed();
        Ok(HostMsg::Opened { req, id })
    }

    /// 没有界面时办 `Open`：在 `near` 所在的工作区（为空时最前面的窗口当前的工作区）里开一个新标签。
    /// `cwd` 为空时沿用 `near` 的目录，再不行用工作区的目录。
    pub(crate) fn open_tab(&self, req: u32, near: Option<SessionId>, cwd: Option<PathBuf>) -> Result<HostMsg, String> {
        let (place, dir) = {
            let mut peers = self.peers();
            self.tidy(&mut peers.layout);
            let Some((window, workspace)) = locate(&peers.layout, near) else {
                return Err(match near {
                    Some(id) => format!("no workspace has session {id}"),
                    None => "there is no workspace to open it in".into(),
                });
            };
            ((window, workspace), peers.layout[window].workspaces[workspace].dir.clone())
        };
        let cwd = cwd.or_else(|| near.and_then(|id| self.cwd(id))).or(dir);
        let id = self.spawn(spawn_options(cwd)).map_err(|err| format!("{err:#}"))?;
        let mut peers = self.peers();
        self.tidy(&mut peers.layout);
        // 开会话的这一会儿 `near` 没了的话放进先前找到的那个工作区（宿主自己不删工作区）；布局整个换过、
        // 那里没有工作区了时，新会话不在哪个工作区里，照样回给请求方。
        let (window, workspace) = locate(&peers.layout, near).unwrap_or(place);
        if let Some(target) = peers.layout.get_mut(window).and_then(|it| it.workspaces.get_mut(workspace)) {
            target.tabs.push(tab(id));
            self.tidy(&mut peers.layout);
            peers.layout_changed();
        }
        Ok(HostMsg::Opened { req, id })
    }

    /// 没有界面时办 `RenameWorkspace`。
    pub(crate) fn rename_workspace(
        &self,
        req: u32,
        window: u32,
        workspace: u32,
        name: &str,
    ) -> Result<HostMsg, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("the workspace name is empty".into());
        }
        let mut peers = self.peers();
        self.tidy(&mut peers.layout);
        let found = (window.checked_sub(1).zip(workspace.checked_sub(1)))
            .and_then(|(w, ws)| peers.layout.get_mut(w as usize)?.workspaces.get_mut(ws as usize));
        let Some(found) = found else {
            return Err(format!("there is no workspace {workspace} in window {window}"));
        };
        found.name = Some(name.to_owned());
        peers.layout_changed();
        Ok(HostMsg::Done { req })
    }

    /// 按宿主里现在的会话整理 `layout`，见 `tidy`。锁的先后：调用方拿着 `peers`，这里再拿 `registry`。
    fn tidy(&self, layout: &mut Vec<WindowLayout>) {
        let registry = self.registry();
        tidy(layout, |id| registry.sessions.contains_key(&id));
    }

    /// 会话 `id` 的 shell 现在所在的目录；会话不在、没回话、不知道目录时为空。
    fn cwd(&self, id: SessionId) -> Option<PathBuf> {
        let (reply, info) = mpsc::channel();
        if !self.deliver(id, Inbox::Info(reply)) {
            return None;
        }
        info.recv_timeout(REPLY_TIMEOUT).ok()?.meta.cwd
    }
}

/// 宿主自己开会话（没有界面时开终端、开始任务）用的选项：从 `cwd` 开始，尺寸是 `SPAWN_SIZE`。
pub(crate) fn spawn_options(cwd: Option<PathBuf>) -> SpawnOptions {
    SpawnOptions {
        size: SPAWN_SIZE,
        cwd,
        integration: IntegrationMode::Detect,
        start: true,
        shell: None,
        settings: None,
    }
}

/// 只有一个分屏的标签，序号由 `tidy` 排。
fn tab(id: SessionId) -> TabLayout {
    let rect = PaneRect { x: 0, y: 0, width: PaneRect::EXTENT, height: PaneRect::EXTENT };
    TabLayout { index: 0, active: false, panes: vec![PaneLayout { index: 0, id, rect, focused: true }] }
}

/// 工作区当前标签里有焦点的分屏的会话；一个标签都没有时为空。`tidy` 过的布局里当前标签、焦点都标着。
fn anchor(workspace: &WorkspaceLayout) -> Option<SessionId> {
    let tab = workspace.tabs.iter().find(|tab| tab.active)?;
    tab.panes.iter().find(|pane| pane.focused).map(|pane| pane.id)
}

/// 最前面的窗口在 `windows` 里的下标。`tidy` 过的布局里总有一个窗口标着 `front`。
fn front(windows: &[WindowLayout]) -> usize {
    windows.iter().position(|window| window.front).unwrap_or(0)
}

/// `near` 所在的工作区（窗口和工作区在各自列表里的下标）；`near` 为空时是最前面的窗口当前的工作区。
fn locate(windows: &[WindowLayout], near: Option<SessionId>) -> Option<(usize, usize)> {
    let Some(near) = near else {
        let window = front(windows);
        let workspace = windows.get(window)?.workspaces.iter().position(|workspace| workspace.active)?;
        return Some((window, workspace));
    };
    windows.iter().enumerate().find_map(|(w, window)| {
        let has =
            |workspace: &WorkspaceLayout| workspace.tabs.iter().flat_map(|tab| &tab.panes).any(|pane| pane.id == near);
        Some((w, window.workspaces.iter().position(has)?))
    })
}

/// 去掉 `alive` 为假的会话；空了的标签去掉，工作区留着，和界面上关掉最后一个标签一样。各级序号按先后
/// 重排；当前的标签、有焦点的分屏、当前的工作区、最前面的窗口没标着时，标上最后一个标签、第一个分屏、
/// 第一个工作区、第一个窗口。一个窗口都没有时放一个空的：手机看到有窗口才新建工作区（`OpenWorkspace`），
/// 不然只开后台会话。
pub(crate) fn tidy(windows: &mut Vec<WindowLayout>, alive: impl Fn(SessionId) -> bool) {
    if windows.is_empty() {
        windows.push(WindowLayout { index: 1, front: true, workspaces: Vec::new() });
    }
    for (window, index) in windows.iter_mut().zip(1..) {
        window.index = index;
        for (workspace, index) in window.workspaces.iter_mut().zip(1..) {
            workspace.index = index;
            for tab in &mut workspace.tabs {
                tab.panes.retain(|pane| alive(pane.id));
            }
            workspace.tabs.retain(|tab| !tab.panes.is_empty());
            for (tab, index) in workspace.tabs.iter_mut().zip(1..) {
                tab.index = index;
                for (pane, index) in tab.panes.iter_mut().zip(1..) {
                    pane.index = index;
                }
                mark_one(&mut tab.panes, |pane| &mut pane.focused, false);
            }
            mark_one(&mut workspace.tabs, |tab| &mut tab.active, true);
        }
        mark_one(&mut window.workspaces, |workspace| &mut workspace.active, false);
    }
    mark_one(windows, |window| &mut window.front, false);
}

/// `items` 里一个都没标着时标上第一个，`last` 时标上最后一个。
fn mark_one<T>(items: &mut [T], flag: impl Fn(&mut T) -> &mut bool, last: bool) {
    if items.iter_mut().any(|item| *flag(item)) {
        return;
    }
    if let Some(item) = if last { items.last_mut() } else { items.first_mut() } {
        *flag(item) = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> SessionId {
        SessionId(n)
    }

    fn pane(n: u128, focused: bool) -> PaneLayout {
        let rect = PaneRect { x: 0, y: 0, width: PaneRect::EXTENT, height: PaneRect::EXTENT };
        PaneLayout { index: 9, id: id(n), rect, focused }
    }

    fn workspace(dir: &str, active: bool, tabs: Vec<TabLayout>) -> WorkspaceLayout {
        WorkspaceLayout { index: 9, name: None, dir: Some(dir.into()), active, tabs }
    }

    #[test]
    fn an_empty_layout_gets_one_empty_front_window() {
        let mut windows = Vec::new();
        tidy(&mut windows, |_| true);
        assert_eq!(windows, [WindowLayout { index: 1, front: true, workspaces: Vec::new() }]);
    }

    #[test]
    fn gone_sessions_leave_and_marks_and_numbers_follow() {
        let split = TabLayout { index: 9, active: false, panes: vec![pane(1, true), pane(2, false)] };
        let lone = TabLayout { index: 9, active: true, panes: vec![pane(3, true)] };
        let mut windows = vec![WindowLayout {
            index: 7,
            front: false,
            workspaces: vec![workspace("/a", false, vec![split, lone]), workspace("/b", true, vec![tab(id(4))])],
        }];
        // 会话 1（分屏里有焦点的那个）、3（当前标签里唯一的）、4（工作区里唯一的）都没了。
        tidy(&mut windows, |id| id == SessionId(2));
        let [window] = windows.as_slice() else { panic!("one window") };
        assert_eq!((window.index, window.front), (1, true));
        let [a, b] = window.workspaces.as_slice() else { panic!("two workspaces") };
        assert_eq!((a.index, a.active, b.index, b.active), (1, false, 2, true));
        assert!(b.tabs.is_empty(), "the workspace stays without tabs");
        let [left] = a.tabs.as_slice() else { panic!("the emptied tab is gone") };
        assert!(left.active);
        assert_eq!(left.panes, [PaneLayout { index: 1, focused: true, ..pane(2, false) }]);
    }

    #[test]
    fn open_goes_next_to_near_or_into_the_front_windows_active_workspace() {
        let back = WindowLayout { index: 1, front: false, workspaces: vec![workspace("/a", true, vec![tab(id(1))])] };
        let front = WindowLayout {
            index: 2,
            front: true,
            workspaces: vec![workspace("/b", false, vec![tab(id(2))]), workspace("/c", true, Vec::new())],
        };
        let windows = vec![back, front];
        assert_eq!(locate(&windows, Some(id(1))), Some((0, 0)));
        assert_eq!(locate(&windows, Some(id(2))), Some((1, 0)));
        assert_eq!(locate(&windows, None), Some((1, 1)));
        assert_eq!(locate(&windows, Some(id(9))), None);
    }
}
