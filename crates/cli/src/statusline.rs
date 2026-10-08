//! Claude Code 的状态栏：`runode setup statusline` 把 `~/.claude/settings.json` 的 `statusLine`
//! 换成 `runode statusline`，Claude Code 每次更新状态栏时把会话的模型、上下文和花费编成 JSON
//! 从标准输入交给它。在 runode 的终端里时它报给宿主（`ClientMsg::AgentUsage`），界面显示在分屏
//! 底下；之后接着跑换下来的原命令（存在 `Dirs::claude_statusline_file`），Claude Code 的状态栏
//! 照旧显示原命令的输出。

use std::{
    io::{self, Read as _, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context as _, Result, anyhow};
use runode_protocol::{ClientMsg, SessionId};
use runode_shared_types::agent::AgentUsage;
use serde_json::{Map, Value};

use crate::{Env, client::Connection};

/// `statusLine` 命令里认自己的记号：`setup` 再装时据此知道原来那条已经是 runode 的。
const MARK: &str = "RUNODE_BIN";

/// `runode statusline`：读标准输入，报给宿主，再跑原命令。报不成（不在 runode 的终端里、app 没在
/// 跑）时不出声：状态栏上不该冒出错误。
pub(crate) fn run(env: &Env, out: &mut dyn Write) -> Result<()> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    if let Some(id) = env.session.as_deref().and_then(|own| own.parse::<SessionId>().ok())
        && let Ok(json) = serde_json::from_str::<Value>(&input)
    {
        let _ = report(env, id, usage(&json));
    }
    let chained = env.dirs.claude_statusline_file().and_then(|file| std::fs::read_to_string(file).ok());
    if let Some(command) = chained.filter(|command| !command.trim().is_empty()) {
        out.write_all(&run_chained(&command, &input)?)?;
    }
    Ok(())
}

fn report(env: &Env, id: SessionId, usage: AgentUsage) -> Result<()> {
    let connection = Connection::open(env)?;
    let req = connection.req();
    connection.request_done(&ClientMsg::AgentUsage { req, id, usage })
}

/// 从 Claude Code 交来的 JSON 里取出界面要显示的几项。
pub(crate) fn usage(json: &Value) -> AgentUsage {
    let int = |pointer: &str| json.pointer(pointer).and_then(Value::as_u64);
    let float = |pointer: &str| json.pointer(pointer).and_then(Value::as_f64).filter(|value| *value >= 0.);
    let context = ["input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"]
        .iter()
        .map(|key| int(&format!("/context_window/current_usage/{key}")))
        .try_fold(0, |sum, tokens| tokens.map(|tokens| sum + tokens));
    AgentUsage {
        model: json.pointer("/model/display_name").and_then(Value::as_str).map(str::to_owned),
        context_tokens: context,
        cache_read_tokens: int("/context_window/current_usage/cache_read_input_tokens"),
        cache_write_tokens: int("/context_window/current_usage/cache_creation_input_tokens"),
        output_tokens: int("/context_window/current_usage/output_tokens"),
        context_window: int("/context_window/context_window_size"),
        cost_micro_usd: float("/cost/total_cost_usd").map(|usd| (usd * 1e6).round() as u64),
        five_hour_percent: float("/rate_limits/five_hour/used_percentage")
            .map(|percent| percent.min(100.).round() as u8),
    }
}

/// 用 sh 跑原命令，`input` 原样交给它的标准输入，返回它的标准输出。
fn run_chained(command: &str, input: &str) -> Result<Vec<u8>> {
    let mut child = Command::new("/bin/sh")
        .args(["-c", command])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("failed to run the previous status line command")?;
    let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin for the previous status line command"))?;
    let input = input.to_owned();
    // 另起线程写，原命令不读标准输入时也不会卡住。
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child.wait_with_output()?;
    let _ = writer.join();
    Ok(output.stdout)
}

/// `setup_statusline` 改的 settings 文件。
pub fn statusline_settings_path(home: &Path) -> PathBuf {
    home.join(".claude/settings.json")
}

/// 把家目录下 Claude Code 的 `statusLine` 换成用 `exe` 跑的 `runode statusline`，原来的命令存进
/// `Dirs::claude_statusline_file`，返回改的 settings 文件。可以重复执行：原来那条已经是 runode 的时
/// 只更新可执行文件的路径，存着的原命令不动。settings 里别的内容不动。
pub fn setup_statusline(dirs: &runode_paths::Dirs, exe: &Path) -> Result<PathBuf> {
    let home = dirs.home.as_deref().ok_or_else(|| anyhow!("cannot tell where your home directory is"))?;
    let chain = dirs.claude_statusline_file().ok_or_else(|| anyhow!("cannot tell where runode keeps its data"))?;
    if let Some(dir) = chain.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    }
    let chain = chain.as_path();
    let path = statusline_settings_path(home);
    let mut settings = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str::<Value>(&text).with_context(|| format!("cannot parse {}", path.display()))?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => Value::Object(Map::new()),
        Err(err) => return Err(err).with_context(|| format!("failed to read {}", path.display())),
    };
    let object = settings.as_object_mut().ok_or_else(|| anyhow!("{} is not a JSON object", path.display()))?;
    let line = object.entry("statusLine").or_insert_with(|| Value::Object(Map::new()));
    if !line.is_object() {
        *line = Value::Object(Map::new());
    }
    let line = line.as_object_mut().expect("just made an object");
    let previous = line.get("command").and_then(Value::as_str).filter(|command| !command.trim().is_empty());
    match previous.map(str::to_owned) {
        Some(command) if command.contains(MARK) && command.ends_with(" statusline") => {}
        Some(command) => {
            std::fs::write(chain, command).with_context(|| format!("failed to write {}", chain.display()))?
        }
        None => match std::fs::remove_file(chain) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => {
                return Err(err).with_context(|| format!("failed to remove {}", chain.display()));
            }
            _ => {}
        },
    }
    line.insert("type".into(), "command".into());
    line.insert("command".into(), command(exe).into());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    }
    let mut text = serde_json::to_string_pretty(&settings)?;
    text.push('\n');
    std::fs::write(&path, text).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

/// 写进 `statusLine` 的命令：在 runode 的终端里用 app 设的 `RUNODE_BIN`，别处用装的时候的 `exe`。
fn command(exe: &Path) -> String {
    let exe = exe.to_string_lossy();
    // 放在双引号里，这几个字符要转义。
    let mut quoted = String::new();
    for c in exe.chars() {
        if matches!(c, '\\' | '"' | '$' | '`') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    format!("\"${{{MARK}:-{quoted}}}\" statusline")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_takes_the_model_the_context_and_the_cost() {
        let json = serde_json::json!({
            "model": { "id": "claude-opus-5-5", "display_name": "Opus" },
            "cost": { "total_cost_usd": 0.01234 },
            "context_window": {
                "context_window_size": 200000,
                "current_usage": {
                    "input_tokens": 8500,
                    "output_tokens": 1200,
                    "cache_creation_input_tokens": 5000,
                    "cache_read_input_tokens": 2000
                }
            },
            "rate_limits": { "five_hour": { "used_percentage": 23.5 } }
        });
        assert_eq!(
            usage(&json),
            AgentUsage {
                model: Some("Opus".into()),
                context_tokens: Some(15500),
                cache_read_tokens: Some(2000),
                cache_write_tokens: Some(5000),
                output_tokens: Some(1200),
                context_window: Some(200000),
                cost_micro_usd: Some(12340),
                five_hour_percent: Some(24),
            }
        );
        // 会话刚开始还没有请求时 `current_usage` 是 null。
        let fresh = serde_json::json!({ "context_window": { "current_usage": null } });
        assert_eq!(usage(&fresh), AgentUsage::default());
    }

    #[test]
    fn the_command_quotes_the_path() {
        assert_eq!(
            command(Path::new("/Applications/runode.app/x")),
            "\"${RUNODE_BIN:-/Applications/runode.app/x}\" statusline"
        );
        assert_eq!(command(Path::new("/a\"$b")), "\"${RUNODE_BIN:-/a\\\"\\$b}\" statusline");
    }
}
