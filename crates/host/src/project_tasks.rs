//! 列一个目录里能跑的项目命令（`ClientMsg::ListProjectTasks`）：手机在会话卡片上列出来，点一下就在
//! 那个会话里跑。先是 runode 根目录的 `tasks.json` 里自己加的、这个目录所在项目的和通用的命令，再从
//! 目录往上找最近的 Makefile 和 package.json，读出目标和 scripts，拼好在这个目录里能直接跑的命令行，
//! 回 `HostMsg::ProjectTasks`。只读几个小文件，在连接的读线程里当场办。

use std::{
    fs,
    io::Read as _,
    path::{Path, PathBuf},
};

use runode_protocol::{MAX_PROJECT_TASKS, ProjectTask, TaskSource, TaskSourceKind};

/// make 按这个顺序找默认的 makefile。
const MAKEFILE_NAMES: [&str; 3] = ["GNUmakefile", "makefile", "Makefile"];
/// 比这大的文件不读：不是手写的。
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// 锁文件和对应的包管理器，同一个目录里有几个时取前面的。
const LOCKFILES: [(&str, &str); 5] = [
    ("pnpm-lock.yaml", "pnpm"),
    ("yarn.lock", "yarn"),
    ("bun.lock", "bun"),
    ("bun.lockb", "bun"),
    ("package-lock.json", "npm"),
];
const PACKAGE_MANAGERS: [&str; 4] = ["npm", "pnpm", "yarn", "bun"];

/// 列出 `dir` 里能跑的命令，依次是自己加的这个项目的、通用的、Makefile 和 package.json。办不了时返回给前端看的原因。
pub(crate) fn list(dir: &Path) -> Result<Vec<TaskSource>, String> {
    if !dir.is_absolute() {
        return Err(format!("{} is not an absolute path", dir.display()));
    }
    if !dir.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    let mut sources = Vec::new();
    let dirs = runode_paths::Dirs::from_env();
    if let Some(file) = dirs.tasks_file()
        && let Some(text) = read_small(&file)
    {
        sources.extend(custom_sources(&text, &file, dir));
    }
    let ancestors = search_path(dir, dirs.home.as_deref());
    let makefile = ancestors.iter().find_map(|at| named_file(at, &MAKEFILE_NAMES));
    if let Some(file) = makefile
        && let Some(text) = read_small(&file)
    {
        let at = file.parent().unwrap_or(dir);
        let prefix = match relative(dir, at) {
            None => "make".to_owned(),
            Some(rel) => format!("make -C {}", quote(&rel)),
        };
        let targets = makefile_targets(&text);
        let truncated = targets.len() > MAX_PROJECT_TASKS;
        let tasks = targets
            .into_iter()
            .take(MAX_PROJECT_TASKS)
            .map(|(name, description)| ProjectTask { command: format!("{prefix} {}", quote(&name)), name, description })
            .collect();
        sources.push(TaskSource { kind: TaskSourceKind::Makefile, file, project: None, tasks, truncated });
    }
    let package = ancestors.iter().find_map(|at| named_file(at, &["package.json"]));
    if let Some(file) = package
        && let Some(text) = read_small(&file)
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&text)
    {
        let at = file.parent().unwrap_or(dir);
        let manager = package_manager(&json, &ancestors[ancestors.iter().position(|p| p == at).unwrap_or(0)..]);
        let scripts = json.get("scripts").and_then(serde_json::Value::as_object);
        let scripts: Vec<_> = scripts.into_iter().flatten().filter_map(|(k, v)| Some((k, v.as_str()?))).collect();
        let truncated = scripts.len() > MAX_PROJECT_TASKS;
        let tasks = scripts
            .into_iter()
            .take(MAX_PROJECT_TASKS)
            .map(|(name, body)| ProjectTask {
                command: format!("{manager} run {}", quote(name)),
                name: name.clone(),
                description: Some(body.to_owned()),
            })
            .collect();
        sources.push(TaskSource { kind: TaskSourceKind::PackageJson, file, project: None, tasks, truncated });
    }
    Ok(sources)
}

/// 自己加的命令文件 `file`（内容是 `text`）里给 `dir` 的两份：`dir` 所在项目的（`projects` 里是 `dir`
/// 自己或上级的、最深的那个，在项目目录里跑，那是 `dir` 的上级时在子 shell 里 cd 过去，不改会话所在的
/// 目录），和通用的（在 `dir` 里跑）。没有命令的那份不给；读不懂的文件当作没有。
fn custom_sources(text: &str, file: &Path, dir: &Path) -> Vec<TaskSource> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let source = |kind, project: Option<PathBuf>, entries: Option<&serde_json::Map<String, serde_json::Value>>| {
        let rel = project.as_deref().and_then(|at| relative(dir, at));
        let entries: Vec<_> = entries.into_iter().flatten().filter_map(|(k, v)| Some((k, v.as_str()?))).collect();
        let truncated = entries.len() > MAX_PROJECT_TASKS;
        let tasks: Vec<_> = entries
            .into_iter()
            .take(MAX_PROJECT_TASKS)
            .map(|(name, line)| ProjectTask {
                command: match &rel {
                    None => line.to_owned(),
                    Some(rel) => format!("(cd {} && {line})", quote(rel)),
                },
                name: name.clone(),
                description: Some(line.to_owned()),
            })
            .collect();
        (!tasks.is_empty()).then(|| TaskSource { kind, file: file.to_path_buf(), project, tasks, truncated })
    };
    let projects = json.get("projects").and_then(serde_json::Value::as_object);
    let project = projects
        .into_iter()
        .flatten()
        .filter(|(at, _)| dir.starts_with(at))
        .max_by_key(|(at, _)| Path::new(at).components().count());
    let global = json.get("global").and_then(serde_json::Value::as_object);
    [
        project.and_then(|(at, tasks)| source(TaskSourceKind::Custom, Some(PathBuf::from(at)), tasks.as_object())),
        source(TaskSourceKind::Global, None, global),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// 往上找的目录：从 `dir` 起，到家目录为止（`dir` 在家目录里时），否则到根目录。
fn search_path(dir: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let stop = home.filter(|home| dir.starts_with(home));
    let mut dirs = Vec::new();
    for at in dir.ancestors() {
        dirs.push(at.to_path_buf());
        if Some(at) == stop {
            break;
        }
    }
    dirs
}

/// `dir` 里名字正好是 `names` 之一的文件，有几个时取前面的。按目录项比名字：不分大小写的文件系统上
/// `dir.join("makefile")` 也打得开 `Makefile`，报出去的文件名就错了。
fn named_file(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    let entries: Vec<_> = fs::read_dir(dir).ok()?.filter_map(|entry| Some(entry.ok()?.file_name())).collect();
    names
        .iter()
        .filter(|name| entries.iter().any(|entry| entry == **name))
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
}

/// 从 `from` 到它的上级 `to` 的相对路径（`..`、`../..`）；是同一个目录时为空。
fn relative(from: &Path, to: &Path) -> Option<String> {
    let rest = from.strip_prefix(to).ok()?;
    let ups = rest.components().count();
    (ups > 0).then(|| vec![".."; ups].join("/"))
}

fn read_small(file: &Path) -> Option<String> {
    let mut text = String::new();
    fs::File::open(file).ok()?.take(MAX_FILE_BYTES).read_to_string(&mut text).ok()?;
    Some(text)
}

/// Makefile 里显式写出来的目标和它那一行 `##` 后面的说明，按先后、去重。不算以 `.` 开头的特殊目标
/// （`.PHONY` 等）、模式规则（`%`）、带变量的（`$`）和像文件路径的（`/`），也不算变量赋值。
fn makefile_targets(text: &str) -> Vec<(String, Option<String>)> {
    let mut targets: Vec<(String, Option<String>)> = Vec::new();
    let mut continued = false;
    for line in text.lines() {
        let was_continued = continued;
        continued = line.ends_with('\\');
        if was_continued || line.starts_with(['\t', ' ', '#']) {
            continue;
        }
        let (rule, description) = match line.split_once("##") {
            Some((rule, description)) => (rule, Some(description.trim()).filter(|d| !d.is_empty())),
            None => (line, None),
        };
        let rule = rule.split_once('#').map_or(rule, |(rule, _)| rule);
        let Some((names, after)) = rule.split_once(':') else { continue };
        if after.starts_with('=') || after.starts_with(":=") || names.contains(['=', '$', '(', ')']) {
            continue;
        }
        for name in names.split_whitespace() {
            if name.starts_with('.') || name.contains(['%', '/']) || targets.iter().any(|(seen, _)| seen == name) {
                continue;
            }
            targets.push((name.to_owned(), description.map(str::to_owned)));
        }
    }
    targets
}

/// 跑 package.json 的 scripts 用哪个包管理器：`packageManager` 字段说了算，没有时看 `dirs`（从
/// package.json 所在的目录往上）里最近的锁文件，都没有时用 npm。
fn package_manager(json: &serde_json::Value, dirs: &[PathBuf]) -> &'static str {
    let declared = json.get("packageManager").and_then(serde_json::Value::as_str).unwrap_or_default();
    let declared = declared.split('@').next().unwrap_or_default();
    if let Some(manager) = PACKAGE_MANAGERS.into_iter().find(|m| *m == declared) {
        return manager;
    }
    dirs.iter()
        .find_map(|dir| LOCKFILES.iter().find(|(file, _)| dir.join(file).is_file()).map(|(_, manager)| *manager))
        .unwrap_or("npm")
}

/// 名字里只有不用转义的字符时原样用，否则加单引号。
fn quote(word: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "-_.:/@+=,%^".contains(c);
    if !word.is_empty() && word.chars().all(plain) {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 取最深的那个项目，在项目目录里跑；通用的在请求的目录里跑；值不是字符串的跳过，没有命令的那份不给。
    #[test]
    fn custom_sources_pick_the_deepest_project() {
        let text = r#"{
            "global": {"up": "git pull"},
            "projects": {
                "/w": {"outer": "x"},
                "/w/app": {"serve": "cargo run -- serve", "lint": "cargo clippy", "n": 1},
                "/w/other": {"no": "y"}
            }
        }"#;
        let file = Path::new("/h/.runode/tasks.json");
        let task = |name: &str, command: &str, line: &str| ProjectTask {
            name: name.into(),
            command: command.into(),
            description: Some(line.into()),
        };
        let sources = custom_sources(text, file, Path::new("/w/app/src"));
        assert_eq!(
            sources,
            [
                TaskSource {
                    kind: TaskSourceKind::Custom,
                    file: file.into(),
                    project: Some("/w/app".into()),
                    tasks: vec![
                        task("serve", "(cd .. && cargo run -- serve)", "cargo run -- serve"),
                        task("lint", "(cd .. && cargo clippy)", "cargo clippy"),
                    ],
                    truncated: false,
                },
                TaskSource {
                    kind: TaskSourceKind::Global,
                    file: file.into(),
                    project: None,
                    tasks: vec![task("up", "git pull", "git pull")],
                    truncated: false,
                },
            ]
        );
        assert_eq!(custom_sources(r#"{"projects": {"/w/app": {}}}"#, file, Path::new("/w/app")), []);
        assert_eq!(custom_sources("{oops", file, Path::new("/w")), []);
        // `/w/application` 不在 `/w/app` 里。
        let sources = custom_sources(text, file, Path::new("/w/application"));
        assert_eq!(sources[0].project.as_deref(), Some(Path::new("/w")));
    }

    #[test]
    fn makefile_targets_skip_variables_specials_and_patterns() {
        let text = "\
.PHONY: build test
VERSION := 1.0
CC ?= cc:x
export PATH:=/bin
build: deps ## 编译
\t$(CC) -o x
test check: build # 普通注释
%.o: %.c
$(OUT): build
dist/app: build
release:: build
long: a \\
  b: c
ifeq ($(OS),Darwin:x)
endif
build: other
";
        let targets = makefile_targets(text);
        assert_eq!(
            targets,
            [
                ("build".to_owned(), Some("编译".to_owned())),
                ("test".to_owned(), None),
                ("check".to_owned(), None),
                ("release".to_owned(), None),
                ("long".to_owned(), None),
            ]
        );
    }

    #[test]
    fn search_stops_at_home() {
        let dirs = search_path(Path::new("/Users/me/dev/app"), Some(Path::new("/Users/me")));
        assert_eq!(dirs, [PathBuf::from("/Users/me/dev/app"), "/Users/me/dev".into(), "/Users/me".into()]);
        let dirs = search_path(Path::new("/opt/x"), Some(Path::new("/Users/me")));
        assert_eq!(dirs, [PathBuf::from("/opt/x"), "/opt".into(), "/".into()]);
    }

    #[test]
    fn relative_paths_go_up() {
        assert_eq!(relative(Path::new("/a/b/c"), Path::new("/a")), Some("../..".into()));
        assert_eq!(relative(Path::new("/a"), Path::new("/a")), None);
    }

    #[test]
    fn names_are_quoted_when_needed() {
        assert_eq!(quote("build:ios"), "build:ios");
        assert_eq!(quote("run it"), "'run it'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote("a;rm"), "'a;rm'");
    }
}
