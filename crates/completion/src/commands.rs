//! PATH 里的可执行文件，补命令名时用。每个目录列一次就记下来，目录的修改时间变了（装了、
//! 删了命令）才重新列。

use std::{
    collections::{BTreeSet, HashMap},
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, PoisonError},
    time::SystemTime,
};

/// 一个目录列出的可执行文件，以及列的时候目录的修改时间。
struct Listing {
    modified: Option<SystemTime>,
    names: Arc<Vec<String>>,
}

/// `path`（冒号分隔的目录）里所有可执行文件的名字，去重、按名字排好。
pub fn executables(path: &OsStr) -> Vec<String> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Listing>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let mut names = BTreeSet::new();
    for dir in std::env::split_paths(path) {
        // 空的一项、`.` 和别的相对路径都相对于当前目录，那里的东西不算命令。
        if dir.as_os_str().is_empty() || !dir.is_absolute() {
            continue;
        }
        let modified = std::fs::metadata(&dir).and_then(|meta| meta.modified()).ok();
        let listed = {
            let cache = cache.lock().unwrap_or_else(PoisonError::into_inner);
            cache.get(&dir).filter(|listing| listing.modified == modified).map(|listing| listing.names.clone())
        };
        let listed = match listed {
            Some(listed) => listed,
            None => {
                let listed = Arc::new(list(&dir));
                let listing = Listing { modified, names: listed.clone() };
                cache.lock().unwrap_or_else(PoisonError::into_inner).insert(dir, listing);
                listed
            }
        };
        names.extend(listed.iter().cloned());
    }
    names.into_iter().collect()
}

fn list(dir: &Path) -> Vec<String> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    read.filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            (!name.starts_with('.') && super::paths::is_executable(&entry.path())).then_some(name)
        })
        .collect()
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
}
