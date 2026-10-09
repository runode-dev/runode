//! Claude Code 的用量：`runode setup usage` 把 `~/.claude/settings.json` 的 `statusLine` 换成
//! `usage::hook_command`，Claude Code 每次更新状态栏时把会话的模型、上下文和花费编成 JSON 从标准输入
//! 交过来。
//! 这里报给宿主，再接着跑换下来的原命令（存在 `Dirs::claude_statusline_file`），Claude Code 的状态栏
//! 照旧显示原命令的输出。

use std::{
    io::{self, Read as _, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context as _, Result, anyhow};
use runode_shared_types::agent::AgentUsage;
use serde_json::Value;

use crate::{
    Env,
    usage::{Agent, hook_command, is_ours, object_mut, quote, read_object, report, write_object},
};

/// 读标准输入，报给宿主，再跑原命令。
pub(crate) fn run(env: &Env, out: &mut dyn Write) -> Result<()> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    if let Ok(json) = serde_json::from_str::<Value>(&input) {
        let _ = report(env, usage(&json));
    }
    let chained = env.dirs.claude_statusline_file().and_then(|file| std::fs::read_to_string(file).ok());
    if let Some(command) = chained.filter(|command| !command.trim().is_empty()) {
        out.write_all(&run_chained(&command, &input)?)?;
    }
    Ok(())
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

/// `install` 改的 settings 文件。
pub(crate) fn settings_path(home: &Path) -> PathBuf {
    home.join(".claude/settings.json")
}

/// 把 `statusLine` 换成 runode 的命令，原来的命令存进 `chain`，返回改的 settings 文件。不在 runode 里
/// 时这条命令直接跑 `chain`。原来那条已经是 runode 的时只换成这次的写法，存着的原命令不动。settings 里
/// 别的内容不动。
pub(crate) fn install(home: &Path, chain: &Path) -> Result<PathBuf> {
    let path = settings_path(home);
    let mut settings = read_object(&path)?;
    let line = object_mut(settings.entry("statusLine").or_insert(Value::Null));
    let previous = line.get("command").and_then(Value::as_str).filter(|command| !command.trim().is_empty());
    match previous.map(str::to_owned) {
        Some(command) if is_ours(&command, Agent::Claude) => {}
        Some(command) => {
            if let Some(dir) = chain.parent() {
                std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
            }
            std::fs::write(chain, command).with_context(|| format!("failed to write {}", chain.display()))?;
        }
        None => match std::fs::remove_file(chain) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => {
                return Err(err).with_context(|| format!("failed to remove {}", chain.display()));
            }
            _ => {}
        },
    }
    line.insert("type".into(), "command".into());
    let previous = format!("[ -f {chain} ] && exec /bin/sh {chain}", chain = quote(chain));
    line.insert("command".into(), hook_command(Agent::Claude, Some(&previous)).into());
    write_object(&path, settings)?;
    Ok(path)
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
}
