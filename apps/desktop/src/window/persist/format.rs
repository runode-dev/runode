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

use runode_protocol::{Conversation, SessionId, SessionInfo, WindowLayout};
use runode_types::pane::Axis;
use serde::{Deserialize, Serialize};

/// 格式改得不兼容时加一；读到别的版本时存档挪到一边（`load`），从默认布局开始。
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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedWindow {
    pub bounds: SavedBounds,
    pub workspaces: Vec<SavedWorkspace>,
    /// 当前的 workspace。
    pub active: usize,
    /// 用户手动收起（`false`）或展开（`true`）过侧栏；没动过时为空，按 workspace 数决定。
    pub sidebar: Option<bool>,
    /// 用户拖动过侧栏宽度时是那个宽度；没拖过时为空，用默认宽度。
    pub sidebar_width: Option<f32>,
    /// 右侧面板拖动过的宽度；没拖过时为空，用默认宽度。
    pub panel_width: Option<f32>,
    /// 预览的文本和 diff 自动换行；默认不换。
    pub preview_wrap: bool,
    /// Markdown 文件显示源码；默认显示排版后的样子。
    pub preview_source: bool,
    /// 文件树里显示被 git 忽略的文件。
    pub show_ignored: bool,
    /// 文件树里不显示名字以 `.` 开头的文件；默认显示。
    pub hide_dotfiles: bool,
    /// Git 面板里改动的文件以树形式查看；默认是列表。
    pub git_tree: bool,
    /// Git 面板里不显示子模块和嵌套仓库、不显示其他工作树；默认都显示。
    pub git_hide_subs: bool,
    pub git_hide_worktrees: bool,
    /// 文件树的搜索结果以树形式查看；默认是列表。
    pub file_search_tree: bool,
    /// 文件树按内容搜索时的开关和要包含、要排除的文件。
    pub file_search_options: SavedSearchOptions,
    /// Git 面板底部的图表收起来了，以及拖动过的高度；没拖过时为空，用默认高度。
    pub git_graph_collapsed: bool,
    pub git_graph_height: Option<f32>,
    /// GitHub Actions 页三段拖过分隔线后的高度（`ActionsPage::heights`），没拖过的为空。
    pub github_actions_heights: [Option<f32>; 3],
}

/// 文件树按内容搜索时的选项，定义跟着文件树。
pub use crate::file_tree::SearchOptions as SavedSearchOptions;

/// 窗口的位置和大小，相对于它所在的屏幕；放大和全屏时是还原后的位置和大小。默认的大小是 0，恢复时
/// 按默认位置开。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedBounds {
    /// 屏幕的 UUID，重启后不变；取不到时为空，恢复到主屏幕上。
    pub display: Option<String>,
    pub mode: WindowMode,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowMode {
    #[default]
    Windowed,
    Maximized,
    Fullscreen,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedWorkspace {
    pub name: String,
    pub dir: PathBuf,
    pub tabs: Vec<SavedTab>,
    /// 当前的标签。
    pub active: usize,
    /// 标题栏命令菜单里上次跑的命令行（`Project::tasks_last`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_task: Option<String>,
    /// 这个 workspace 的右侧面板显示的是 Git 面板还是文件树，至多一个为真；收着时都为假。
    #[serde(default, skip_serializing_if = "is_false")]
    pub git: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub files: bool,
    /// GitHub Actions 页里固定到状态栏上的工作流（`Workspace::pinned_workflows`）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pinned_workflows: Vec<PinnedWorkflow>,
}

/// 固定到状态栏上的一个工作流，定义跟着 GitHub Actions 页。
pub use crate::github_actions::PinnedWorkflow;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedTab {
    pub root: SavedNode,
    /// 获得焦点的终端在叶子顺序（从左到右、从上到下）里的位置。
    pub focused: usize,
    pub zoomed: bool,
    /// 折叠成只剩标题条的终端在叶子顺序里的位置。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collapsed: Vec<usize>,
}

/// 分屏树，结构和 `pane::Node` 一样，叶子记终端所在的目录和宿主里的会话。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SavedNode {
    Leaf {
        /// 取不到时为空，恢复时从 workspace 的目录开始。
        cwd: Option<PathBuf>,
        /// 终端在宿主里的会话。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<SessionId>,
        /// 写存档时终端的 shell 还没启动过。shell 启动时布局不变、不一定重写存档，所以这一项可能
        /// 已经过时，恢复时以宿主为准（见 `plan_restore`）。
        #[serde(default, skip_serializing_if = "is_false")]
        unstarted: bool,
        /// 前台 agent 在进行的对话：会话接不上、要在原目录新开时接着它。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conversation: Option<Conversation>,
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
    /// 启动时要结束的会话，免得一直留在宿主里、挡着宿主空闲退出：存档里记着、没人连着的会话中
    /// shell 已经退出的和上次一直没启动的。
    pub end: Vec<SessionId>,
}

/// 按宿主里还活着的会话 `live` 定下存档里每个终端怎么恢复。叶子记的会话要接上，得同时满足：
/// 还在宿主里、shell 没退出、启动过、没有别的界面连着（`SessionInfo::claimed`），而且是存档里
/// 第一次出现（按窗口、workspace、标签和叶子的先后；手改出重复的，后面的在原目录新开）。
/// 不满足的清掉 `session`，在原目录新开；其中还在宿主里、没人连着、又已经退出或者没启动过的，
/// 放进 `end`。没启动过要存档和宿主都这么说：存档记着 `unstarted`、宿主也读不到
/// 前台程序（`SessionMeta::foreground`，shell 启动后就是 shell 自己）；存档的记录过时了、宿主
/// 那边其实已经启动的照样接上。存档里没记着的会话不在这里结束：看板留着已经退出的会话给人看
/// agent 最后的输出，这类会话由 `watch_background` 问过看板再收。
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
    RestorePlan { windows, end }
}

/// 把没开 app 时经宿主办的事接进要恢复的存档：宿主手里的布局 `held`（见 `ClientMsg::LayoutChanged`）
/// 里有、存档里没有的工作区，没人连着的活会话，以及改过的工作区名字。两边按窗口的序号对，窗口里目录
/// 一样的工作区是同一个，名字跟着宿主改，新会话各开一个标签加在后面；存档里没有的目录在对应序号的窗口
/// 末尾新建，没有名字时用 `name_of` 按目录取。宿主不知道目录的工作区不接，里面的会话成为后台会话。
pub fn adopt(
    windows: &mut Vec<SavedWindow>,
    held: &[WindowLayout],
    live: &[SessionInfo],
    name_of: impl Fn(&Path) -> String,
) {
    let mut known: HashSet<SessionId> = windows
        .iter_mut()
        .flat_map(|window| &mut window.workspaces)
        .flat_map(|workspace| &mut workspace.tabs)
        .flat_map(|tab| tab.root.leaves_mut())
        .filter_map(|leaf| match leaf {
            SavedNode::Leaf { session, .. } => *session,
            SavedNode::Split { .. } => None,
        })
        .collect();
    for (wi, held_window) in held.iter().enumerate() {
        for (si, held_workspace) in held_window.workspaces.iter().enumerate() {
            let Some(dir) = &held_workspace.dir else {
                continue;
            };
            let fresh: Vec<SavedTab> = held_workspace
                .tabs
                .iter()
                .flat_map(|tab| &tab.panes)
                .filter_map(|pane| live.iter().find(|info| info.id == pane.id && !info.exited && !info.claimed))
                .filter(|info| known.insert(info.id))
                .map(|info| SavedTab {
                    root: SavedNode::Leaf {
                        cwd: info.meta.cwd.clone(),
                        session: Some(info.id),
                        unstarted: false,
                        conversation: info.meta.conversation.clone(),
                    },
                    focused: 0,
                    zoomed: false,
                    collapsed: Vec::new(),
                })
                .collect();
            let saved = windows.get_mut(wi).and_then(|window| {
                let workspaces = &mut window.workspaces;
                let ix = Some(si)
                    .filter(|&si| workspaces.get(si).is_some_and(|workspace| workspace.dir == *dir))
                    .or_else(|| workspaces.iter().position(|workspace| workspace.dir == *dir))?;
                workspaces.get_mut(ix)
            });
            if let Some(workspace) = saved {
                if let Some(name) = &held_workspace.name {
                    workspace.name.clone_from(name);
                }
                workspace.tabs.extend(fresh);
                continue;
            }
            if windows.is_empty() {
                windows.push(SavedWindow::default());
            }
            let window = wi.min(windows.len() - 1);
            let name = held_workspace.name.clone().unwrap_or_else(|| name_of(dir));
            let workspace = SavedWorkspace { name, dir: dir.clone(), tabs: fresh, ..SavedWorkspace::default() };
            windows[window].workspaces.push(workspace);
        }
    }
}

/// 新开的终端从哪里开始：记下的目录，没有时是 workspace 的目录。不看它还在不在：目录在挂死的网络卷上时
/// stat 会卡住界面线程；不在了由宿主开会话时退回 workspace 的目录（`ClientMsg::Spawn::fallback_cwd`）。
pub fn start_dir(cwd: Option<&Path>, workspace_dir: &Path) -> PathBuf {
    cwd.unwrap_or(workspace_dir).to_path_buf()
}

/// 读存档，没有存档时为 `None`。读不了、内容坏了或者是别的版本时也为 `None`，并把原文件挪到一边
/// （`set_aside`）：之后布局一变就要写存档，不挪开就会被默认布局盖掉。
pub fn load() -> Option<State> {
    load_at(&path()?)
}

fn load_at(path: &Path) -> Option<State> {
    match read(path) {
        Ok(state) => state,
        Err(err) => {
            tracing::warn!("failed to read the saved window layout, setting it aside and starting fresh: {err}");
            set_aside(path);
            None
        }
    }
}

/// 读 `path` 上的存档：不存在时为 `Ok(None)`，读不了、内容坏了或者是别的版本时报错。
fn read(path: &Path) -> io::Result<Option<State>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    // 先只看版本：别的版本的格式可能对不上，按格式解会报得莫名其妙。
    #[derive(Deserialize)]
    struct Version {
        version: u32,
    }
    let version = serde_json::from_str::<Version>(&text)?.version;
    if version != VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("saved in format {version}, this build reads format {VERSION}"),
        ));
    }
    Ok(Some(serde_json::from_str(&text)?))
}

/// 写存档，见 `write_at`。
pub fn write(state: &State) -> io::Result<()> {
    let path = path().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))?;
    write_at(&path, state)
}

/// 把存档写到 `path`，经 `runode_paths::replace_file` 整个换上：写到一半退出、断电或者磁盘满时
/// 不会留下半个文件，写出错时也不留下临时文件。
fn write_at(path: &Path, state: &State) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    runode_paths::replace_file(path, &serde_json::to_vec_pretty(state)?)
}

/// 读不了的存档挪到一边，之后写的时候不覆盖它，留着排查或者换回能读它的版本。
fn set_aside(path: &Path) {
    let bad = path.with_extension("json.bad");
    if let Err(err) = fs::rename(path, &bad) {
        tracing::warn!("failed to move the saved window layout aside to {}: {err}", bad.display());
    }
}

fn path() -> Option<PathBuf> {
    runode_paths::Dirs::from_env().windows_file()
}

#[cfg(test)]
mod tests;
