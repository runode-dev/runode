//! 读文件树要显示的目录。

use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
};

/// 只有一个子目录的目录最多连着并这么多层，防着指回上层的符号链接绕圈。
pub const MAX_COMPACT: usize = 16;

/// 目录里的一项。
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DirEntry {
    /// 原样的文件名：拼操作用的路径要用它，显示时才换成 `to_string_lossy`，免得非法 UTF-8 的名字指到别的文件上。
    #[cfg_attr(feature = "serde", serde(with = "name_bytes"))]
    pub name: OsString,
    pub is_dir: bool,
}

/// 文件名按字节序列化，哪个系统上都读得回：Unix 上是原样的字节；Windows 上读写都按 UTF-8，读到不合法的
/// 字节时换成替换字符。
#[cfg(feature = "serde")]
mod name_bytes {
    use std::ffi::OsString;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(name: &OsString, serializer: S) -> Result<S::Ok, S::Error> {
        #[cfg(unix)]
        let bytes = std::os::unix::ffi::OsStrExt::as_bytes(name.as_os_str());
        #[cfg(not(unix))]
        let text = name.to_string_lossy();
        #[cfg(not(unix))]
        let bytes = text.as_bytes();
        serializer.serialize_bytes(bytes)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<OsString, D::Error> {
        let bytes = Vec::<u8>::deserialize(deserializer)?;
        #[cfg(unix)]
        let name = std::os::unix::ffi::OsStringExt::from_vec(bytes);
        #[cfg(not(unix))]
        let name = OsString::from(String::from_utf8_lossy(&bytes).into_owned());
        Ok(name)
    }
}

/// 读到的一组目录：每个目录的内容，读不了的（删掉了、改了名）为 `None`。
pub type Listings = Vec<(PathBuf, Option<Vec<DirEntry>>)>;

/// `walk_files` 最多列这么多个文件，免得在家目录这种地方走个没完。
const MAX_WALK: usize = 50_000;

/// `root` 下的所有文件，相对 `root`：往下走，不跟符号链接，和 `read_dir` 一样跳过 `.git`、`.DS_Store`，最多
/// `MAX_WALK` 个。不在 git 仓库里时按名称找文件用它。
pub fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut dirs = vec![PathBuf::new()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = fs::read_dir(root.join(&dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if skipped(&name) {
                continue;
            }
            let path = dir.join(name);
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                dirs.push(path);
            } else {
                files.push(path);
                if files.len() >= MAX_WALK {
                    return files;
                }
            }
        }
    }
    files
}

/// 列目录时不列的：git 的数据目录和访达的 `.DS_Store`。
fn skipped(name: &OsStr) -> bool {
    name == ".git" || name == ".DS_Store"
}

/// 读一个目录，目录在前，各自按名字排，不分大小写；读不了时为空。`skipped` 的不列。
pub fn read_dir(dir: &Path) -> Option<Vec<DirEntry>> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            if skipped(&name) {
                return None;
            }
            // 指向目录的符号链接也当目录；只有符号链接才要再查一次。
            let is_dir = match entry.file_type() {
                Ok(kind) if kind.is_symlink() => fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir()),
                Ok(kind) => kind.is_dir(),
                Err(_) => false,
            };
            Some(DirEntry { name, is_dir })
        })
        .collect();
    let key = |entry: &DirEntry| entry.name.to_string_lossy().to_lowercase();
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| key(a).cmp(&key(b))));
    Some(entries)
}

/// 目录里只有一个子目录时是它的名字，文件树把这样的目录和子目录并成一行。
pub fn single_dir(entries: &[DirEntry]) -> Option<&OsStr> {
    match entries {
        [entry] if entry.is_dir => Some(&entry.name),
        _ => None,
    }
}

/// 读 `dirs` 这些目录，再往下多看一层：收起的子目录只有一个子目录时要和它并成一行，得先
/// 知道它里面有什么；这样的链条一直读到头。`ignored` 说是被忽略的子目录不往下看，里面多半很大。
pub fn list_dirs(dirs: Vec<PathBuf>, ignored: impl Fn(&Path) -> bool) -> Listings {
    let mut listed: HashSet<PathBuf> = dirs.iter().cloned().collect();
    let mut listings: Vec<_> = dirs
        .into_iter()
        .map(|dir| {
            let listing = read_dir(&dir);
            (dir, listing)
        })
        .collect();
    let children: Vec<_> = listings
        .iter()
        .flat_map(|(dir, listing)| {
            listing.iter().flatten().filter(|entry| entry.is_dir).map(|entry| dir.join(&entry.name))
        })
        .filter(|child| !ignored(child))
        .collect();
    for child in children {
        let mut next = Some(child);
        for _ in 0..MAX_COMPACT {
            let Some(dir) = next.take().filter(|dir| listed.insert(dir.clone())) else {
                break;
            };
            let listing = read_dir(&dir);
            next = listing.as_deref().and_then(single_dir).map(|name| dir.join(name));
            listings.push((dir, listing));
        }
    }
    listings
}
