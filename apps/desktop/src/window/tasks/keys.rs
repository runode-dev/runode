//! 命令的快捷键，和命令一样存在 runode 根目录的 `tasks.json`（`Dirs::tasks_file`）里、按项目分份：
//! `{"keys": {"global": {"触发键": "名字", …}, "projects": {"/项目/目录": {"触发键": "名字", …}, …}}}`，
//! 触发键是配置文件的写法（`alt+cmd+r`）。按下时先看终端目录所在项目的那一份（`keys.projects` 里是它自己或
//! 上级的、最深的那个），再看通用的；都没绑时这个键照常交给别的绑定或终端，所以各个项目能把同一个键绑给
//! 各自的命令。项目的键跑这个目录列出来的、不是通用的命令（自己加的、Makefile、package.json），通用的键只跑
//! 通用命令。启动和配置重载时读进来（`TaskKeys`），装快捷键时每个绑着的键都绑上 `RunTaskKey`。改文件在
//! `custom`。

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use gpui::{Action, Global};
use runode_config::keybind;
use runode_paths::Dirs;
use serde_json::Value;

/// 读进来的命令快捷键，触发键换成了 GPUI 的写法；每份按文件里的先后，同一个键写了两次时后写的赢。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TaskKeys {
    global: Vec<(String, String)>,
    projects: Vec<(PathBuf, Vec<(String, String)>)>,
}

impl Global for TaskKeys {}

/// 按了 `TaskKeys` 里的一个键（GPUI 的写法）。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub struct RunTaskKey(pub String);

impl TaskKeys {
    /// 读 `tasks.json` 里的快捷键，没有文件时为空。读不了、读不懂（比如正手改到一半）时记一条警告、返回
    /// `None`，调用方留着上次读到的，免得所有命令的快捷键一下子都没了。
    pub fn read() -> Option<Self> {
        let Some(file) = Dirs::from_env().tasks_file() else {
            return Some(Self::default());
        };
        let text = match fs::read_to_string(&file) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Some(Self::default()),
            Err(err) => {
                tracing::warn!("cannot read the task shortcuts in {}: {err}", file.display());
                return None;
            }
        };
        Self::parse(&text).inspect_err(|err| tracing::warn!("{}: {err}", file.display())).ok()
    }

    /// 空文件当作没有。
    fn parse(text: &str) -> Result<Self, serde_json::Error> {
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        let json = serde_json::from_str::<Value>(text)?;
        let keys = json.get("keys");
        let table = |value: &Value| -> Vec<(String, String)> {
            let entries = value.as_object().into_iter().flatten();
            entries
                .filter_map(|(trigger, name)| {
                    let name = name.as_str()?.to_owned();
                    keybind::parse_trigger(trigger)
                        .inspect_err(|err| tracing::warn!("task shortcut {trigger}: {err}"))
                        .ok()
                        .map(|keys| (keys, name))
                })
                .collect()
        };
        let projects = keys.and_then(|keys| keys.get("projects")).and_then(Value::as_object).into_iter().flatten();
        Ok(Self {
            global: keys.and_then(|keys| keys.get("global")).map(table).unwrap_or_default(),
            projects: projects.map(|(dir, tasks)| (PathBuf::from(dir), table(tasks))).collect(),
        })
    }

    /// 绑着的每个键，GPUI 的写法，去了重。
    pub fn triggers(&self) -> Vec<&str> {
        let tables = std::iter::once(&self.global).chain(self.projects.iter().map(|(_, table)| table));
        let mut keys: Vec<&str> = tables.flatten().map(|(keys, _)| keys.as_str()).collect();
        keys.sort_unstable();
        keys.dedup();
        keys
    }

    /// `dir` 用的是哪个项目的那一份：`keys.projects` 里是它自己或上级的、最深的那个。
    pub(super) fn project(&self, dir: &Path) -> Option<&Path> {
        let dirs = self.projects.iter().map(|(at, _)| at.as_path());
        dirs.filter(|at| dir.starts_with(at)).max_by_key(|at| at.components().count())
    }

    /// `project` 那一份，为空时是通用的。
    fn table(&self, project: Option<&Path>) -> &[(String, String)] {
        match project {
            None => &self.global,
            Some(project) => self.projects.iter().find(|(at, _)| at == project).map_or(&[], |(_, table)| table),
        }
    }

    /// `project` 那一份（通用的为空）里 `keys` 跑的命令。
    pub(super) fn name_at(&self, project: Option<&Path>, keys: &str) -> Option<&str> {
        self.table(project).iter().rfind(|(bound, _)| bound == keys).map(|(_, name)| name.as_str())
    }

    /// 在 `dir` 里按 `keys` 跑哪条命令：（是不是通用命令，名字）。
    pub(super) fn lookup(&self, dir: &Path, keys: &str) -> Option<(bool, &str)> {
        let project = self.project(dir).and_then(|project| self.name_at(Some(project), keys));
        project.map(|name| (false, name)).or_else(|| self.name_at(None, keys).map(|name| (true, name)))
    }

    /// `project` 那一份（通用的为空）里绑在 `name` 上的键，配置文件的写法。
    pub(super) fn shortcut(&self, project: Option<&Path>, name: &str) -> Option<String> {
        let table = self.table(project);
        // 被同一份里后写的同一个键盖掉的不算。
        let live = table.iter().find(|(keys, bound)| bound == name && self.name_at(project, keys) == Some(name));
        live.and_then(|(keys, _)| keybind::format_trigger(keys))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::TaskKeys;

    /// 先看最深的项目那份，没绑再看通用的；别的项目的、读不懂的键不算。
    #[test]
    fn looks_up_the_deepest_project_then_global() {
        let keys = TaskKeys::parse(
            r#"{"keys": {
                "global": {"cmd+r": "up", "cmd+u": "up"},
                "projects": {"/w": {"cmd+t": "outer"}, "/w/app": {"cmd+r": "serve", "oops+x": "bad"}, "/x": {"cmd+y": "x"}}
            }}"#,
        )
        .unwrap();
        assert_eq!(keys.lookup(Path::new("/w/app/src"), "cmd-r"), Some((false, "serve")));
        assert_eq!(keys.lookup(Path::new("/w/app/src"), "cmd-u"), Some((true, "up")));
        assert_eq!(keys.lookup(Path::new("/w/app"), "cmd-t"), None);
        assert_eq!(keys.lookup(Path::new("/w/other"), "cmd-t"), Some((false, "outer")));
        assert_eq!(keys.lookup(Path::new("/w/other"), "cmd-r"), Some((true, "up")));
        assert_eq!(keys.lookup(Path::new("/w/app"), "cmd-y"), None);
        assert_eq!(keys.triggers(), ["cmd-r", "cmd-t", "cmd-u", "cmd-y"]);
        assert_eq!(keys.shortcut(Some(Path::new("/w/app")), "serve").as_deref(), Some("cmd+r"));
        assert_eq!(keys.shortcut(None, "serve"), None);
        assert!(TaskKeys::parse("{oops").is_err());
        assert_eq!(TaskKeys::parse(" ").unwrap(), TaskKeys::default());
    }
}
