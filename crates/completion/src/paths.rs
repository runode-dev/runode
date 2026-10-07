//! 规格里要文件或目录的参数：按当前词里已经写出的目录部分列出 shell 当前目录下对应目录的内容。

use std::path::{Path, PathBuf};

/// 一个目录最多列这么多项，免得巨大的目录拖慢按键。
const MAX_ENTRIES: usize = 5000;

/// 列出哪些项。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    All,
    Folders,
    /// 目录和可执行文件，补写成路径的命令名时用。
    Executables,
}

/// 列出的一项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

/// 当前词 `typed` 里目录部分（到最后一个 `/` 为止）的字符数，补全只换掉它后面的文件名。
pub fn dir_chars(typed: &str) -> usize {
    typed.rfind('/').map_or(0, |i| typed[..=i].chars().count())
}

/// 列出 `typed` 的目录部分指向的目录：相对路径从 `cwd` 算，`~` 开头的从 `home` 算。
/// 按 `filter` 挑选。隐藏文件只在要补的文件名以 `.` 开头时列出。按名字排序，不区分大小写。
pub fn list(typed: &str, cwd: &Path, home: Option<&Path>, filter: Filter) -> Vec<Entry> {
    let split = typed.rfind('/').map_or(0, |i| i + 1);
    let (dir, name) = typed.split_at(split);
    let Some(dir) = resolve_path(dir, Some(cwd), home) else {
        return Vec::new();
    };
    let show_hidden = name.starts_with('.');
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = read
        .filter_map(Result::ok)
        .take(MAX_ENTRIES)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if name.starts_with('.') && !show_hidden {
                return None;
            }
            // 指向目录的符号链接也算目录。
            let is_dir = match entry.file_type() {
                Ok(kind) if kind.is_symlink() => entry.path().is_dir(),
                Ok(kind) => kind.is_dir(),
                Err(_) => false,
            };
            let keep = match filter {
                Filter::All => true,
                Filter::Folders => is_dir,
                Filter::Executables => is_dir || is_executable(&entry.path()),
            };
            keep.then_some(Entry { name, is_dir })
        })
        .collect();
    entries.sort_by_cached_key(|entry| entry.name.to_lowercase());
    entries
}

/// 是不是自己能执行的普通文件（跟着符号链接走）。
pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// 词里写的路径对应的实际路径：`~` 和 `~/` 开头的从 `home` 算，`~user` 这样别人的主目录不处理，
/// 绝对路径原样返回，其余从 `cwd` 算；缺了要用的那个目录时是 `None`。
pub fn resolve_path(path: &str, cwd: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    match path.strip_prefix('~') {
        Some("") => home.map(Path::to_owned),
        Some(rest) => Some(home?.join(rest.strip_prefix('/')?)),
        None if path.starts_with('/') => Some(path.into()),
        None => Some(cwd?.join(path)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_home_absolute_and_relative_paths() {
        let (cwd, home) = (Some(Path::new("/w")), Some(Path::new("/h")));
        assert_eq!(resolve_path("~", cwd, home), Some("/h".into()));
        assert_eq!(resolve_path("~/x", cwd, home), Some("/h/x".into()));
        assert_eq!(resolve_path("~user/x", cwd, home), None);
        assert_eq!(resolve_path("/abs", None, None), Some("/abs".into()));
        assert_eq!(resolve_path("rel", cwd, None), Some("/w/rel".into()));
        assert_eq!(resolve_path("rel", None, home), None);
    }

    fn scratch() -> tempdir::Dir {
        let dir = tempdir::Dir::new("paths");
        std::fs::create_dir_all(dir.path().join("src/nested")).unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "").unwrap();
        std::fs::write(dir.path().join(".env"), "").unwrap();
        dir
    }

    #[test]
    fn lists_the_directory_part_of_the_word() {
        let dir = scratch();
        let names = |typed: &str, folders_only: bool| -> Vec<(String, bool)> {
            list(typed, dir.path(), None, if folders_only { Filter::Folders } else { Filter::All })
                .into_iter()
                .map(|e| (e.name, e.is_dir))
                .collect()
        };
        assert_eq!(names("", false), [("Cargo.toml".into(), false), ("src".into(), true)]);
        assert_eq!(names("Ca", false), [("Cargo.toml".into(), false), ("src".into(), true)]);
        assert_eq!(names("src/m", false), [("main.rs".into(), false), ("nested".into(), true)]);
        assert_eq!(names("", true), [("src".into(), true)]);
        // 以 `.` 开头时才列隐藏文件。
        assert_eq!(names(".", false).len(), 4);
        assert!(names("missing/", false).is_empty());
        assert_eq!(dir_chars("src/m"), 4);
        assert_eq!(dir_chars("m"), 0);
    }

    #[test]
    fn lists_executables_and_folders_for_commands() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch();
        let script = dir.path().join("run.sh");
        std::fs::write(&script, "").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let names: Vec<String> = list("", dir.path(), None, Filter::Executables).into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["run.sh", "src"]);
    }

    #[test]
    fn expands_the_home_directory() {
        let dir = scratch();
        let entries = list("~/src/", Path::new("/nonexistent"), Some(dir.path()), Filter::All);
        assert_eq!(entries.len(), 2);
        assert!(list("~/", Path::new("/"), None, Filter::All).is_empty());
        assert!(list("~other/", Path::new("/"), Some(dir.path()), Filter::All).is_empty());
    }

    /// 测试用的临时目录，用完删掉。
    mod tempdir {
        use std::path::{Path, PathBuf};

        pub struct Dir(PathBuf);

        impl Dir {
            pub fn new(name: &str) -> Self {
                use std::sync::atomic::{AtomicUsize, Ordering};
                static NEXT: AtomicUsize = AtomicUsize::new(0);
                let path = std::env::temp_dir().join(format!(
                    "runode-test-{name}-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                let _ = std::fs::remove_dir_all(&path);
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }

            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
