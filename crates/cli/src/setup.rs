//! 教 agent 用 runode：把操作别的终端的使用说明装成 agent 按需加载的 skill。Claude Code 读
//! `~/.claude/skills/<名字>/`，Codex 读 `~/.agents/skills/<名字>/`，两边是同样的文件：`SKILL.md`，
//! 加上它按需让 agent 去读的 `references/<主题>.md`。
//! 装的是 GitHub 上仓库 main 里最新的一套（`SKILLS_URL`）：先下文件清单 `files.txt`，再照着下每个
//! 文件，只改 skill 或加文件都不用发新版 app；下不到时用编进这一版的 `FILES`。装过哪些文件记在
//! skill 目录下的 `INSTALLED` 里，下到的清单里去掉的下次装时删掉。

use std::{
    io::{self, Read as _},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context as _, Result};

/// 给哪个 agent 装。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupTarget {
    Claude,
    Codex,
}

/// 一套 skill 文件：相对 skill 目录的路径（`<名字>/SKILL.md`、`<名字>/references/<主题>.md`）和内容。
/// `SKILL.md` 开头是 skill 的元数据（名字和什么时候用）；两个 agent 装的都是这些。
pub type SkillFiles = Vec<(String, String)>;

/// 编进这一版的 skill 文件，构建时照仓库里的清单 `skills/files.txt` 生成，顺序也一样。
pub(crate) const FILES: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/skill_files.rs"));
/// 仓库里放 skill 的目录在 GitHub 上的原始文件地址，下面是 `files.txt` 和清单里的各个文件。
pub const SKILLS_URL: &str = "https://raw.githubusercontent.com/runode-dev/runode/main/skills";
/// 下回来的一个文件最多这么大，再大就不是 skill。
const MAX_FILE_BYTES: usize = 256 * 1024;
/// 清单最多列这么多文件。
const MAX_FILES: usize = 64;
/// 下回来的清单最多这么大：`MAX_FILES` 个路径绰绰有余。
const MAX_LIST_BYTES: usize = 16 * 1024;
/// 上次装了哪些文件，一行一个路径，放在 skill 目录下；agent 只认 `<名字>/SKILL.md`，不会读它。
const INSTALLED: &str = ".runode-files";

fn skills_dir(target: SetupTarget, home: &Path) -> PathBuf {
    match target {
        SetupTarget::Claude => home.join(".claude/skills"),
        SetupTarget::Codex => home.join(".agents/skills"),
    }
}

/// 这一版往 `home` 下给 `target` 装哪些文件，和 `FILES` 一一对应；下到的清单可能多几个。
pub fn setup_paths(target: SetupTarget, home: &Path) -> Vec<PathBuf> {
    let dir = skills_dir(target, home);
    FILES.iter().map(|(path, _)| dir.join(path)).collect()
}

/// 编进这一版的 skill 文件。
pub fn bundled_skills() -> SkillFiles {
    FILES.iter().map(|(path, content)| ((*path).to_owned(), (*content).to_owned())).collect()
}

/// 从 `url`（通常是 `SKILLS_URL`）下最新的一套 skill 文件：先下清单 `files.txt`，再下清单里的每个。
/// 清单里有一个路径不对、一个文件下不到或不像 skill，就整套不用，返回 `None`，免得装上新旧混着的
/// 一套。外调 curl，清单下到后各个文件同时下，每个请求最多等十秒。
pub fn fetch_skills(url: &str) -> Option<SkillFiles> {
    let list = String::from_utf8(download(&format!("{url}/files.txt"), MAX_LIST_BYTES)?).ok()?;
    let paths: Vec<&str> = list.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    if paths.is_empty() || paths.len() > MAX_FILES || !paths.iter().all(|path| is_skill_path(path)) {
        return None;
    }
    let bodies: Vec<Option<Vec<u8>>> = std::thread::scope(|scope| {
        let downloads: Vec<_> =
            paths.iter().map(|path| scope.spawn(move || download(&format!("{url}/{path}"), MAX_FILE_BYTES))).collect();
        downloads.into_iter().map(|download| download.join().ok().flatten()).collect()
    });
    paths.into_iter().zip(bodies).map(|(path, body)| Some((path.to_owned(), as_skill_file(path, body?)?))).collect()
}

/// 下 `url`，最多收 `limit` 个字节：边读边数，超了就停掉 curl，不把整个回应攒在内存里再看大小。
fn download(url: &str, limit: usize) -> Option<Vec<u8>> {
    let mut child = Command::new("curl")
        .args(["--fail", "--silent", "--location", "--max-time", "10", "--proto", "=https", "--proto-redir", "=https"])
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut body = Vec::new();
    let read = child.stdout.take()?.take(limit as u64 + 1).read_to_end(&mut body);
    if read.is_err() || body.len() > limit {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    child.wait().ok()?.success().then_some(body)
}

/// 清单里的路径只能是 `<名字>/SKILL.md` 或 `<名字>/references/<主题>.md`，名字和主题只用小写字母、
/// 数字和 `-`：下回来的清单不能让文件写到 skill 目录外面去。
fn is_skill_path(path: &str) -> bool {
    let plain = |part: &str| {
        !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    match path.split('/').collect::<Vec<_>>()[..] {
        [name, "SKILL.md"] => plain(name),
        [name, "references", file] => plain(name) && file.strip_suffix(".md").is_some_and(plain),
        _ => false,
    }
}

/// 下回来的 `body` 像不像清单说的那个文件：不太大、是 UTF-8；`SKILL.md` 开头是这个名字的元数据，
/// reference 开头是标题。挡住错误页面和放错地方的文件。
fn as_skill_file(path: &str, body: Vec<u8>) -> Option<String> {
    let text = String::from_utf8(body).ok().filter(|text| text.len() <= MAX_FILE_BYTES)?;
    let looks_right = match path.strip_suffix("/SKILL.md") {
        Some(name) => text.starts_with(&format!("---\nname: {name}\n")),
        None => text.starts_with("# "),
    };
    looks_right.then_some(text)
}

/// 把 `files`（`fetch_skills` 或 `bundled_skills` 给的）装到 `home` 下 `target` 读的地方，返回写的
/// 文件。可以重复执行，每次整个换掉。`latest` 说 `files` 是不是下到的最新一套：是时上次装了、这次
/// 没有的文件删掉，免得去掉的 skill 还被 agent 加载；编进这一版的那套可能比 main 旧，没有的未必是
/// 去掉了，不删，留在记录里等下到最新的再说。
pub fn setup(target: SetupTarget, home: &Path, files: &[(String, String)], latest: bool) -> Result<Vec<PathBuf>> {
    let dir = skills_dir(target, home);
    let mut written = Vec::new();
    for (path, content) in files {
        let path = dir.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
        }
        runode_paths::replace_file(&path, content.as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))?;
        written.push(path);
    }
    let mut record: String = files.iter().map(|(path, _)| format!("{path}\n")).collect();
    // 记录是这台电脑上的文件，照样只认 skill 目录里的路径。
    let gone = installed_paths(&dir)?.into_iter().flatten().filter(|old| files.iter().all(|(path, _)| path != old));
    for old in gone.filter(|old| is_skill_path(old)) {
        if !latest {
            record.push_str(&format!("{old}\n"));
            continue;
        }
        let path = dir.join(&old);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).with_context(|| format!("failed to remove {}", path.display())),
        }
        // 空了的 `references/` 和 skill 目录一起删；里面还有用户自己放的文件时删不掉，留着。
        for parent in Path::new(&old).ancestors().skip(1).filter(|parent| !parent.as_os_str().is_empty()) {
            let _ = std::fs::remove_dir(dir.join(parent));
        }
    }
    let record_file = dir.join(INSTALLED);
    runode_paths::replace_file(&record_file, record.as_bytes())
        .with_context(|| format!("failed to write {}", record_file.display()))?;
    Ok(written)
}

/// 给 `target` 装过使用说明（`setup_paths` 或 `files` 里有文件在），又和 `files` 不一样、缺了文件
/// 或清单变了时重装，返回重装了没有。`files` 得是 `fetch_skills` 下到的，app 启动时用它，skill
/// 更新了不用手动再装；没装过的不装，不替用户装上他没要的东西，用户删掉了 skill 只剩记录的也算
/// 没装过。记录里的文件还在也算装过：从 main 装的、这一版没编进来的 skill 在 main 上去掉了，也要删掉。
pub fn refresh(target: SetupTarget, home: &Path, files: &[(String, String)]) -> Result<bool> {
    let dir = skills_dir(target, home);
    let recorded = installed_paths(&dir)?;
    let mut installed = setup_paths(target, home).iter().any(|path| path.exists())
        || recorded.iter().flatten().any(|path| is_skill_path(path) && dir.join(path).exists());
    let mut stale = recorded.is_some_and(|recorded| !recorded.iter().eq(files.iter().map(|(path, _)| path)));
    for (path, content) in files {
        let path = dir.join(path);
        match std::fs::read(&path) {
            Ok(existing) => {
                installed = true;
                stale |= existing != content.as_bytes();
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => stale = true,
            Err(err) => return Err(err).with_context(|| format!("failed to read {}", path.display())),
        }
    }
    if !(installed && stale) {
        return Ok(false);
    }
    setup(target, home, files, true)?;
    Ok(true)
}

/// `dir` 下上次装了哪些文件，没有记录时返回 `None`。
fn installed_paths(dir: &Path) -> Result<Option<Vec<String>>> {
    let file = dir.join(INSTALLED);
    match std::fs::read_to_string(&file) {
        Ok(text) => Ok(Some(text.lines().map(str::to_owned).collect())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("failed to read {}", file.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_paths_inside_a_skill_are_taken() {
        for path in ["runode/SKILL.md", "other-skill/SKILL.md", "runode/references/keys.md"] {
            assert!(is_skill_path(path), "{path}");
        }
        for path in [
            "../SKILL.md",
            "runode/../../x/SKILL.md",
            "/etc/SKILL.md",
            "runode/references/../SKILL.md",
            "runode/references/keys.txt",
            "runode/other/keys.md",
            "Runode/SKILL.md",
            "runode/references/.md",
            "SKILL.md",
        ] {
            assert!(!is_skill_path(path), "{path}");
        }
    }

    #[test]
    fn only_a_file_that_looks_right_is_taken() {
        let skill = FILES[0].1;
        assert_eq!(as_skill_file("runode/SKILL.md", skill.into()).as_deref(), Some(skill));
        assert_eq!(as_skill_file("other-skill/SKILL.md", skill.into()), None);
        assert_eq!(as_skill_file("runode/references/keys.md", b"# Keys\n".to_vec()).as_deref(), Some("# Keys\n"));
        assert_eq!(as_skill_file("runode/references/keys.md", b"<html>404</html>".to_vec()), None);
        assert_eq!(as_skill_file("runode/SKILL.md", vec![0xff, 0xfe]), None);
        let huge = format!("---\nname: runode\n{}", "x".repeat(MAX_FILE_BYTES));
        assert_eq!(as_skill_file("runode/SKILL.md", huge.into()), None);
    }

    #[test]
    fn every_skill_file_is_listed_and_every_link_resolves() {
        // skills/ 下的文件都在清单里，忘了列的装不上。
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills");
        let mut on_disk = Vec::new();
        let mut dirs = vec![root.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                // Finder 留下的 .DS_Store 这类隐藏文件不算。
                if name.starts_with('.') {
                    continue;
                }
                if path.is_dir() {
                    dirs.push(path);
                } else if name != "files.txt" {
                    on_disk.push(path.strip_prefix(&root).unwrap().to_string_lossy().into_owned());
                }
            }
        }
        let listed: Vec<&str> = FILES.iter().map(|(path, _)| *path).collect();
        for path in &on_disk {
            assert!(listed.contains(&path.as_str()), "skills/{path} is not in skills/files.txt");
        }
        assert!(listed.len() <= MAX_FILES);
        for (path, content) in FILES {
            assert!(is_skill_path(path), "{path}");
            assert!(as_skill_file(path, (*content).into()).is_some(), "{path}");
            // SKILL.md 让 agent 去读的 reference 都在清单里。
            let Some(name) = path.strip_suffix("/SKILL.md") else { continue };
            for (at, _) in content.match_indices("](references/") {
                let link = &content[at + 2..];
                let link = &link[..link.find(')').unwrap()];
                assert!(listed.contains(&format!("{name}/{link}").as_str()), "{path} links to {link}");
            }
        }
    }

    #[test]
    fn each_skill_has_its_name_and_a_description() {
        for &(path, skill) in FILES {
            let Some(name) = path.strip_suffix("/SKILL.md") else { continue };
            assert!(skill.starts_with(&format!("---\nname: {name}\ndescription: ")), "{name}");
            // Codex 和 skill 规范都不认超过 1024 个字符的描述，整个 skill 不会被加载。
            let description = skill.lines().nth(2).unwrap().trim_start_matches("description: ");
            assert!(description.chars().count() <= 1024, "{name}: {}", description.chars().count());
        }
    }
}
