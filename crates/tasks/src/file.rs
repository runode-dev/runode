//! 改写 runode 根目录的 `tasks.json`（`Dirs::tasks_file`）里自己加的命令和它们的快捷键：
//! `{"global": {"名字": "命令行", …}, "projects": {"/项目/目录": {…}}, "keys": {"global": {"触发键": "名字", …},
//! "projects": {"/项目/目录": {…}}}}`。快捷键按名字找命令，命令改名、换份、删除时跟着改。只动 JSON，
//! 怎么认键、目录属于哪个项目由调用方给（`Rules`）。

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
};

use serde_json::{Map, Value};

/// 改 `tasks.json` 时要调用方回答的两件事。
pub struct Rules {
    /// 两种写法是不是同一个键。
    pub same_key: fn(&str, &str) -> bool,
    /// 终端目录不在已经记过的项目里时，它的项目快捷键记在哪个目录。
    pub project_of: fn(&Path) -> PathBuf,
}

/// 改命令文件的读、改、写换上去整个拿着它：保存和删除各是一个后台任务，几个窗口也会同时改，两个一起读到
/// 同一份内容再各自换上去时，先写的那份改动就丢了。
static WRITING: Mutex<()> = Mutex::new(());

/// 快捷键怎么跟着命令改，见 `move_keys`。
pub enum Keys {
    /// 原来绑的几个键原样跟着走。
    Keep,
    /// 换成这一个，为空时不绑。
    Set(Option<String>),
}

/// 把 `file` 里的 `old`（项目目录，通用的为空；名字）换成 `new`（项目目录；名字和命令行），`new` 为空时
/// 删掉 `old`；绑在它上面的快捷键按 `keys` 跟着改，项目命令的记在终端目录 `dir` 用的那份里（`key_project`）。
pub fn write_task(
    file: &Path,
    dir: &Path,
    old: (Option<&Path>, &str),
    new: Option<(Option<&Path>, &str, &str)>,
    keys: Keys,
    rules: &Rules,
) -> Result<(), String> {
    edit_file(file, |root| {
        let projects = old.0.is_some() || new.is_some_and(|(project, ..)| project.is_some());
        let keys_at = projects.then(|| key_project(root, dir, rules));
        let scope = |project: Option<&Path>| project.and(keys_at.as_deref());
        with_task(root, old, new)?;
        let to = new.map(|(project, name, _)| (scope(project), name));
        move_keys(root, (scope(old.0), old.1), to, &keys, rules)
    })
}

/// 给终端目录 `dir` 列出来的、叫 `name` 的命令（Makefile、package.json 的）绑上 `trigger`，为空时去掉它的
/// 快捷键，记在 `dir` 用的那份里。
pub fn write_task_key(file: &Path, dir: &Path, name: &str, trigger: Option<&str>, rules: &Rules) -> Result<(), String> {
    edit_file(file, |root| {
        let at = key_project(root, dir, rules);
        let keys = Keys::Set(trigger.map(str::to_owned));
        move_keys(root, (Some(&at), name), Some((Some(&at), name)), &keys, rules)
    })
}

/// 改命令文件 `file`：文件和它的目录不在时建出来，没有文件、改完也是空的（比如删除）时不建；读不懂原来的
/// 内容时不写，免得把手写的文件冲掉。
fn edit_file(file: &Path, edit: impl FnOnce(&mut Map<String, Value>) -> Result<(), String>) -> Result<(), String> {
    let _writing = WRITING.lock().unwrap_or_else(PoisonError::into_inner);
    let text = match fs::read_to_string(file) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(format!("{}: {err}", file.display())),
    };
    let updated = edit_text(text.as_deref(), edit).map_err(|err| format!("{}: {err}", file.display()))?;
    if text.is_none() && updated.trim() == "{}" {
        return Ok(());
    }
    let dir = file.parent().map(PathBuf::from).unwrap_or_default();
    // 用户手写过的文件：写临时文件再换上去，写到一半出错不留下空的或半截的。
    fs::create_dir_all(&dir)
        .and_then(|()| runode_paths::replace_file(file, updated.as_bytes()))
        .map_err(|err| format!("{}: {err}", file.display()))
}

/// 在命令文件的内容 `text`（没有文件时为空）上做 `edit`。删空了的项目、通用那份和快捷键那份一起拿掉，
/// 别的内容和先后原样留着。
fn edit_text(
    text: Option<&str>,
    edit: impl FnOnce(&mut Map<String, Value>) -> Result<(), String>,
) -> Result<String, String> {
    let mut json = match text.filter(|text| !text.trim().is_empty()) {
        Some(text) => serde_json::from_str(text).map_err(|err| err.to_string())?,
        None => Value::Object(Map::new()),
    };
    let root = json.as_object_mut().ok_or("not a JSON object")?;
    edit(root)?;
    if let Some(keys) = root.get_mut("keys").and_then(Value::as_object_mut) {
        prune(keys);
    }
    prune(root);
    let mut text = serde_json::to_string_pretty(&json).map_err(|err| err.to_string())?;
    text.push('\n');
    Ok(text)
}

/// 拿掉空了的项目，和空了的 `global`、`projects`、`keys`。
fn prune(root: &mut Map<String, Value>) {
    if let Some(projects) = root.get_mut("projects").and_then(Value::as_object_mut) {
        projects.retain(|_, tasks| tasks.as_object().is_none_or(|tasks| !tasks.is_empty()));
    }
    root.retain(|key, value| {
        !(matches!(key.as_str(), "global" | "projects" | "keys") && value.as_object().is_some_and(Map::is_empty))
    });
}

/// 把 `old` 换成 `new`。同一份里就在 `old` 原来的位置换，换到另一份时从原来那份删掉、加在新那份的最后；
/// `new` 为空时只删掉 `old`。新名字在那一份里别处已经有了时，那一条让位。
fn with_task(
    root: &mut Map<String, Value>,
    old: (Option<&Path>, &str),
    new: Option<(Option<&Path>, &str, &str)>,
) -> Result<(), String> {
    let same = new.is_some_and(|(project, ..)| project == old.0);
    replace(scope_tasks(root, old.0)?, Some(old.1), new.filter(|_| same).map(|(_, name, command)| (name, command)));
    if let Some((project, name, command)) = new.filter(|_| !same) {
        replace(scope_tasks(root, project)?, None, Some((name, command)));
    }
    Ok(())
}

/// 快捷键那一份 `from`（项目目录，通用的为空；名字）里绑在这个名字上的键都去掉，`to` 有时换到那里：
/// `Keep` 原来那几个键搬过去（同一份里就原地改名，先后不变），`Set` 只绑给的那一个，那份里同一个键（写法
/// 不同也算）原来跑别的命令时换成这一条。
fn move_keys(
    root: &mut Map<String, Value>,
    from: (Option<&Path>, &str),
    to: Option<(Option<&Path>, &str)>,
    keys: &Keys,
    rules: &Rules,
) -> Result<(), String> {
    let table = key_table(root, from.0)?;
    // 同一个键写了两种写法时后写的赢：原地改名才不会把被盖住的那个挪到后面、抢走别的命令的键。
    if let (Keys::Keep, Some((project, name))) = (keys, to)
        && project == from.0
    {
        for value in table.values_mut().filter(|value| value.as_str() == Some(from.1)) {
            *value = Value::String(name.to_owned());
        }
        return Ok(());
    }
    let bound: Vec<String> =
        table.iter().filter(|(_, name)| name.as_str() == Some(from.1)).map(|(trigger, _)| trigger.clone()).collect();
    table.retain(|_, name| name.as_str() != Some(from.1));
    let triggers = match keys {
        Keys::Keep => bound,
        Keys::Set(trigger) => trigger.iter().cloned().collect(),
    };
    if let Some((project, name)) = to {
        let table = key_table(root, project)?;
        for trigger in triggers {
            if let Keys::Set(_) = keys {
                table.retain(|bound, _| !(rules.same_key)(bound, &trigger));
            }
            table.insert(trigger, Value::String(name.to_owned()));
        }
    }
    Ok(())
}

/// 快捷键里 `project` 那一份（为空时是通用的），没有时建出来。
fn key_table<'a>(
    root: &'a mut Map<String, Value>,
    project: Option<&Path>,
) -> Result<&'a mut Map<String, Value>, String> {
    let keys = root.entry("keys").or_insert_with(|| Value::Object(Map::new()));
    scope_tasks(keys.as_object_mut().ok_or("`keys` is not a JSON object")?, project)
}

/// 终端目录 `dir` 的项目快捷键记在哪份：先找已经有键的、`dir` 自己或上级里最深的那份，再找自己加的
/// 命令所在的项目，都没有时问 `Rules::project_of`。
fn key_project(root: &Map<String, Value>, dir: &Path, rules: &Rules) -> PathBuf {
    let deepest = |table: Option<&Value>| {
        let dirs = table.and_then(Value::as_object).into_iter().flatten().map(|(at, _)| Path::new(at));
        dirs.filter(|at| dir.starts_with(at)).max_by_key(|at| at.components().count()).map(Path::to_path_buf)
    };
    deepest(root.get("keys").and_then(|keys| keys.get("projects")))
        .or_else(|| deepest(root.get("projects")))
        .unwrap_or_else(|| (rules.project_of)(dir))
}

/// `project` 那一份（为空时是通用的）的命令表，没有时建出来。
fn scope_tasks<'a>(
    root: &'a mut Map<String, Value>,
    project: Option<&Path>,
) -> Result<&'a mut Map<String, Value>, String> {
    let object = || Value::Object(Map::new());
    let tasks = match project {
        None => root.entry("global").or_insert_with(object),
        Some(project) => root
            .entry("projects")
            .or_insert_with(object)
            .as_object_mut()
            .ok_or("`projects` is not a JSON object")?
            .entry(project.to_string_lossy())
            .or_insert_with(object),
    };
    tasks.as_object_mut().ok_or_else(|| "a task list is not a JSON object".to_owned())
}

/// 在命令表里把 `old` 换成 `new`，就在 `old` 的位置；没有 `old` 时 `new` 加在最后，`new` 为空时删掉
/// `old`。`new` 的名字别处已经有了时，那一条让位。
fn replace(tasks: &mut Map<String, Value>, old: Option<&str>, new: Option<(&str, &str)>) {
    let entry = |(name, command): (&str, &str)| (name.to_owned(), Value::String(command.to_owned()));
    let mut placed = false;
    let mut rebuilt = Map::new();
    for (key, value) in std::mem::take(tasks) {
        if Some(key.as_str()) == old {
            rebuilt.extend(new.map(entry));
            placed = true;
        } else if new.is_none_or(|(name, _)| key != name) {
            rebuilt.insert(key, value);
        }
    }
    if !placed {
        rebuilt.extend(new.map(entry));
    }
    *tasks = rebuilt;
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Keys, Rules, edit_text, move_keys, write_task};

    /// `command` 和 `cmd` 是同一个修饰键。
    const RULES: Rules = Rules {
        same_key: |a, b| a.replace("command", "cmd") == b.replace("command", "cmd"),
        project_of: Path::to_path_buf,
    };

    /// 文本进、文本出地改命令：`edit_text` 套上 `with_task`。
    fn with_task(
        text: Option<&str>,
        old: (Option<&Path>, &str),
        new: Option<(Option<&Path>, &str, &str)>,
    ) -> Result<String, String> {
        edit_text(text, |root| super::with_task(root, old, new))
    }

    fn pretty(json: serde_json::Value) -> String {
        serde_json::to_string_pretty(&json).unwrap() + "\n"
    }

    #[test]
    fn adds_to_new_and_existing_files() {
        let app = Some(Path::new("/w/app"));
        assert_eq!(
            with_task(None, (app, "dev"), Some((app, "dev", "cargo run"))).unwrap(),
            pretty(serde_json::json!({"projects": {"/w/app": {"dev": "cargo run"}}}))
        );
        let existing = r#"{"note": 1, "global": {"dev": "old", "lint": "cargo clippy"}}"#;
        let updated = with_task(Some(existing), (None, "dev"), Some((None, "dev", "cargo run"))).unwrap();
        assert_eq!(
            updated,
            pretty(serde_json::json!({"note": 1, "global": {"dev": "cargo run", "lint": "cargo clippy"}}))
        );
        assert!(with_task(Some("[1]"), (None, "dev"), Some((None, "dev", "x"))).is_err());
        assert!(with_task(Some("{oops"), (None, "dev"), Some((None, "dev", "x"))).is_err());
    }

    /// 几个后台任务同时改命令文件：各自的改动都留下，不会读到同一份内容再互相盖掉。
    #[test]
    fn concurrent_writes_keep_every_change() {
        let dir = std::env::temp_dir().join(format!("rdr-tasks-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("tasks.json");
        let names: Vec<String> = (0..16).map(|ix| format!("task{ix}")).collect();
        std::thread::scope(|scope| {
            for name in &names {
                let file = &file;
                scope.spawn(move || {
                    write_task(file, Path::new("/"), (None, name), Some((None, name, "true")), Keys::Keep, &RULES)
                        .unwrap()
                });
            }
        });
        let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        for name in &names {
            assert!(json["global"].get(name).is_some(), "{name} is missing: {json}");
        }
    }

    /// 快捷键跟着命令走：`Keep` 把原来的键搬到新名字、新的一份，`Set` 换成给的那一个、挤掉同一份里同一个键
    /// （写法不同也算）上的别的命令，删掉命令时它的键一起去掉，删空了的那份拿掉；别的份和别的命令的键不动。
    #[test]
    fn keys_follow_their_task() {
        let app = Some(Path::new("/w/app"));
        let existing = r#"{"keys": {"global": {"cmd+r": "up"}, "projects": {"/w/app": {"cmd+1": "dev", "cmd+2": "dev", "command+t": "lint"}}}}"#;
        let edit =
            |from, to, keys: Keys| edit_text(Some(existing), |root| move_keys(root, from, to, &keys, &RULES)).unwrap();
        assert_eq!(
            edit((app, "dev"), Some((None, "serve")), Keys::Keep),
            pretty(serde_json::json!({"keys": {
                "global": {"cmd+r": "up", "cmd+1": "serve", "cmd+2": "serve"},
                "projects": {"/w/app": {"command+t": "lint"}}
            }}))
        );
        assert_eq!(
            edit((app, "dev"), Some((app, "dev")), Keys::Set(Some("cmd+t".into()))),
            pretty(serde_json::json!({"keys": {"global": {"cmd+r": "up"}, "projects": {"/w/app": {"cmd+t": "dev"}}}}))
        );
        assert_eq!(
            edit((None, "up"), None, Keys::Keep),
            pretty(
                serde_json::json!({"keys": {"projects": {"/w/app": {"cmd+1": "dev", "cmd+2": "dev", "command+t": "lint"}}}})
            )
        );
        assert_eq!(edit_text(None, |root| move_keys(root, (None, "x"), None, &Keys::Keep, &RULES)).unwrap(), "{}\n");
        // 同一个键两种写法、后写的 `lint` 生效：只改 `dev` 的名字不动先后，键还是 `lint` 的。
        let aliased = r#"{"keys": {"global": {"cmd+r": "dev", "command+r": "lint"}}}"#;
        let renamed =
            edit_text(Some(aliased), |root| move_keys(root, (None, "dev"), Some((None, "serve")), &Keys::Keep, &RULES));
        assert_eq!(
            renamed.unwrap(),
            pretty(serde_json::json!({"keys": {"global": {"cmd+r": "serve", "command+r": "lint"}}}))
        );
    }

    /// 删掉一条，其余的先后不变；删空了的那份一起拿掉。
    #[test]
    fn removes_a_task_keeping_the_order() {
        let existing = r#"{"global": {"a": "1", "b": "2", "c": "3"}, "projects": {"/w": {"x": "1"}}}"#;
        let updated = with_task(Some(existing), (None, "a"), None).unwrap();
        assert_eq!(
            updated,
            pretty(serde_json::json!({"global": {"b": "2", "c": "3"}, "projects": {"/w": {"x": "1"}}}))
        );
        let updated = with_task(Some(existing), (Some(Path::new("/w")), "x"), None).unwrap();
        assert_eq!(updated, pretty(serde_json::json!({"global": {"a": "1", "b": "2", "c": "3"}})));
    }

    /// 同一份里改名留在原来的位置，新名字别处已经有了时那一条让位；换到另一份时加在最后。
    #[test]
    fn renames_in_place_and_moves_between_scopes() {
        let existing = r#"{"global": {"a": "1", "b": "2", "c": "3"}}"#;
        let updated = with_task(Some(existing), (None, "b"), Some((None, "z", "9"))).unwrap();
        assert_eq!(updated, pretty(serde_json::json!({"global": {"a": "1", "z": "9", "c": "3"}})));
        let updated = with_task(Some(existing), (None, "c"), Some((None, "a", "9"))).unwrap();
        assert_eq!(updated, pretty(serde_json::json!({"global": {"b": "2", "a": "9"}})));
        let app = Some(Path::new("/w/app"));
        let updated = with_task(Some(existing), (None, "b"), Some((app, "b", "2"))).unwrap();
        assert_eq!(
            updated,
            pretty(serde_json::json!({"global": {"a": "1", "c": "3"}, "projects": {"/w/app": {"b": "2"}}}))
        );
    }
}
