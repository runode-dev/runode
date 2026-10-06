//! 按写法找会话（命令行里的 SESSION）：标识或它的前缀、自己、按位置（左右上下、同一标签里的
//! 前后、第几个窗口、工作区、标签和分屏），以及按标题、agent 和目录。写法见 `args::HELP`。
//!
//! 按位置找要 app 里各个终端摆在哪（`HostMsg::Layout`），由 app 的界面回答；app 没开窗口时
//! 回不了，这时只有按位置的写法报错，别的照常。必须正好对上一个会话，对上几个时列出候选。

use std::{
    fmt,
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow, bail};
use runode_protocol::{
    ClientMsg, HostMsg, PaneLayout, PaneRect, SessionId, SessionInfo, TabLayout, WindowLayout, WorkspaceLayout,
};
use runode_shared_types::{
    agent::AgentState,
    pane::{Direction, Rect, neighbor},
};

use crate::{
    Env,
    client::{Connection, kind},
    commands::{state_name, tilde, title},
};

/// 一种找会话的写法。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Selector {
    /// 标识或它的前缀，小写。
    Id(String),
    /// 自己所在的会话。
    Own,
    /// 自己旁边那个分屏。
    Beside(Direction),
    /// 同一个标签里按分屏顺序的下一个（`true`）或上一个，转圈。
    Cycle(bool),
    /// 按窗口、工作区、标签、分屏的序号找；没给的按自己所在的（不在 runode 里时按最前面的窗口）。
    At { window: Option<u32>, workspace: Option<u32>, spot: Spot },
    /// 标题里含着这段文字，不分大小写。
    Title(String),
    /// 前台在跑这种 agent（`AgentKind::label`），给了状态时还要在这个状态。
    Agent { kind: String, state: Option<AgentState> },
    /// 在这个目录里：含 `/` 或以 `~`、`.` 开头的按路径比，否则比目录的名字。
    Cwd(String),
}

/// `Selector::At` 在工作区里找哪个分屏。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Spot {
    /// 当前标签里有焦点的分屏（只写了 `win:`、`ws:` 时）。
    Focused,
    /// 第几个标签，给了分屏序号时是它的第几个分屏，否则是它有焦点的那个。
    Tab(u32, Option<u32>),
    /// 当前标签里第几个分屏。
    Pane(u32),
}

impl Selector {
    /// 解析一种写法，写错时返回说明。
    pub(crate) fn parse(given: &str) -> Result<Self, String> {
        let bad = |why: &str| format!("cannot read the session {given}: {why}");
        match given {
            "self" | "." => return Ok(Self::Own),
            "left" => return Ok(Self::Beside(Direction::Left)),
            "right" => return Ok(Self::Beside(Direction::Right)),
            "up" => return Ok(Self::Beside(Direction::Up)),
            "down" => return Ok(Self::Beside(Direction::Down)),
            "next" => return Ok(Self::Cycle(true)),
            "prev" => return Ok(Self::Cycle(false)),
            _ => {}
        }
        // 这三种的值里可能有 `/` 和 `:`，先认。
        if let Some(text) = given.strip_prefix("title:") {
            return if text.is_empty() { Err(bad("title: needs some text")) } else { Ok(Self::Title(text.into())) };
        }
        if let Some(dir) = given.strip_prefix("cwd:") {
            return if dir.is_empty() { Err(bad("cwd: needs a directory")) } else { Ok(Self::Cwd(dir.into())) };
        }
        if let Some(agent) = given.strip_prefix("agent:") {
            let (kind, state) = match agent.split_once(':') {
                Some((kind, state)) => (kind, Some(agent_state(state).ok_or_else(|| bad("unknown agent state"))?)),
                None => (agent, None),
            };
            if kind.is_empty() {
                return Err(bad("agent: needs a kind such as claude or codex"));
            }
            return Ok(Self::Agent { kind: kind.to_ascii_lowercase(), state });
        }
        if given.contains(':') {
            return at(given).ok_or_else(|| bad("positions are win:W, ws:K, tab:N, tab:N.M and pane:N, joined by /"));
        }
        if !given.is_empty() && given.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(Self::Id(given.to_ascii_lowercase()));
        }
        Err(bad("not an id, a position or a filter; see `runode help`"))
    }

    /// 要知道各个终端摆在哪才找得到。
    pub(crate) fn needs_layout(&self) -> bool {
        matches!(self, Self::Beside(_) | Self::Cycle(_) | Self::At { .. })
    }
}

impl fmt::Display for Selector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Id(prefix) => f.write_str(prefix),
            Self::Own => f.write_str("self"),
            Self::Beside(direction) => f.write_str(direction_name(*direction)),
            Self::Cycle(forward) => f.write_str(if *forward { "next" } else { "prev" }),
            Self::At { window, workspace, spot } => {
                let mut parts = Vec::new();
                parts.extend(window.map(|w| format!("win:{w}")));
                parts.extend(workspace.map(|k| format!("ws:{k}")));
                match spot {
                    Spot::Focused => {}
                    Spot::Tab(n, None) => parts.push(format!("tab:{n}")),
                    Spot::Tab(n, Some(m)) => parts.push(format!("tab:{n}.{m}")),
                    Spot::Pane(n) => parts.push(format!("pane:{n}")),
                }
                f.write_str(&parts.join("/"))
            }
            Self::Title(text) => write!(f, "title:{text}"),
            Self::Agent { kind, state: None } => write!(f, "agent:{kind}"),
            Self::Agent { kind, state: Some(state) } => write!(f, "agent:{kind}:{}", state_name(*state)),
            Self::Cwd(dir) => write!(f, "cwd:{dir}"),
        }
    }
}

/// `win:W/ws:K/tab:N.M` 这类按序号的写法，各段按这个顺序、都可以不写，但至少有一段。
fn at(given: &str) -> Option<Selector> {
    let number = |s: &str| s.parse::<u32>().ok().filter(|&n| n > 0);
    let (mut window, mut workspace, mut spot) = (None, None, None);
    for part in given.split('/') {
        let (key, value) = part.split_once(':')?;
        match key {
            "win" if window.is_none() && workspace.is_none() && spot.is_none() => window = Some(number(value)?),
            "ws" if workspace.is_none() && spot.is_none() => workspace = Some(number(value)?),
            "tab" if spot.is_none() => {
                spot = Some(match value.split_once('.') {
                    Some((tab, pane)) => Spot::Tab(number(tab)?, Some(number(pane)?)),
                    None => Spot::Tab(number(value)?, None),
                });
            }
            "pane" if spot.is_none() => spot = Some(Spot::Pane(number(value)?)),
            _ => return None,
        }
    }
    Some(Selector::At { window, workspace, spot: spot.unwrap_or(Spot::Focused) })
}

fn agent_state(name: &str) -> Option<AgentState> {
    Some(match name {
        "working" => AgentState::Working,
        "idle" => AgentState::Idle,
        "blocked" => AgentState::Blocked,
        _ => return None,
    })
}

pub(crate) fn direction_name(direction: Direction) -> &'static str {
    match direction {
        Direction::Left => "left",
        Direction::Right => "right",
        Direction::Up => "up",
        Direction::Down => "down",
    }
}

/// 找会话要用的东西：所有会话、自己是哪个，以及 app 里各个终端摆在哪。
pub(crate) struct World {
    pub(crate) sessions: Vec<SessionInfo>,
    /// app 回答的布局；没问过，或者 app 没开窗口回答不了时为 `None`。
    pub(crate) layout: Option<Vec<WindowLayout>>,
    /// 自己所在的会话，见 `Env::session`。
    pub(crate) own: Option<SessionId>,
    /// 家目录，见 `Env::home`。
    pub(crate) home: Option<PathBuf>,
}

/// 一个分屏在布局里的位置。
#[derive(Clone, Copy)]
pub(crate) struct Place<'a> {
    pub(crate) window: &'a WindowLayout,
    pub(crate) workspace: &'a WorkspaceLayout,
    pub(crate) tab: &'a TabLayout,
    pub(crate) pane: &'a PaneLayout,
}

impl Place<'_> {
    /// 正显示着：窗口当前的工作区里当前的标签。
    pub(crate) fn shown(&self) -> bool {
        self.workspace.active && self.tab.active
    }
}

impl World {
    /// 列出会话；`layout` 时还问 app 各个终端摆在哪。
    pub(crate) fn load(connection: &Connection, env: &Env, layout: bool) -> Result<Self> {
        connection.send(&ClientMsg::ListSessions)?;
        let sessions = match connection.reply()? {
            HostMsg::SessionList { sessions } => sessions,
            other => bail!("unexpected answer: {}", kind(&other)),
        };
        let layout = if layout { connection.layout()? } else { None };
        let own = env.session.as_deref().and_then(|own| own.parse().ok());
        Ok(Self { sessions, layout, own, home: env.home.clone() })
    }

    /// 按写法找正好一个会话。
    pub(crate) fn find(&self, selector: &Selector) -> Result<&SessionInfo> {
        let candidates: Vec<&SessionInfo> = match selector {
            Selector::Id(prefix) => {
                self.sessions.iter().filter(|info| info.id.to_string().starts_with(prefix)).collect()
            }
            Selector::Own => vec![self.session(self.own_id(selector)?, selector)?],
            Selector::Beside(_) | Selector::Cycle(_) | Selector::At { .. } => {
                vec![self.session(self.locate(selector)?, selector)?]
            }
            Selector::Title(text) => {
                let text = text.to_lowercase();
                self.live().filter(|info| title(&info.meta).is_some_and(|t| t.to_lowercase().contains(&text))).collect()
            }
            Selector::Agent { kind, state } => self
                .live()
                .filter(|info| {
                    info.meta.agent.is_some_and(|agent| {
                        agent.kind.label() == kind && state.is_none_or(|state| agent.state == state)
                    })
                })
                .collect(),
            Selector::Cwd(dir) => {
                let wanted = Wanted::new(dir, self.home.as_deref());
                self.live().filter(|info| info.meta.cwd.as_deref().is_some_and(|cwd| wanted.matches(cwd))).collect()
            }
        };
        match candidates.as_slice() {
            [one] => Ok(one),
            [] if matches!(selector, Selector::Id(_)) => bail!("no session {selector}; see `runode list`"),
            [] => bail!("no session matches {selector}; see `runode list`"),
            many => {
                let mut message = format!("{selector} matches more than one session:");
                for info in many {
                    message.push_str("\n  ");
                    message.push_str(&self.describe(info));
                }
                message.push_str("\ngive one of these ids instead");
                bail!(message)
            }
        }
    }

    /// 还在跑的会话：按标题、agent、目录找时不算已经退出的。
    fn live(&self) -> impl Iterator<Item = &SessionInfo> {
        self.sessions.iter().filter(|info| !info.exited)
    }

    fn session(&self, id: SessionId, selector: &Selector) -> Result<&SessionInfo> {
        self.sessions.iter().find(|info| info.id == id).ok_or_else(|| anyhow!("no session at {selector}"))
    }

    fn own_id(&self, selector: &Selector) -> Result<SessionId> {
        self.own.ok_or_else(|| anyhow!("{selector} only works inside a runode terminal"))
    }

    /// 候选的一行：短标识、标题、目录、位置。
    fn describe(&self, info: &SessionInfo) -> String {
        let mut line = info.id.to_string()[..crate::commands::SHORT_ID].to_owned();
        for part in [
            title(&info.meta),
            info.meta.cwd.as_deref().map(|cwd| tilde(cwd, self.home.as_deref())),
            self.place(info.id).map(|place| place_name(&place)),
        ]
        .into_iter()
        .flatten()
        {
            line.push_str("  ");
            line.push_str(&part);
        }
        line
    }

    /// 会话在布局里的位置；不在任何窗口里（后台会话）或者没有布局时为 `None`。
    pub(crate) fn place(&self, id: SessionId) -> Option<Place<'_>> {
        self.places().find(|place| place.pane.id == id)
    }

    pub(crate) fn places(&self) -> impl Iterator<Item = Place<'_>> {
        self.layout.iter().flatten().flat_map(|window| {
            window.workspaces.iter().flat_map(move |workspace| {
                workspace
                    .tabs
                    .iter()
                    .flat_map(move |tab| tab.panes.iter().map(move |pane| Place { window, workspace, tab, pane }))
            })
        })
    }

    fn windows(&self, selector: &Selector) -> Result<&[WindowLayout]> {
        match self.layout.as_deref() {
            Some(windows) if !windows.is_empty() => Ok(windows),
            _ => bail!("{selector} needs a runode window, and the app has none open"),
        }
    }

    /// 自己的位置，按位置找时要用。
    fn own_place(&self, selector: &Selector) -> Result<Place<'_>> {
        let own = self.own_id(selector)?;
        self.place(own).ok_or_else(|| anyhow!("{selector} needs your terminal to be in a runode window"))
    }

    /// 按位置的写法找到的分屏里的会话。
    fn locate(&self, selector: &Selector) -> Result<SessionId> {
        let windows = self.windows(selector)?;
        match *selector {
            Selector::Beside(direction) => {
                let own = self.own_place(selector)?;
                let others = own.tab.panes.iter().filter(|pane| pane.id != own.pane.id);
                neighbor(rect(own.pane.rect), direction, others.map(|pane| (pane.id, rect(pane.rect))))
                    .ok_or_else(|| anyhow!("there is no pane {} of yours", beside_name(direction)))
            }
            Selector::Cycle(forward) => {
                let own = self.own_place(selector)?;
                let mut panes: Vec<&PaneLayout> = own.tab.panes.iter().collect();
                panes.sort_by_key(|pane| pane.index);
                if panes.len() < 2 {
                    bail!("your tab has no other pane");
                }
                let at = panes.iter().position(|pane| pane.id == own.pane.id).unwrap_or(0);
                let next = if forward { (at + 1) % panes.len() } else { (at + panes.len() - 1) % panes.len() };
                Ok(panes[next].id)
            }
            Selector::At { window, workspace, spot } => {
                // 没给窗口、工作区时从自己所在的地方找，不在 runode 里时从最前面的窗口找。
                let own = self.own.and_then(|own| self.place(own));
                let win = match window {
                    Some(index) => windows
                        .iter()
                        .find(|w| w.index == index)
                        .ok_or_else(|| anyhow!("there is no window {index}"))?,
                    None => {
                        own.map(|own| own.window).or_else(|| windows.iter().find(|w| w.front)).unwrap_or(&windows[0])
                    }
                };
                let here = own.filter(|own| window.is_none() && own.window.index == win.index);
                let ws = match workspace {
                    Some(index) => win
                        .workspaces
                        .iter()
                        .find(|ws| ws.index == index)
                        .ok_or_else(|| anyhow!("window {} has no workspace {index}", win.index))?,
                    None => here
                        .map(|own| own.workspace)
                        .or_else(|| win.workspaces.iter().find(|ws| ws.active))
                        .or_else(|| win.workspaces.first())
                        .ok_or_else(|| anyhow!("window {} has no workspace", win.index))?,
                };
                let here = here.filter(|own| workspace.is_none() && own.workspace.index == ws.index);
                let current_tab = || {
                    here.map(|own| own.tab)
                        .or_else(|| ws.tabs.iter().find(|tab| tab.active))
                        .or_else(|| ws.tabs.first())
                        .ok_or_else(|| anyhow!("workspace {} has no tab", ws.index))
                };
                let focused = |tab: &TabLayout| {
                    tab.panes
                        .iter()
                        .find(|pane| pane.focused)
                        .or_else(|| tab.panes.iter().min_by_key(|pane| pane.index))
                        .map(|pane| pane.id)
                        .ok_or_else(|| anyhow!("tab {} has no pane", tab.index))
                };
                let pane_of = |tab: &TabLayout, index: u32| {
                    tab.panes
                        .iter()
                        .find(|pane| pane.index == index)
                        .map(|pane| pane.id)
                        .ok_or_else(|| anyhow!("tab {} has no pane {index}", tab.index))
                };
                match spot {
                    Spot::Focused => focused(current_tab()?),
                    Spot::Pane(index) => pane_of(current_tab()?, index),
                    Spot::Tab(index, pane) => {
                        let tab = ws
                            .tabs
                            .iter()
                            .find(|tab| tab.index == index)
                            .ok_or_else(|| anyhow!("workspace {} has no tab {index}", ws.index))?;
                        match pane {
                            Some(pane) => pane_of(tab, pane),
                            None => focused(tab),
                        }
                    }
                }
            }
            _ => unreachable!("only positional selectors need the layout"),
        }
    }

    /// `own` 旁边各个方向上的分屏是哪个会话，列会话的 REL 一列用；自己不在窗口里时为空。
    pub(crate) fn relations(&self) -> Vec<(SessionId, &'static str)> {
        let Some(own) = self.own.and_then(|own| self.place(own)) else {
            return Vec::new();
        };
        let mut relations = vec![(own.pane.id, "self")];
        let others = || own.tab.panes.iter().filter(|pane| pane.id != own.pane.id);
        for direction in [Direction::Left, Direction::Right, Direction::Up, Direction::Down] {
            let candidates = others().map(|pane| (pane.id, rect(pane.rect)));
            if let Some(id) = neighbor(rect(own.pane.rect), direction, candidates) {
                relations.push((id, direction_name(direction)));
            }
        }
        relations
    }
}

/// `place` 写成按序号找它的写法。
pub(crate) fn place_name(place: &Place<'_>) -> String {
    format!("win:{}/ws:{}/tab:{}.{}", place.window.index, place.workspace.index, place.tab.index, place.pane.index)
}

fn beside_name(direction: Direction) -> &'static str {
    match direction {
        Direction::Left => "to the left",
        Direction::Right => "to the right",
        Direction::Up => "above",
        Direction::Down => "below",
    }
}

fn rect(rect: PaneRect) -> Rect {
    Rect { x: rect.x.into(), y: rect.y.into(), width: rect.width.into(), height: rect.height.into() }
}

/// `Selector::Cwd` 要的目录。
enum Wanted {
    Path(PathBuf),
    Name(String),
}

impl Wanted {
    fn new(dir: &str, home: Option<&Path>) -> Self {
        if !(dir.contains('/') || dir.starts_with('~') || dir.starts_with('.')) {
            return Self::Name(dir.into());
        }
        let path = match (dir, dir.strip_prefix("~/"), home) {
            ("~", _, Some(home)) => home.to_owned(),
            (_, Some(rest), Some(home)) => home.join(rest),
            _ => PathBuf::from(dir),
        };
        let path = if path.is_absolute() {
            path
        } else {
            std::env::current_dir().map(|here| here.join(&path)).unwrap_or(path)
        };
        Self::Path(normalize(&path))
    }

    fn matches(&self, cwd: &Path) -> bool {
        match self {
            Self::Path(path) => normalize(cwd) == *path,
            Self::Name(name) => cwd.file_name().is_some_and(|last| last == name.as_str()),
        }
    }
}

/// 去掉路径里的 `.` 和 `..`、末尾的 `/`，不碰文件系统。
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_forms() {
        let parse = |s: &str| Selector::parse(s).unwrap();
        assert_eq!(parse("ABCD"), Selector::Id("abcd".into()));
        assert_eq!(parse("."), Selector::Own);
        assert_eq!(parse("down"), Selector::Beside(Direction::Down));
        assert_eq!(parse("prev"), Selector::Cycle(false));
        assert_eq!(parse("pane:2"), Selector::At { window: None, workspace: None, spot: Spot::Pane(2) });
        assert_eq!(parse("tab:3.1"), Selector::At { window: None, workspace: None, spot: Spot::Tab(3, Some(1)) });
        assert_eq!(parse("win:2"), Selector::At { window: Some(2), workspace: None, spot: Spot::Focused });
        assert_eq!(
            parse("win:2/ws:1/tab:4"),
            Selector::At { window: Some(2), workspace: Some(1), spot: Spot::Tab(4, None) }
        );
        assert_eq!(parse("title:Build: a/b"), Selector::Title("Build: a/b".into()));
        assert_eq!(
            parse("agent:Claude:idle"),
            Selector::Agent { kind: "claude".into(), state: Some(AgentState::Idle) }
        );
        assert_eq!(parse("cwd:~/src"), Selector::Cwd("~/src".into()));
        for written in ["self", "left", "next", "pane:2", "tab:3.1", "win:2/ws:1/tab:4", "agent:codex:working"] {
            assert_eq!(parse(written).to_string(), written);
        }
        for bad in
            ["", "sideways", "tab:", "tab:0", "tab:x", "tab:1.x", "pane:1/tab:2", "ws:1/win:2", "foo:1", "agent:"]
        {
            assert!(Selector::parse(bad).is_err(), "{bad}");
        }
        assert!(Selector::parse("agent:claude:asleep").is_err());
    }

    #[test]
    fn directories_match_by_path_or_name() {
        let name = Wanted::new("runode", None);
        assert!(name.matches(Path::new("/src/runode")));
        assert!(!name.matches(Path::new("/src/runode/crates")));
        let path = Wanted::new("/src/./runode/", None);
        assert!(path.matches(Path::new("/src/runode")));
        assert!(!path.matches(Path::new("/other/runode")));
        let home = Path::new("/Users/me");
        assert!(Wanted::new("~/src", Some(home)).matches(Path::new("/Users/me/src")));
        assert!(Wanted::new("~", Some(home)).matches(home));
    }
}
