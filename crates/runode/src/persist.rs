//! 窗口布局的存档：开着哪些窗口，各自的 workspace、标签和分屏，以及每个终端所在的目录。
//! 布局一变就写，下次启动时读回来，在原来的目录里重新开 shell；进程本身不保留。
//!
//! 这里只管存档的格式和读写文件，和界面之间的转换由 `WindowView` 负责。

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use runode_model::pane::Axis;
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
    /// 右侧的改动栏和文件树是否显示，以及拖动过的宽度；没拖过时为空，用默认宽度。
    #[serde(default)]
    pub changes: bool,
    #[serde(default)]
    pub changes_width: Option<f32>,
    #[serde(default)]
    pub files: bool,
    #[serde(default)]
    pub files_width: Option<f32>,
    /// 文件树里显示被 git 忽略的文件。
    #[serde(default)]
    pub show_ignored: bool,
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

/// 分屏树，结构和 `pane::Node` 一样，叶子记终端所在的目录。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SavedNode {
    Leaf {
        /// 取不到时为空，恢复时从 workspace 的目录开始。
        cwd: Option<PathBuf>,
    },
    Split {
        axis: Axis,
        ratio: f32,
        first: Box<SavedNode>,
        second: Box<SavedNode>,
    },
}

/// 恢复的终端从哪里开始：记下的目录还在就用它，否则用 workspace 的目录；都不在了为空，
/// 也就是家目录。
pub fn start_dir(cwd: Option<&Path>, workspace_dir: &Path) -> Option<PathBuf> {
    cwd.filter(|cwd| cwd.is_dir())
        .or_else(|| Some(workspace_dir).filter(|dir| dir.is_dir()))
        .map(Path::to_path_buf)
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
    runode_dirs::Dirs::from_env().windows_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(cwd: &str) -> SavedNode {
        SavedNode::Leaf { cwd: Some(cwd.into()) }
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
                    first: Box::new(SavedNode::Leaf { cwd: None }),
                    second: Box::new(leaf("/")),
                }),
            },
            focused: 2,
            zoomed: false,
        };
        State::new(vec![SavedWindow {
            bounds: SavedBounds { display: None, mode: WindowMode::Windowed, x: 10., y: 20., width: 960., height: 620. },
            workspaces: vec![SavedWorkspace { name: "tmp".into(), dir: "/tmp".into(), tabs: vec![tab], active: 0 }],
            active: 0,
            sidebar: None,
            sidebar_width: Some(240.),
            changes: true,
            changes_width: None,
            files: false,
            files_width: Some(200.),
            show_ignored: true,
        }])
    }

    #[test]
    fn round_trips_through_json() {
        let state = state();
        let text = serde_json::to_string(&state).unwrap();
        assert_eq!(serde_json::from_str::<State>(&text).unwrap(), state);
        assert!(text.contains(r#""type":"split","axis":"horizontal""#), "{text}");
    }

    #[test]
    fn start_dir_falls_back_to_workspace_then_home() {
        let missing = Path::new("/nonexistent/runode-test");
        assert_eq!(start_dir(Some(Path::new("/")), Path::new("/tmp")), Some("/".into()));
        assert_eq!(start_dir(Some(missing), Path::new("/tmp")), Some("/tmp".into()));
        assert_eq!(start_dir(None, missing), None);
    }
}
