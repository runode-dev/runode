//! PATH 里的可执行文件，补命令名、给命令名上色时用。补命令名要整份列表：每个目录列一次就记下来，
//! 目录的修改时间变了（装了、删了命令）才重新列，可以先在后台列好（`warm_executables`）。判断
//! 一个名字是不是命令只看各目录下有没有这个文件，不列目录。

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, PoisonError},
    thread,
    time::SystemTime,
};

use crate::paths::is_executable;

/// 一个目录列出的可执行文件，以及列的时候目录的修改时间。
struct Listing {
    modified: Option<SystemTime>,
    names: Arc<Vec<String>>,
}

/// `path`（冒号分隔的目录）里所有可执行文件的名字，去重、按名字排好。
pub fn executables(path: &OsStr) -> Vec<String> {
    let mut names = BTreeSet::new();
    for dir in command_dirs(path) {
        names.extend(listing(dir).iter().cloned());
    }
    names.into_iter().collect()
}

/// 在后台线程里把 `path` 里的目录列好记下，之后 `executables` 只要核对各目录的修改时间。同一个
/// `path` 只列一次；之后目录变了照常由 `executables` 重新列。
pub fn warm_executables(path: &OsStr) {
    static WARMED: OnceLock<Mutex<HashSet<OsString>>> = OnceLock::new();
    let warmed = WARMED.get_or_init(Default::default);
    if !warmed.lock().unwrap_or_else(PoisonError::into_inner).insert(path.to_owned()) {
        return;
    }
    let path = path.to_owned();
    let spawned = thread::Builder::new().name("list-commands".into()).spawn(move || {
        for dir in command_dirs(&path) {
            listing(dir);
        }
    });
    if let Err(err) = spawned {
        tracing::debug!("failed to list the commands in the background: {err}");
    }
}

/// `path` 里有没有叫 `name` 的可执行文件。按顺序看各目录下有没有这个文件，找到就停，不列目录。
pub fn is_command(path: &OsStr, name: &str) -> bool {
    // 和 `list` 一样不算隐藏文件；带 `/` 的是路径，不是命令名。
    if name.is_empty() || name.starts_with('.') || name.contains('/') {
        return false;
    }
    command_dirs(path).any(|dir| is_executable(&dir.join(name)))
}

/// `path` 里找命令的目录：空的一项、`.` 和别的相对路径都相对于当前目录，那里的东西不算命令。
fn command_dirs(path: &OsStr) -> impl Iterator<Item = PathBuf> {
    std::env::split_paths(path).filter(|dir| dir.is_absolute())
}

/// 列过的目录。
fn cache() -> &'static Mutex<HashMap<PathBuf, Listing>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Listing>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// `dir` 里的可执行文件，按名字排好。每个目录列一次就记下来，修改时间变了才重新列。
fn listing(dir: PathBuf) -> Arc<Vec<String>> {
    let cache = cache();
    let modified = std::fs::metadata(&dir).and_then(|meta| meta.modified()).ok();
    let listed = {
        let cache = cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache.get(&dir).filter(|listing| listing.modified == modified).map(|listing| listing.names.clone())
    };
    if let Some(listed) = listed {
        return listed;
    }
    let listed = Arc::new(list(&dir));
    let listing = Listing { modified, names: listed.clone() };
    cache.lock().unwrap_or_else(PoisonError::into_inner).insert(dir, listing);
    listed
}

fn list(dir: &Path) -> Vec<String> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = read
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            (!name.starts_with('.') && is_executable(&entry.path())).then_some(name)
        })
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_executables_on_the_path_and_notices_changes() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("runode-commands-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let make = |name: &str, mode: u32| {
            let path = dir.join(name);
            std::fs::write(&path, "").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        make("zz-tool", 0o755);
        make("notes.txt", 0o644);
        let path = std::env::join_paths([dir.as_path(), Path::new("/bin")]).unwrap();
        let names = executables(&path);
        assert!(names.contains(&"zz-tool".to_owned()) && names.contains(&"sh".to_owned()));
        assert!(!names.contains(&"notes.txt".to_owned()));
        assert!(is_command(&path, "zz-tool") && !is_command(&path, "notes.txt") && !is_command(&path, "zz"));
        // 目录变了就重新列；修改时间的精度可能是秒，等它走过去。
        std::thread::sleep(std::time::Duration::from_millis(1100));
        make("aa-tool", 0o755);
        assert!(executables(&path).contains(&"aa-tool".to_owned()));
        // 同一个目录写成相对路径时不列。
        let cwd = std::env::current_dir().unwrap();
        let up = "../".repeat(cwd.components().count() - 1);
        let relative = Path::new(&up).join(dir.strip_prefix("/").unwrap());
        assert!(relative.is_relative() && relative.join("zz-tool").exists());
        assert!(executables(relative.as_os_str()).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn is_command_looks_up_the_name_without_listing() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("runode-is-command-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (first, second) = (dir.join("first"), dir.join("second"));
        for sub in [&first, &second] {
            std::fs::create_dir_all(sub).unwrap();
        }
        let make = |path: PathBuf, mode: u32| {
            std::fs::write(&path, "").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        make(second.join("zz-tool"), 0o755);
        make(second.join("notes.txt"), 0o644);
        make(second.join(".hidden"), 0o755);
        // 能进不能列的目录：列目录拿不到里面的东西，按名字看还看得到。
        std::fs::set_permissions(&second, std::fs::Permissions::from_mode(0o311)).unwrap();
        let path = std::env::join_paths([first.as_path(), second.as_path()]).unwrap();
        assert!(is_command(&path, "zz-tool"));
        let cached = |dir: &PathBuf| cache().lock().unwrap().contains_key(dir);
        assert!(!cached(&first) && !cached(&second), "is_command listed a directory");
        assert!(!executables(&path).contains(&"zz-tool".to_owned()));
        for name in ["notes.txt", ".hidden", "zz", "", ".", "..", "second/zz-tool", "../second/zz-tool"] {
            assert!(!is_command(&path, name), "{name:?} is not a command");
        }
        // 写成相对路径的目录不算。
        let relative = std::env::join_paths([Path::new("relative-dir")]).unwrap();
        assert!(!is_command(&relative, "zz-tool"));
        std::fs::set_permissions(&second, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn warm_executables_lists_in_the_background() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("runode-warm-commands-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let tool = dir.join("zz-warm");
        std::fs::write(&tool, "").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        warm_executables(dir.as_os_str());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let listed = loop {
            let listed = cache().lock().unwrap().get(&dir).map(|listing| listing.names.clone());
            if listed.is_some() || std::time::Instant::now() >= deadline {
                break listed;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(listed.as_deref().map(Vec::as_slice), Some(&["zz-warm".to_owned()][..]));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
