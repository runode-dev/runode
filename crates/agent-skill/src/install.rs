//! 照 skills CLI 的布局装 skill：正本写在 `~/.agents/skills/<名字>/`，不读那里的 agent（`LINKS`）的目录里放
//! 指向正本的符号链接，Windows 上放一份副本。runode 写过的目录里留一份记录（`RECORD`），
//! `refresh` 只换有记录的。

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde_json::Value;

use crate::{BUNDLED, Env, Skill, State};

/// runode 往一个 skill 目录里写过哪些文件，一行一个相对路径；agent 只认 `SKILL.md`，不会读它。目录里可能还有
/// 不归 runode 的文件，记录只说明列着的那些。
const RECORD: &str = ".runode-files";
/// skills CLI 的 lock 文件里，从本仓库装的 skill 记的来源。
const NPX_SOURCE: &str = "runode-dev/runode";
/// 认得的 skills CLI lock 文件格式版本，别的版本不碰。
const NPX_LOCK_VERSION: u64 = 3;

/// 一个 skill 装完的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// 装的是哪个 skill。
    pub skill: &'static str,
    /// 正本目录，`~/.agents/skills/<名字>`。
    pub canonical: PathBuf,
    /// 正本是用户自己管的符号链接，没动它。
    pub left_as_is: bool,
    /// 用得上这个 skill 的 agent（显示名）和它那边的 skill 目录：新建或本来就有的指向正本的链接，或者副本。
    pub linked: Vec<(&'static str, PathBuf)>,
    /// 已经被别的东西占着、没动的位置：指向别处的链接、普通文件，或者没有 runode 的记录、已经有 `SKILL.md`
    /// 的目录。
    pub skipped: Vec<PathBuf>,
}

/// 不读 `~/.agents/skills` 的 agent：显示名，以及它装着时放 skill 的目录（装没装看它的配置目录在不在）。
struct AgentLink {
    agent: &'static str,
    skills_dir: fn(&Env, &Path) -> Option<PathBuf>,
}

const LINKS: &[AgentLink] = &[
    AgentLink {
        agent: "Claude Code",
        skills_dir: |env, home| {
            let config = env.claude_config.clone().unwrap_or_else(|| home.join(".claude"));
            config.is_dir().then(|| config.join("skills"))
        },
    },
    AgentLink {
        agent: "Antigravity",
        skills_dir: |_, home| {
            let gemini = home.join(".gemini");
            gemini.join("antigravity").is_dir().then(|| gemini.join("config").join("skills"))
        },
    },
    AgentLink {
        agent: "Kiro CLI",
        skills_dir: |_, home| {
            let kiro = home.join(".kiro");
            kiro.is_dir().then(|| kiro.join("skills"))
        },
    },
];

/// 一个位置上现在是什么，不跟着符号链接走。
enum Entry {
    Missing,
    Link,
    Dir,
    Other,
}

fn entry(path: &Path) -> io::Result<Entry> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Ok(Entry::Link),
        Ok(meta) if meta.is_dir() => Ok(Entry::Dir),
        Ok(_) => Ok(Entry::Other),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Entry::Missing),
        Err(err) => Err(err),
    }
}

fn canonical_dir(home: &Path, name: &str) -> PathBuf {
    home.join(".agents").join("skills").join(name)
}

fn no_home() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "no home directory")
}

/// 名字是 `name` 的 skill 在这台电脑上的状态；不是编进这一版的 skill 时 `current` 总是假。读不了正本时
/// 当作没装。
pub fn state(env: &Env, name: &str) -> State {
    let Some(home) = env.home.as_deref() else { return State::Missing };
    let dir = canonical_dir(home, name);
    match entry(&dir) {
        Ok(Entry::Link) if dir.join("SKILL.md").is_file() => State::Elsewhere,
        Ok(Entry::Dir) if dir.join("SKILL.md").is_file() => {
            let by_runode = dir.join(RECORD).is_file();
            let skill = BUNDLED.iter().find(|skill| skill.name == name);
            let current = skill.is_some_and(|skill| matches(&dir, skill).unwrap_or(false));
            State::Installed { by_runode, current }
        }
        _ => State::Missing,
    }
}

/// 把 `skill` 装给这台电脑上的 agent，可以重复执行。正本是符号链接时不动它，只补 agent 那边的链接；
/// 正本是别的工具装的目录时覆盖同名文件、写上记录接管过来，别的文件留着，并从 skills CLI 的 lock 文件里删掉
/// 这一项，`npx skills update` 才不把它换回去。
pub fn install(env: &Env, skill: &Skill) -> io::Result<Report> {
    let home = env.home.as_deref().ok_or_else(no_home)?;
    let report = place(env, home, skill)?;
    if !report.left_as_is {
        forget_npx_install(env, home, skill.name);
    }
    Ok(report)
}

/// 装上编进这一版的全部 skill（`BUNDLED`），每个一份 `Report`。一个装不上就停下，错误里带着是哪个 skill。
pub fn install_all(env: &Env) -> io::Result<Vec<Report>> {
    BUNDLED
        .iter()
        .map(|skill| {
            install(env, skill)
                .map_err(|err| io::Error::new(err.kind(), format!("cannot install the {} skill: {err}", skill.name)))
        })
        .collect()
}

/// 正本里有 runode 的记录、又和编进这一版的不一样（内容不同、缺了文件或文件变了）的 skill 重装一遍，
/// agent 那边的链接一起补齐，返回重装了没有。没装过的、别的工具装的不碰，不替用户装他没要的东西。
pub fn refresh(env: &Env) -> io::Result<bool> {
    let Some(home) = env.home.as_deref() else { return Ok(false) };
    let mut refreshed = false;
    for skill in BUNDLED {
        let dir = canonical_dir(home, skill.name);
        if !matches!(entry(&dir)?, Entry::Dir) {
            continue;
        }
        let Some(recorded) = recorded(&dir)? else { continue };
        if recorded.iter().eq(skill.files.iter().map(|(path, _)| path)) && matches(&dir, skill)? {
            continue;
        }
        place(env, home, skill)?;
        refreshed = true;
    }
    Ok(refreshed)
}

/// 写正本（不是符号链接时），再给 `LINKS` 里装着的 agent 放链接或副本。
fn place(env: &Env, home: &Path, skill: &Skill) -> io::Result<Report> {
    let canonical = canonical_dir(home, skill.name);
    let left_as_is = matches!(entry(&canonical)?, Entry::Link);
    if !left_as_is {
        write_skill(&canonical, skill)?;
    }
    let mut report = Report { skill: skill.name, canonical, left_as_is, linked: Vec::new(), skipped: Vec::new() };
    for link in LINKS {
        let Some(skills_dir) = (link.skills_dir)(env, home) else { continue };
        let target = skills_dir.join(skill.name);
        let found = entry(&target)?;
        // 指向正本的链接，或者 agent 的 skills 目录本身链到了 `~/.agents/skills`：`target` 就是正本，不能当副本删。
        if !matches!(found, Entry::Missing) && same_place(&target, &report.canonical) {
            report.linked.push((link.agent, target));
            continue;
        }
        match found {
            Entry::Missing => {
                fs::create_dir_all(&skills_dir)?;
                link_to(&report.canonical, &target, skill)?;
            }
            Entry::Link | Entry::Other => {
                report.skipped.push(target);
                continue;
            }
            // 没有记录、已经有 `SKILL.md` 的目录是用户自己放的或别的工具装的，覆盖会丢掉他改过的内容。
            Entry::Dir if recorded(&target)?.is_none() && target.join("SKILL.md").exists() => {
                report.skipped.push(target);
                continue;
            }
            // Windows 上的链接本来就是副本，覆盖写就是换新。
            Entry::Dir if !cfg!(windows) && remove_copy(&target)? => link_to(&report.canonical, &target, skill)?,
            Entry::Dir => write_skill(&target, skill)?,
        }
        report.linked.push((link.agent, target));
    }
    Ok(report)
}

#[cfg(not(windows))]
fn link_to(canonical: &Path, target: &Path, _: &Skill) -> io::Result<()> {
    std::os::unix::fs::symlink(canonical, target)
}

/// Windows 上建符号链接要开发者模式或管理员权限，放一份副本。
#[cfg(windows)]
fn link_to(_: &Path, target: &Path, skill: &Skill) -> io::Result<()> {
    write_skill(target, skill)
}

/// `dir` 里只有 runode 记录过的文件时，把它们连同记录和 `dir` 一起删掉，返回删没删。里面另有别的东西（用户
/// 自己放的文件、子目录、链接）或者没有记录时什么都不删。
fn remove_copy(dir: &Path) -> io::Result<bool> {
    let Some(recorded) = recorded(dir)? else { return Ok(false) };
    let recorded: Vec<&str> = recorded.iter().map(String::as_str).filter(|path| is_skill_file(path)).collect();
    if !holds_only(dir, "", &recorded)? {
        return Ok(false);
    }
    for path in &recorded {
        remove_if_exists(&dir.join(path))?;
    }
    remove_if_exists(&dir.join(RECORD))?;
    let _ = fs::remove_dir(dir.join("references"));
    // 查过之后又有人放了文件进来时删不掉，交给调用方按普通目录覆盖写。
    Ok(fs::remove_dir(dir).is_ok())
}

/// `dir`（`prefix` 是它相对 skill 目录的前缀）里除了记录本身和 `references/`，只有 `recorded` 列着的普通文件。
fn holds_only(dir: &Path, prefix: &str, recorded: &[&str]) -> io::Result<bool> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { return Ok(false) };
        let kind = entry.file_type()?;
        let ours = if kind.is_dir() {
            prefix.is_empty() && name == "references" && holds_only(&entry.path(), "references/", recorded)?
        } else {
            kind.is_file()
                && ((prefix.is_empty() && name == RECORD) || recorded.contains(&format!("{prefix}{name}").as_str()))
        };
        if !ours {
            return Ok(false);
        }
    }
    Ok(true)
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// 两个路径最终是不是同一个目录。
fn same_place(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// 把 `skill` 的文件写进 `dir`，换掉记录；上次记录里有、这次没有的文件删掉，空了的 `references/` 一起删。
/// 不在记录里的文件不动。
fn write_skill(dir: &Path, skill: &Skill) -> io::Result<()> {
    let old = recorded(dir)?;
    for (path, content) in skill.files {
        let file = dir.join(path);
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent)?;
        }
        runode_paths::replace_file(&file, content.as_bytes())?;
    }
    // 记录是盘上的文件，只认 skill 里的路径，不让它把删除引到 skill 目录外面。
    let gone = old.iter().flatten().filter(|old| is_skill_file(old) && skill.files.iter().all(|(path, _)| path != old));
    for path in gone {
        remove_if_exists(&dir.join(path))?;
        // 里面还有用户自己放的文件时删不掉，留着。
        if let Some((parent, _)) = path.rsplit_once('/') {
            let _ = fs::remove_dir(dir.join(parent));
        }
    }
    let record: String = skill.files.iter().map(|(path, _)| format!("{path}\n")).collect();
    runode_paths::replace_file(&dir.join(RECORD), record.as_bytes())
}

/// `dir` 里 runode 上次写了哪些文件，没有记录时为 `None`。
fn recorded(dir: &Path) -> io::Result<Option<Vec<String>>> {
    match fs::read_to_string(dir.join(RECORD)) {
        Ok(text) => Ok(Some(text.lines().map(str::to_owned).collect())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// `dir` 里 `skill` 的每个文件都在、字节都和编进去的一样。
fn matches(dir: &Path, skill: &Skill) -> io::Result<bool> {
    for (path, content) in skill.files {
        match fs::read(dir.join(path)) {
            Ok(existing) if existing == content.as_bytes() => {}
            Ok(_) => return Ok(false),
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(err),
        }
    }
    Ok(true)
}

/// `SKILL.md` 或 `references/<主题>.md`。
fn is_skill_file(path: &str) -> bool {
    path == "SKILL.md"
        || path
            .strip_prefix("references/")
            .and_then(|file| file.strip_suffix(".md"))
            .is_some_and(|topic| !topic.is_empty() && !topic.starts_with('.') && !topic.contains(['/', '\\']))
}

/// skills CLI 的 lock 文件：设了 `XDG_STATE_HOME` 时在它下面，否则在 `~/.agents` 下。
fn npx_lock_file(env: &Env, home: &Path) -> PathBuf {
    match &env.state_home {
        Some(state) => state.join("skills").join(".skill-lock.json"),
        None => home.join(".agents").join(".skill-lock.json"),
    }
}

/// 从 skills CLI 的 lock 文件里删掉从本仓库装的 `name`，别的内容和格式（两格缩进、结尾有没有换行）照旧。
/// 读不了、格式版本不认得、或者那一项不是从本仓库装的时不动。出错也不报：skill 已经装好了，lock 只管
/// `npx skills update` 会不会把它换回去。
fn forget_npx_install(env: &Env, home: &Path, name: &str) {
    let file = npx_lock_file(env, home);
    let Ok(text) = fs::read_to_string(&file) else { return };
    let Ok(mut lock) = serde_json::from_str::<Value>(&text) else { return };
    if lock.get("version").and_then(Value::as_u64) != Some(NPX_LOCK_VERSION) {
        return;
    }
    let Some(skills) = lock.get_mut("skills").and_then(Value::as_object_mut) else { return };
    if skills.get(name).and_then(|entry| entry.get("source")).and_then(Value::as_str) != Some(NPX_SOURCE) {
        return;
    }
    skills.shift_remove(name);
    let Ok(mut out) = serde_json::to_string_pretty(&lock) else { return };
    if text.ends_with('\n') {
        out.push('\n');
    }
    let _ = runode_paths::replace_file(&file, out.as_bytes());
}
