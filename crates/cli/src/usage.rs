//! agent 把自己的模型和用量报给 runode，界面显示在分屏底下（`SessionMeta::agent_usage`）。
//!
//! `runode setup usage` 让装着的 agent 都调 `runode usage-hook <agent>`：Claude Code 当作状态栏
//! 命令（`claude`），Codex 和 Gemini CLI 当作钩子（`codex`、`gemini`），pi 经它的扩展（`pi`）。它从
//! 标准输入读 agent 交来的 JSON 报给宿主（`ClientMsg::AgentUsage`），报不成时不出声。
//!
//! 写进 agent 配置的命令（`hook_command`）先看自己在不在 runode 的终端里（有 app 设的 `RUNODE_BIN`）：
//! 在就交给 `"$RUNODE_BIN" usage-hook <agent>`；不在就不起 runode，读完输入就走，只替 Claude Code
//! 跑它原来的状态栏命令。命令里不写 runode 的路径，app 挪了位置不用重装，Codex 也不用重新信任钩子。

use std::{
    fs::File,
    io::{self, Read as _, Seek as _, SeekFrom},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, anyhow, bail};
use runode_protocol::{ClientMsg, SessionId};
use runode_shared_types::agent::AgentUsage;
use serde_json::{Map, Value};

use crate::{Env, client::Connection};

pub(crate) mod claude;
pub(crate) mod codex;
pub(crate) mod gemini;
pub(crate) mod pi;

/// 报用量的 agent。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Agent {
    Claude,
    Codex,
    Gemini,
    Pi,
}

impl Agent {
    const ALL: [Self; 4] = [Self::Claude, Self::Codex, Self::Gemini, Self::Pi];

    /// 命令行和脚本里的写法。
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Pi => "pi",
        }
    }

    pub(crate) fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|agent| agent.name() == name)
    }
}

/// `runode usage-hook <agent>`：读 agent 交来的 JSON，报给宿主。
pub(crate) fn run(agent: Agent, env: &Env, out: &mut dyn std::io::Write) -> Result<()> {
    match agent {
        Agent::Claude => claude::run(env, out)?,
        Agent::Codex => codex::run(env),
        Agent::Gemini => gemini::run(env),
        Agent::Pi => pi::run(env),
    }
    Ok(())
}

/// 给家目录下装着的 agent（有 `~/.claude` 的 Claude Code、有 `~/.codex` 的 Codex、有 `~/.gemini` 的
/// Gemini CLI、有 `~/.pi` 的 pi）接上用量，返回改的文件。可以重复执行，见各自的 `install`。一个都没装
/// 时出错，什么都不写。
pub fn setup_usage(dirs: &runode_paths::Dirs) -> Result<Vec<PathBuf>> {
    let home = dirs.home.as_deref().ok_or_else(|| anyhow!("cannot tell where your home directory is"))?;
    let installed = |dir: &str| home.join(dir).is_dir();
    if ![".claude", ".codex", ".gemini", ".pi"].iter().any(|dir| installed(dir)) {
        bail!("found none of Claude Code (~/.claude), Codex (~/.codex), Gemini CLI (~/.gemini) and pi (~/.pi)");
    }
    let mut changed = Vec::new();
    if installed(".claude") {
        let chain = dirs.claude_statusline_file().ok_or_else(|| anyhow!("cannot tell where runode keeps its data"))?;
        changed.push(claude::install(home, &chain)?);
    }
    if installed(".codex") {
        changed.push(codex::install(home)?);
    }
    if installed(".gemini") {
        changed.push(gemini::install(home)?);
    }
    if installed(".pi") {
        changed.push(pi::install(home)?);
    }
    Ok(changed)
}

/// `setup_usage` 会改的文件，按家目录下现在装着哪些 agent。
pub fn usage_paths(home: &Path) -> Vec<PathBuf> {
    [
        (".claude", claude::settings_path(home)),
        (".codex", codex::hooks_path(home)),
        (".gemini", gemini::settings_path(home)),
        (".pi", pi::extension_path(home)),
    ]
    .into_iter()
    .filter(|(dir, _)| home.join(dir).is_dir())
    .map(|(_, path)| path)
    .collect()
}

/// 从会话记录末尾往前读这么多，找最近的用量。
// shortcut: 只看最后 256 KiB，最近一条用量之后的输出比这还长时这次报不出用量、等下一次；真碰到了再改成
// 按块往前读。
const TAIL_BYTES: u64 = 256 * 1024;

/// agent 从标准输入交来的 JSON；读不了、不是 JSON 时为 `None`。钩子不能因此失败：Gemini 把退出码
/// 不是 0 的钩子当成警告甚至拦下这一轮。
pub(crate) fn read_stdin() -> Option<Value> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input).ok()?;
    serde_json::from_str(&input).ok()
}

/// 会话记录（JSONL）末尾 `TAIL_BYTES` 里从后往前的各行。
pub(crate) fn tail_lines(path: &Path) -> Vec<String> {
    let read = || -> io::Result<Vec<u8>> {
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(file.metadata()?.len().saturating_sub(TAIL_BYTES)))?;
        let mut tail = Vec::new();
        file.read_to_end(&mut tail)?;
        Ok(tail)
    };
    let tail = read().unwrap_or_default();
    String::from_utf8_lossy(&tail).lines().rev().map(str::to_owned).collect()
}

/// 在 runode 的终端里时把 `usage` 报给自己所在的会话；不在时什么都不做。
pub(crate) fn report(env: &Env, usage: AgentUsage) -> Result<()> {
    let Some(id) = env.session.as_deref().and_then(|own| own.parse::<SessionId>().ok()) else {
        return Ok(());
    };
    let connection = Connection::open(env)?;
    let req = connection.req();
    connection.request_done(&ClientMsg::AgentUsage { req, id, usage })
}

/// 放进 sh 单引号里的 `path`。
pub(crate) fn quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"))
}

/// 写进 agent 配置的 sh 命令，见模块文档；不在 runode 里时先跑 `elsewhere`（同样是 sh），再读完输入。
/// `RUNODE_BIN` 指的文件不在了（app 挪走了而 shell 还是旧的）时当作不在 runode 里。最后总是成功退出：
/// Gemini 把退出码不是 0 的钩子当成警告甚至拦下这一轮。
pub(crate) fn hook_command(agent: Agent, elsewhere: Option<&str>) -> String {
    let report = format!("[ -x \"${{RUNODE_BIN-}}\" ] && exec \"$RUNODE_BIN\" usage-hook {}", agent.name());
    match elsewhere {
        Some(elsewhere) => format!("{report}; {elsewhere}; cat >/dev/null"),
        None => format!("{report}; cat >/dev/null"),
    }
}

/// `command` 是不是 runode 给 `agent` 写的 `hook_command`。
pub(crate) fn is_ours(command: &str, agent: Agent) -> bool {
    command.contains(&format!("usage-hook {};", agent.name()))
}

/// 在 `hooks` 里（Codex 和 Gemini 的钩子配置，`{ 事件: [{ hooks: [{ type, command, … }] }] }`）
/// 给 `events` 每个事件的末尾挂上 `hook`（它的 `command` 是给 `agent` 的 `hook_command`）。已经挂着时
/// 只换掉那一条，别的钩子不动，也不挪动位置。
pub(crate) fn add_hooks(hooks: &mut Map<String, Value>, events: &[&str], agent: Agent, hook: Value) {
    for event in events {
        let groups = hooks.entry(*event).or_insert_with(|| Value::Array(Vec::new()));
        if !groups.is_array() {
            *groups = Value::Array(Vec::new());
        }
        let groups = groups.as_array_mut().expect("just made an array");
        let existing =
            groups.iter_mut().filter_map(|group| group.get_mut("hooks")?.as_array_mut()).flatten().find(|hook| {
                hook.get("command").and_then(Value::as_str).is_some_and(|command| is_ours(command, agent))
            });
        match existing {
            Some(existing) => *existing = hook.clone(),
            None => groups.push(serde_json::json!({ "hooks": [hook.clone()] })),
        }
    }
}

/// 读一个 JSON 对象文件，没有时当空对象。
pub(crate) fn read_object(path: &Path) -> Result<Map<String, Value>> {
    let value = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str::<Value>(&text).with_context(|| format!("cannot parse {}", path.display()))?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(err) => return Err(err).with_context(|| format!("failed to read {}", path.display())),
    };
    match value {
        Value::Object(object) => Ok(object),
        _ => bail!("{} is not a JSON object", path.display()),
    }
}

/// 按缩进写回 JSON 对象文件，键的先后不变。
pub(crate) fn write_object(path: &Path, object: Map<String, Value>) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    }
    let mut text = serde_json::to_string_pretty(&Value::Object(object))?;
    text.push('\n');
    std::fs::write(path, text).with_context(|| format!("failed to write {}", path.display()))
}

/// 把 `value` 换成对象（不是对象时换成空对象），返回它。
pub(crate) fn object_mut(value: &mut Value) -> &mut Map<String, Value> {
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
    value.as_object_mut().expect("just made an object")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_reports_only_inside_runode() {
        let command = hook_command(Agent::Codex, None);
        assert_eq!(command, r#"[ -x "${RUNODE_BIN-}" ] && exec "$RUNODE_BIN" usage-hook codex; cat >/dev/null"#);
        assert!(is_ours(&command, Agent::Codex));
        assert!(!is_ours(&command, Agent::Gemini));
        assert!(!is_ours("theirs.sh codex", Agent::Codex));
        assert_eq!(quote(Path::new("/it's")), r"'/it'\''s'");
        assert_eq!(Agent::parse("gemini"), Some(Agent::Gemini));
        assert_eq!(Agent::parse("vim"), None);
    }
}
