//! 列一个目录里能跑的命令（`list`），以及看板新开的工作树先跑什么（`worktree_setup`）。`list` 先列
//! runode 根目录的 `tasks.json` 里自己加的、这个目录所在项目的和通用的命令，再从目录往上找最近的
//! Makefile 和 package.json，读出目标和 scripts，拼好在这个目录里能直接跑的命令行。宿主用它回
//! `ClientMsg::ListTasks`，手机在会话卡片上、桌面在标题栏的菜单里列出来。只读几个小文件，但在断掉的网络挂载上也会一直卡住，调用方别在要及时响应的线程上调。

use std::{
    collections::HashSet,
    fs,
    io::Read as _,
    path::{Path, PathBuf},
};

use runode_types::tasks::{MAX_TASKS, TaskEntry, TaskSource, TaskSourceKind};

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
pub fn list(dir: &Path) -> Result<Vec<TaskSource>, String> {
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
        && let Some(prefix) = make_prefix(dir, file.parent().unwrap_or(dir))
        && let Some(text) = read_small(&file)
    {
        let targets = makefile_targets(&text);
        let truncated = targets.len() > MAX_TASKS;
        let tasks = targets
            .into_iter()
            .take(MAX_TASKS)
            .map(|(name, description)| TaskEntry { command: format!("{prefix} {}", quote(&name)), name, description })
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
        let scripts: Vec<_> = scripts
            .into_iter()
            .flatten()
            .filter(|(k, _)| quotable(k))
            .filter_map(|(k, v)| Some((k, v.as_str()?)))
            .collect();
        let truncated = scripts.len() > MAX_TASKS;
        let tasks = scripts
            .into_iter()
            .take(MAX_TASKS)
            .map(|(name, body)| TaskEntry {
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
/// 自己或上级的、最深的那个，在 `TaskSource::project` 里跑），和通用的（在 `dir` 里跑）。命令行原样
/// 给，不包 `(cd .. && …)`：fish 里括号是命令替换，行尾的 `#` 注释也会吃掉右括号；换目录由前端在
/// 项目目录里开终端办。没有命令的那份不给；读不懂的文件当作没有。
fn custom_sources(text: &str, file: &Path, dir: &Path) -> Vec<TaskSource> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let source = |kind, project: Option<PathBuf>, entries: Option<&serde_json::Map<String, serde_json::Value>>| {
        let entries: Vec<_> = entries.into_iter().flatten().filter_map(|(k, v)| Some((k, v.as_str()?))).collect();
        let truncated = entries.len() > MAX_TASKS;
        let tasks: Vec<_> = entries
            .into_iter()
            .take(MAX_TASKS)
            .map(|(name, line)| TaskEntry {
                command: line.to_owned(),
                name: name.clone(),
                description: Some(line.to_owned()),
            })
            .collect();
        (!tasks.is_empty()).then(|| TaskSource { kind, file: file.to_path_buf(), project, tasks, truncated })
    };
    let project = deepest(json.get("projects"), dir);
    let global = json.get("global").and_then(serde_json::Value::as_object);
    [
        project.and_then(|(at, tasks)| source(TaskSourceKind::Custom, Some(PathBuf::from(at)), tasks.as_object())),
        source(TaskSourceKind::Global, None, global),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// 给看板新开的工作树先跑的命令：`tasks.json` 的 `setup` 里给 `dir`（任务的目录）自己或上级的、最深的
/// 那一项，在工作树里跑。没有文件、读不懂、没配时为 `None`。和 `list` 一样会读文件。
pub fn worktree_setup(dir: &Path) -> Option<String> {
    let text = read_small(&runode_paths::Dirs::from_env().tasks_file()?)?;
    setup_in(&text, dir)
}

fn setup_in(text: &str, dir: &Path) -> Option<String> {
    let json = serde_json::from_str::<serde_json::Value>(text).ok()?;
    let line = deepest(json.get("setup"), dir)?.1.as_str()?.trim();
    (!line.is_empty()).then(|| line.to_owned())
}

/// 按目录分份的表 `table` 里给 `dir` 的那份：键是 `dir` 自己或上级的、最深的那个。
fn deepest<'a>(table: Option<&'a serde_json::Value>, dir: &Path) -> Option<(&'a String, &'a serde_json::Value)> {
    table?
        .as_object()?
        .iter()
        .filter(|(at, _)| dir.starts_with(at))
        .max_by_key(|(at, _)| Path::new(at).components().count())
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

/// 在 `dir` 里跑上级 `at` 里的 Makefile 用的命令开头：同一个目录时是 `make`，否则是 `make -C` 加目录。
/// 目录平常是相对路径（`..`、`../..`）；`..` 由 make 从进程真实的目录算，`dir` 经过符号链接时到不了
/// `at`，这时用 `at` 的绝对路径。绝对路径 `quote` 拼不了时为空：相对路径跑的是另一个目录里的 Makefile，
/// 不如不列。
fn make_prefix(dir: &Path, at: &Path) -> Option<String> {
    let Some(rel) = relative(dir, at) else {
        return Some("make".to_owned());
    };
    let real = |path: &Path| fs::canonicalize(path).ok();
    let reaches = real(&dir.join(&rel)).is_some_and(|target| Some(target) == real(at));
    let target = if reaches { Some(rel.as_str()) } else { at.to_str().filter(|absolute| quotable(absolute)) };
    target.map(|target| format!("make -C {}", quote(target)))
}

/// 读整个文件；比 `MAX_FILE_BYTES` 大的返回 None，不截断着读。
fn read_small(file: &Path) -> Option<String> {
    let mut text = String::new();
    // 多读一个字节才知道超没超；不先看文件长度，读的过程中文件可能变大。
    fs::File::open(file).ok()?.take(MAX_FILE_BYTES + 1).read_to_string(&mut text).ok()?;
    (text.len() as u64 <= MAX_FILE_BYTES).then_some(text)
}

/// Makefile 里显式写出来的目标和它那一行 `##` 后面的说明，按先后、去重。不算以 `.` 开头的特殊目标
/// （`.PHONY` 等）、模式规则（`%`）、带变量的（`$`）、像文件路径的（`/`）和 `quotable` 不收的，也不算
/// 变量赋值。只列得出 `MAX_TASKS` 个，收到多一个（看得出列不全）就不往下找了。
fn makefile_targets(text: &str) -> Vec<(String, Option<String>)> {
    let mut targets: Vec<(String, Option<String>)> = Vec::new();
    let mut seen = HashSet::new();
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
            if name.starts_with('.') || name.contains(['%', '/']) || !quotable(name) || !seen.insert(name) {
                continue;
            }
            targets.push((name.to_owned(), description.map(str::to_owned)));
            if targets.len() > MAX_TASKS {
                return targets;
            }
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

/// 能用 `quote` 拼进命令行的任务名：以 `-` 开头的会被 make、包管理器当成选项；fish 的单引号里
/// `\\` 和 `\'` 是转义，含 `\` 的加了单引号在 fish 里也不对；控制字符（换行等）会把一条命令拆开。
/// 这些任务不列出来。
fn quotable(name: &str) -> bool {
    !name.starts_with('-') && !name.contains(|c: char| c == '\\' || c.is_control())
}

/// 名字里只有不用转义的字符时原样用，否则加单引号。只给 `quotable` 的名字用：单引号里的 `\` 在 fish
/// 里不是原样的。以 `=` 开头的也加：zsh 把 `=名字` 展开成那个命令的路径。
fn quote(word: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "-_.:/@+=,%^".contains(c);
    if !word.is_empty() && !word.starts_with('=') && word.chars().all(plain) {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// setup 取最深的那个项目；空的和不是字符串的当没配。
    #[test]
    fn setup_picks_the_deepest_project() {
        let text = r#"{"setup": {"/w": "make deps", "/w/app": " pnpm install ", "/w/empty": " ", "/w/n": 1}}"#;
        assert_eq!(setup_in(text, Path::new("/w/app/src")).as_deref(), Some("pnpm install"));
        assert_eq!(setup_in(text, Path::new("/w/lib")).as_deref(), Some("make deps"));
        assert_eq!(setup_in(text, Path::new("/w/empty")), None);
        assert_eq!(setup_in(text, Path::new("/w/n")), None);
        assert_eq!(setup_in(text, Path::new("/elsewhere")), None);
        assert_eq!(setup_in("{", Path::new("/w")), None);
    }

    /// 取最深的那个项目，在项目目录里跑；通用的在请求的目录里跑；值不是字符串的跳过，没有命令的那份不给。
    #[test]
    fn custom_sources_pick_the_deepest_project() {
        let text = r#"{
            "global": {"up": "git pull"},
            "projects": {
                "/w": {"outer": "x"},
                "/w/app": {"serve": "cargo run -- serve", "lint": "cargo clippy # slow", "n": 1},
                "/w/other": {"no": "y"}
            }
        }"#;
        let file = Path::new("/h/.runode/tasks.json");
        let task = |name: &str, command: &str, line: &str| TaskEntry {
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
                        task("serve", "cargo run -- serve", "cargo run -- serve"),
                        task("lint", "cargo clippy # slow", "cargo clippy # slow"),
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

    /// 变量赋值、特殊目标、模式规则、带变量的不列；以 `-` 开头的（`make -x` 会当成选项）、含 `\` 和
    /// 控制字符的也不列。
    #[test]
    fn makefile_targets_skip_variables_specials_and_patterns() {
        let text = "\
.PHONY: build test
-x: build
a\\b: build
bell\x07: build
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

    /// 正好到上限的文件照读，超过上限的整个不读，不截断着读。
    #[test]
    fn files_over_the_limit_are_not_read() {
        let root = std::env::temp_dir().join(format!("runode-tasks-large-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let limit = MAX_FILE_BYTES as usize;
        fs::write(root.join("fits"), "a".repeat(limit)).unwrap();
        fs::write(root.join("over"), "a".repeat(limit + 1)).unwrap();
        let fits = read_small(&root.join("fits"));
        let over = read_small(&root.join("over"));
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(fits.map(|text| text.len()), Some(limit));
        assert_eq!(over, None);
    }

    /// 请求的目录经过符号链接时，`make -C ..` 里的 `..` 由 make 从真实的目录算，到不了找到的
    /// Makefile 那里：改用它所在目录的绝对路径。
    #[cfg(unix)]
    #[test]
    fn a_symlinked_directory_gets_an_absolute_make_dir() {
        let root = std::env::temp_dir().join(format!("runode-tasks-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("ext/project/sub")).unwrap();
        fs::create_dir_all(root.join("work")).unwrap();
        fs::write(root.join("ext/project/Makefile"), "real:\n").unwrap();
        fs::write(root.join("work/Makefile"), "wrong:\n").unwrap();
        std::os::unix::fs::symlink(root.join("ext/project/sub"), root.join("work/link")).unwrap();
        let sources = list(&root.join("work/link")).unwrap();
        let normal = list(&root.join("ext/project/sub")).unwrap();
        fs::remove_dir_all(&root).unwrap();
        let commands = |sources: &[TaskSource]| -> Vec<String> {
            let make = sources.iter().find(|source| source.kind == TaskSourceKind::Makefile).unwrap();
            make.tasks.iter().map(|task| task.command.clone()).collect()
        };
        assert_eq!(commands(&sources), [format!("make -C {} wrong", quote(root.join("work").to_str().unwrap()))]);
        assert_eq!(commands(&normal), ["make -C .. real"]);
    }

    /// 经过符号链接、Makefile 所在目录的绝对路径又拼不进命令行（这里带反斜杠）时不列：退回 `make -C ..`
    /// 跑的是另一个目录里的 Makefile。
    #[cfg(unix)]
    #[test]
    fn an_unreachable_makefile_with_an_unquotable_path_is_not_listed() {
        let root = std::env::temp_dir().join(format!("runode-tasks-unquotable-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("ext/project/sub")).unwrap();
        fs::create_dir_all(root.join("wo\\rk")).unwrap();
        fs::write(root.join("ext/project/Makefile"), "real:\n").unwrap();
        fs::write(root.join("wo\\rk/Makefile"), "real:\n").unwrap();
        std::os::unix::fs::symlink(root.join("ext/project/sub"), root.join("wo\\rk/link")).unwrap();
        let sources = list(&root.join("wo\\rk/link")).unwrap();
        fs::remove_dir_all(&root).unwrap();
        assert!(sources.iter().all(|source| source.kind != TaskSourceKind::Makefile), "{sources:?}");
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
        assert_eq!(quote("=build"), "'=build'");
        assert_eq!(quote("a=b"), "a=b");
    }

    /// 目标多得列不全时收到 `MAX_TASKS` 加一个就停，看得出列不全。
    #[test]
    fn makefile_targets_stop_past_the_limit() {
        let text: String = (0..MAX_TASKS * 50).map(|i| format!("t{i}:\n")).collect();
        let targets = makefile_targets(&text);
        assert_eq!(targets.len(), MAX_TASKS + 1);
        assert_eq!(targets[MAX_TASKS].0, format!("t{MAX_TASKS}"));
    }
}
