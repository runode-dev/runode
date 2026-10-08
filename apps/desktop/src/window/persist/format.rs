//! 窗口布局的存档：开着哪些窗口，各自的 workspace、标签和分屏，以及每个终端所在的目录和
//! 宿主里的会话。布局一变就写，下次启动时读回来：会话还在宿主里（宿主单独一个进程跑时）就
//! 接上它，不在了就在原来的目录里重新开 shell，见 `plan_restore`。
//!
//! 这里只管存档的格式、读写文件和恢复时哪些会话接得上，和界面之间的转换由 `WindowView` 负责。

use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
};

use runode_protocol::{SessionId, SessionInfo};
use runode_shared_types::pane::Axis;
use serde::{Deserialize, Serialize};

/// 格式改得不兼容时加一；读到别的版本当作没有存档。
const VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    /// 按打开的先后。
    pub windows: Vec<SavedWindow>,
}

impl State {
    pub fn new(windows: Vec<SavedWindow>) -> Self {
        Self { version: VERSION, windows }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedWindow {
    pub bounds: SavedBounds,
    pub workspaces: Vec<SavedWorkspace>,
    /// 当前的 workspace。
    pub active: usize,
    /// 用户手动收起（`false`）或展开（`true`）过侧栏；没动过时为空，按 workspace 数决定。
    #[serde(default)]
    pub sidebar: Option<bool>,
    /// 用户拖动过侧栏宽度时是那个宽度；没拖过时为空，用默认宽度。
    #[serde(default)]
    pub sidebar_width: Option<f32>,
    /// 右侧面板显示的是 Git 面板还是文件树，至多一个为真；收着时都为假。
    #[serde(default)]
    pub git: bool,
    #[serde(default)]
    pub files: bool,
    /// 右侧面板拖动过的宽度；没拖过时为空，用默认宽度。文件树和 Git 面板还是两栏时存的是
    /// 文件树的宽度。
    #[serde(default, alias = "files_width")]
    pub panel_width: Option<f32>,
    /// 预览栏拖动过的宽度；预览的是哪个文件、开没开着都不存。
    #[serde(default)]
    pub preview_width: Option<f32>,
    /// 文件树里显示被 git 忽略的文件。
    #[serde(default)]
    pub show_ignored: bool,
    /// Git 面板里改动的文件以树形式查看；默认是列表。
    #[serde(default)]
    pub git_tree: bool,
    /// 文件树的搜索结果以树形式查看；默认是列表。
    #[serde(default)]
    pub file_search_tree: bool,
    /// Git 面板底部的图表收起来了，以及拖动过的高度；没拖过时为空，用默认高度。
    #[serde(default)]
    pub git_graph_collapsed: bool,
    #[serde(default)]
    pub git_graph_height: Option<f32>,
}

/// 窗口的位置和大小，相对于它所在的屏幕；放大和全屏时是还原后的位置和大小。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedBounds {
    /// 屏幕的 UUID，重启后不变；取不到时为空，恢复到主屏幕上。
    #[serde(default)]
    pub display: Option<String>,
    pub mode: WindowMode,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowMode {
    Windowed,
    Maximized,
    Fullscreen,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedWorkspace {
    pub name: String,
    pub dir: PathBuf,
    pub tabs: Vec<SavedTab>,
    /// 当前的标签。
    pub active: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedTab {
    pub root: SavedNode,
    /// 获得焦点的终端在叶子顺序（从左到右、从上到下）里的位置。
    pub focused: usize,
    #[serde(default)]
    pub zoomed: bool,
}

/// 分屏树，结构和 `pane::Node` 一样，叶子记终端所在的目录和宿主里的会话。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SavedNode {
    Leaf {
        /// 取不到时为空，恢复时从 workspace 的目录开始。
        cwd: Option<PathBuf>,
        /// 终端在宿主里的会话；记这一项以前的存档里没有。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<SessionId>,
        /// 写存档时终端的 shell 还没启动过。宿主里有会话的（接上的会话一直没切过去）接上也是空的，
        /// 下次启动时宿主那边看着也没启动的话结束它，在原目录另开；从没在宿主里开过会话的
        /// （`TerminalView::deferred` 建的、一直没显示过）`session` 为空，下次启动直接在原目录
        /// 新开。shell 启动时布局不变、
        /// 不一定重写存档，所以这一项可能已经过时，恢复时以宿主为准（见 `plan_restore`）。
        #[serde(default, skip_serializing_if = "is_false")]
        unstarted: bool,
    },
    Split {
        axis: Axis,
        ratio: f32,
        first: Box<SavedNode>,
        second: Box<SavedNode>,
    },
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl SavedNode {
    /// 叶子，从左到右、从上到下。
    fn leaves_mut(&mut self) -> Vec<&mut SavedNode> {
        match self {
            SavedNode::Leaf { .. } => vec![self],
            SavedNode::Split { first, second, .. } => {
                let mut leaves = first.leaves_mut();
                leaves.extend(second.leaves_mut());
                leaves
            }
        }
    }
}

/// 恢复时会话怎么办，见 `plan_restore`。
#[derive(Clone, Debug, PartialEq)]
pub struct RestorePlan {
    /// 要恢复的窗口：叶子的 `session` 只留下这次接得上的，其余清空（在原目录新开）。
    pub windows: Vec<SavedWindow>,
    /// 启动时要结束的会话，免得一直留在宿主里、挡着宿主空闲退出：宿主里 shell 已经退出、没人连
    /// 着的会话（不管存档里记没记），以及存档里记着、上次一直没启动的会话。
    pub end: Vec<SessionId>,
}

/// 按宿主里还活着的会话 `live` 定下存档里每个终端怎么恢复。叶子记的会话要接上，得同时满足：
/// 还在宿主里、shell 没退出、启动过、没有别的界面连着（`SessionInfo::claimed`），而且是存档里
/// 第一次出现（按窗口、workspace、标签和叶子的先后；手改或者旧版本写出的重复的，后面的在原
/// 目录新开）。不满足的清掉 `session`，在原目录新开；其中还在宿主里、没人连着、又已经退出或者
/// 没启动过的，放进 `end`。没启动过要存档和宿主都这么说：存档记着 `unstarted`、宿主也读不到
/// 前台程序（`SessionMeta::foreground`，shell 启动后就是 shell 自己）；存档的记录过时了、宿主
/// 那边其实已经启动的照样接上。存档里没记着的会话，shell 已经退出、又没人连着的也放进 `end`。
/// 宿主跑在 app 里时它是新的，`live` 为空，全部新开、什么都不结束。
pub fn plan_restore(mut windows: Vec<SavedWindow>, live: &[SessionInfo]) -> RestorePlan {
    let mut seen = HashSet::new();
    let mut end = Vec::new();
    let leaves = windows
        .iter_mut()
        .flat_map(|window| &mut window.workspaces)
        .flat_map(|workspace| &mut workspace.tabs)
        .flat_map(|tab| tab.root.leaves_mut());
    for leaf in leaves {
        let SavedNode::Leaf { session, unstarted, .. } = leaf else {
            continue;
        };
        let Some(id) = session.take() else {
            continue;
        };
        let first = seen.insert(id);
        let Some(info) = live.iter().find(|info| info.id == id) else {
            continue;
        };
        if !first || info.claimed {
            continue;
        }
        let never_started = *unstarted && info.meta.foreground.is_none();
        if info.exited || never_started {
            end.push(id);
        } else {
            *session = Some(id);
        }
    }
    let orphans = live.iter().filter(|info| info.exited && !info.claimed).map(|info| info.id);
    for id in orphans {
        if !end.contains(&id) {
            end.push(id);
        }
    }
    RestorePlan { windows, end }
}

/// 恢复的终端从哪里开始：记下的目录还在就用它，否则用 workspace 的目录；都不在了为空，
/// 也就是家目录。
pub fn start_dir(cwd: Option<&Path>, workspace_dir: &Path) -> Option<PathBuf> {
    cwd.filter(|cwd| cwd.is_dir()).or_else(|| Some(workspace_dir).filter(|dir| dir.is_dir())).map(Path::to_path_buf)
}

/// 读存档。没有存档或者是别的版本时为 `Ok(None)`，读不了或内容坏了时报错。
pub fn load() -> io::Result<Option<State>> {
    let Some(path) = path() else {
        return Ok(None);
    };
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    // 先只看版本：别的版本的格式可能对不上，不该当成坏文件。
    #[derive(Deserialize)]
    struct Version {
        version: u32,
    }
    if serde_json::from_str::<Version>(&text)?.version != VERSION {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&text)?))
}

/// 写存档：先写到旁边的临时文件再改名，写到一半退出也不会留下半个文件。
pub fn write(state: &State) -> io::Result<()> {
    let path = path().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(state)?)?;
    fs::rename(&tmp, &path)
}

/// 内容坏了的存档挪到一边，下次写的时候不覆盖它，留着排查。
pub fn set_aside() {
    if let Some(path) = path() {
        let _ = fs::rename(&path, path.with_extension("json.bad"));
    }
}

fn path() -> Option<PathBuf> {
    runode_paths::Dirs::from_env().windows_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(cwd: &str) -> SavedNode {
        SavedNode::Leaf { cwd: Some(cwd.into()), session: None, unstarted: false }
    }

    fn session_leaf(id: u128, unstarted: bool) -> SavedNode {
        SavedNode::Leaf { cwd: Some("/tmp".into()), session: Some(SessionId(id)), unstarted }
    }

    fn state() -> State {
        let tab = SavedTab {
            root: SavedNode::Split {
                axis: Axis::Horizontal,
                ratio: 0.3,
                first: Box::new(leaf("/tmp")),
                second: Box::new(SavedNode::Split {
                    axis: Axis::Vertical,
                    ratio: 0.5,
                    first: Box::new(SavedNode::Leaf { cwd: None, session: Some(SessionId(7)), unstarted: true }),
                    second: Box::new(leaf("/")),
                }),
            },
            focused: 2,
            zoomed: false,
        };
        State::new(vec![SavedWindow {
            bounds: SavedBounds {
                display: None,
                mode: WindowMode::Windowed,
                x: 10.,
                y: 20.,
                width: 960.,
                height: 620.,
            },
            workspaces: vec![SavedWorkspace { name: "tmp".into(), dir: "/tmp".into(), tabs: vec![tab], active: 0 }],
            active: 0,
            sidebar: None,
            sidebar_width: Some(240.),
            git: true,
            files: false,
            panel_width: Some(200.),
            preview_width: Some(420.),
            show_ignored: true,
            git_tree: true,
            file_search_tree: true,
            git_graph_collapsed: true,
            git_graph_height: Some(260.),
        }])
    }

    #[test]
    fn reads_windows_saved_before_the_tree_view_settings() {
        let text = serde_json::to_string(&state()).unwrap();
        let old = text.replace(r#","git_tree":true"#, "").replace(r#","file_search_tree":true"#, "");
        assert_ne!(old, text);
        let state: State = serde_json::from_str(&old).unwrap();
        assert!(!state.windows[0].git_tree);
        assert!(!state.windows[0].file_search_tree);
    }

    #[test]
    fn reads_windows_saved_with_separate_git_and_files_widths() {
        let text = serde_json::to_string(&state()).unwrap();
        let old = text.replace(r#""panel_width":200.0"#, r#""git_width":320.0,"files_width":200.0"#);
        assert_ne!(old, text);
        // 以前两栏各存一个宽度，右侧面板沿用文件树的。
        let state: State = serde_json::from_str(&old).unwrap();
        assert_eq!(state.windows[0].panel_width, Some(200.));
    }

    #[test]
    fn reads_windows_saved_before_the_graph_pane() {
        let text = serde_json::to_string(&state()).unwrap();
        let old = text.replace(r#","git_graph_collapsed":true,"git_graph_height":260.0"#, "");
        assert_ne!(old, text);
        // 以前存的窗口图表展开着，用默认高度。
        let state: State = serde_json::from_str(&old).unwrap();
        assert!(!state.windows[0].git_graph_collapsed);
        assert_eq!(state.windows[0].git_graph_height, None);
    }

    #[test]
    fn round_trips_through_json() {
        let state = state();
        let text = serde_json::to_string(&state).unwrap();
        assert_eq!(serde_json::from_str::<State>(&text).unwrap(), state);
        assert!(text.contains(r#""type":"split","axis":"horizontal""#), "{text}");
    }

    #[test]
    fn leaves_keep_their_sessions_through_json() {
        let state = state();
        let text = serde_json::to_string(&state).unwrap();
        assert!(text.contains(r#""session":"00000000000000000000000000000007","unstarted":true"#), "{text}");
        // 没有会话、启动过的叶子不写这两项。
        assert!(text.contains(r#"{"type":"leaf","cwd":"/tmp"}"#), "{text}");
        assert_eq!(serde_json::from_str::<State>(&text).unwrap(), state);
    }

    #[test]
    fn reads_leaves_saved_before_sessions_were_recorded() {
        let old = r#"{"version":1,"windows":[{"bounds":{"mode":"windowed","x":0,"y":0,"width":960,"height":620},
            "workspaces":[{"name":"tmp","dir":"/tmp","active":0,
            "tabs":[{"focused":0,"root":{"type":"leaf","cwd":"/tmp"}}]}],"active":0}]}"#;
        let state: State = serde_json::from_str(old).unwrap();
        assert_eq!(
            state.windows[0].workspaces[0].tabs[0].root,
            SavedNode::Leaf { cwd: Some("/tmp".into()), session: None, unstarted: false }
        );
    }

    fn info(id: u128, claimed: bool, exited: bool) -> SessionInfo {
        SessionInfo {
            id: SessionId(id),
            size: runode_shared_types::grid::GridSize { cols: 80, rows: 24, cell_width_px: 8, cell_height_px: 16 },
            meta: Default::default(),
            clients: 0,
            claimed,
            exited,
            size_owner: None,
        }
    }

    fn window(tabs: Vec<SavedNode>) -> SavedWindow {
        let mut window = state().windows.remove(0);
        window.workspaces[0].tabs = tabs.into_iter().map(|root| SavedTab { root, focused: 0, zoomed: false }).collect();
        window
    }

    fn sessions(windows: &[SavedWindow]) -> Vec<Option<u128>> {
        let mut windows = windows.to_vec();
        windows
            .iter_mut()
            .flat_map(|window| &mut window.workspaces)
            .flat_map(|workspace| &mut workspace.tabs)
            .flat_map(|tab| tab.root.leaves_mut())
            .map(|leaf| match leaf {
                SavedNode::Leaf { session, .. } => session.map(|id| id.0),
                SavedNode::Split { .. } => unreachable!(),
            })
            .collect()
    }

    #[test]
    fn live_sessions_are_reattached_and_the_rest_start_over() {
        let split = SavedNode::Split {
            axis: Axis::Vertical,
            ratio: 0.5,
            first: Box::new(session_leaf(1, false)),
            second: Box::new(session_leaf(2, false)),
        };
        let windows =
            vec![window(vec![split, session_leaf(3, false), leaf("/")]), window(vec![session_leaf(4, false)])];
        // 2 已经不在宿主里了；4 有别的界面连着。
        let live = [info(1, false, false), info(3, false, false), info(4, true, false), info(9, false, false)];
        let plan = plan_restore(windows, &live);
        assert_eq!(sessions(&plan.windows), [Some(1), None, Some(3), None, None]);
        assert!(plan.end.is_empty());
    }

    #[test]
    fn a_session_saved_twice_is_reattached_only_the_first_time() {
        let windows =
            vec![window(vec![session_leaf(1, false), session_leaf(1, false)]), window(vec![session_leaf(1, false)])];
        let plan = plan_restore(windows, &[info(1, false, false)]);
        assert_eq!(sessions(&plan.windows), [Some(1), None, None]);
        assert!(plan.end.is_empty());
    }

    #[test]
    fn exited_and_unstarted_sessions_start_over_and_are_ended() {
        let windows = vec![window(vec![
            session_leaf(1, false),
            session_leaf(2, true),
            session_leaf(2, true),
            session_leaf(3, true),
            session_leaf(4, false),
        ])];
        // 1 的 shell 退出了；2 上次没启动过；3 没启动过但别的界面连着，不归这里结束；4 不在了。
        let live = [info(1, false, true), info(2, false, false), info(3, true, false)];
        let plan = plan_restore(windows, &live);
        assert_eq!(sessions(&plan.windows), [None; 5]);
        assert_eq!(plan.end, [SessionId(1), SessionId(2)]);
    }

    /// 宿主里 shell 已经退出、没人连着的会话，存档里没记着也结束；有界面连着的、还在跑的不动。
    #[test]
    fn exited_sessions_nobody_holds_are_ended_even_if_not_saved() {
        let windows = vec![window(vec![session_leaf(1, false), session_leaf(2, false)])];
        let live = [
            info(1, false, true),
            info(2, false, false),
            info(3, false, true),
            info(4, true, true),
            info(5, false, false),
            info(3, false, true),
        ];
        let plan = plan_restore(windows, &live);
        assert_eq!(sessions(&plan.windows), [None, Some(2)]);
        assert_eq!(plan.end, [SessionId(1), SessionId(3)]);
        // 没有存档要恢复时也照样结束。
        assert_eq!(plan_restore(Vec::new(), &live).end, [SessionId(1), SessionId(3)]);
    }

    /// 存档说没启动过，宿主那边却已经有前台程序（标签切过去启动了 shell，存档没来得及重写）：
    /// 以宿主为准接上，不结束它。
    #[test]
    fn a_stale_unstarted_mark_does_not_end_a_running_shell() {
        let windows = vec![window(vec![session_leaf(1, true)])];
        let mut running = info(1, false, false);
        running.meta.foreground = Some("zsh".into());
        let plan = plan_restore(windows, &[running]);
        assert_eq!(sessions(&plan.windows), [Some(1)]);
        assert!(plan.end.is_empty());
    }

    #[test]
    fn nothing_is_reattached_when_the_host_is_new() {
        let windows = vec![window(vec![session_leaf(1, false), session_leaf(2, true)])];
        let plan = plan_restore(windows, &[]);
        assert_eq!(sessions(&plan.windows), [None, None]);
        assert!(plan.end.is_empty());
    }

    #[test]
    fn start_dir_falls_back_to_workspace_then_home() {
        let missing = Path::new("/nonexistent/runode-test");
        assert_eq!(start_dir(Some(Path::new("/")), Path::new("/tmp")), Some("/".into()));
        assert_eq!(start_dir(Some(missing), Path::new("/tmp")), Some("/tmp".into()));
        assert_eq!(start_dir(None, missing), None);
    }
}
