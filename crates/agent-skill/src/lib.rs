//! 教 agent 用 runode 的 skill：仓库 `skills/` 下的文件在构建时编进二进制（`BUNDLED`），照 skills CLI
//! （`npx skills add -g`）的布局装到这台电脑上（`install`），app 启动时把 runode 装过的那份换成这一版的
//! （`refresh`）。正本在 `~/.agents/skills/<名字>/`，直接读那里的 agent 不用另装。

use std::{ffi::OsString, path::PathBuf};

use runode_paths::Dirs;

mod install;

pub use install::{Report, install, install_all, refresh, state};

/// 一个 skill：名字（也是目录名）和它的文件。路径相对 skill 目录、用 `/` 分隔，`SKILL.md` 在最前，
/// 其余是 `references/<主题>.md`。
#[derive(Debug)]
pub struct Skill {
    pub name: &'static str,
    pub files: &'static [(&'static str, &'static str)],
}

/// 编进这一版的 skill，按名字排序。
pub const BUNDLED: &[Skill] = include!(concat!(env!("OUT_DIR"), "/skills.rs"));

/// 装 skill 时要看的位置。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Env {
    /// 家目录；没有时什么都不装。
    pub home: Option<PathBuf>,
    /// Claude Code 的配置目录 `CLAUDE_CONFIG_DIR`；没设时 Claude Code 用 `~/.claude`。
    pub claude_config: Option<PathBuf>,
    /// `XDG_STATE_HOME`：设了时 skills CLI 的 lock 文件在它下面。
    pub state_home: Option<PathBuf>,
}

impl Env {
    /// 家目录取自 `dirs`，别的按 `var` 给出的环境变量，空值当作没设。
    pub fn from_dirs(dirs: &Dirs, var: impl Fn(&str) -> Option<OsString>) -> Self {
        let var = |key: &str| var(key).filter(|v| !v.is_empty()).map(PathBuf::from);
        Self { home: dirs.home.clone(), claude_config: var("CLAUDE_CONFIG_DIR"), state_home: var("XDG_STATE_HOME") }
    }
}

/// 一个 skill 在这台电脑上装成什么样，只看正本。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// 正本不在，或者里面（连同正本是指向别处的链接时链到的地方）没有 `SKILL.md`。
    Missing,
    /// 正本是目录。`by_runode`：里面有 runode 的记录，`refresh` 会让它跟上新版；`current`：文件和这一版
    /// 编进去的一样。
    Installed { by_runode: bool, current: bool },
    /// 正本是符号链接，归别的工具或用户自己管，runode 不动它。
    Elsewhere,
}
