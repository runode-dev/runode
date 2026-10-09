//! Codex 的用量：`runode setup usage` 在 `~/.codex/hooks.json` 的 `SessionStart`、`PostToolUse` 和
//! `Stop` 末尾各挂一条调 runode 的钩子（`usage::hook_command`）。Codex 跑钩子时把会话的 JSON（含 `model` 和会话记录的
//! `transcript_path`）从标准输入交过来，这里从会话记录末尾找最近一条 `token_count` 事件，连同模型报给
//! 宿主。什么都不输出：`Stop` 钩子的输出 Codex 会当成要不要接着干的决定来读。
//!
//! 新加或改过的钩子 Codex 要用户先信任（它在 `config.toml` 的 `hooks.state` 里按钩子的位置记着），
//! 所以只往末尾加，不挪动已有钩子的位置。

use std::path::{Path, PathBuf};

use anyhow::Result;
use runode_shared_types::agent::AgentUsage;
use serde_json::{Value, json};

use crate::{
    Env,
    usage::{Agent, add_hooks, hook_command, object_mut, read_object, read_stdin, report, tail_lines, write_object},
};

/// 挂在哪些事件上：会话开始时先报模型，每次用完工具和每轮结束时报用量。
const EVENTS: [&str; 3] = ["SessionStart", "PostToolUse", "Stop"];
/// 钩子的超时，秒。
const TIMEOUT_SECS: u64 = 5;

/// 读标准输入，报给宿主。
pub(crate) fn run(env: &Env) {
    let Some(json) = read_stdin() else { return };
    let event = json.get("transcript_path").and_then(Value::as_str).and_then(|path| last_token_count(Path::new(path)));
    let _ = report(env, usage(json.get("model").and_then(Value::as_str), event.as_ref()));
}

/// 会话记录里最近一条 `token_count` 事件的 `payload`。
pub(crate) fn last_token_count(path: &Path) -> Option<Value> {
    tail_lines(path).into_iter().filter(|line| line.contains("\"token_count\"")).find_map(|line| {
        let mut event = serde_json::from_str::<Value>(&line).ok()?;
        (event.pointer("/payload/type")?.as_str()? == "token_count").then(|| event["payload"].take())
    })
}

/// 从模型名和 `token_count` 事件里取出界面要显示的几项。Codex 的输入 token 已经含着读缓存的那部分，
/// 不报花费。
pub(crate) fn usage(model: Option<&str>, event: Option<&Value>) -> AgentUsage {
    let int = |pointer: &str| event.and_then(|event| event.pointer(pointer)).and_then(Value::as_u64);
    let last = |key: &str| int(&format!("/info/last_token_usage/{key}"));
    // 限额有主次两个窗口，哪个是五小时的看 `window_minutes`。
    let five_hour = event.and_then(|event| {
        ["primary", "secondary"].iter().find_map(|window| {
            let window = event.pointer(&format!("/rate_limits/{window}"))?;
            (window.get("window_minutes")?.as_u64()? == 300).then(|| window.get("used_percent")?.as_f64()).flatten()
        })
    });
    AgentUsage {
        model: model.map(str::to_owned),
        context_tokens: last("input_tokens"),
        cache_read_tokens: last("cached_input_tokens"),
        cache_write_tokens: last("cache_write_input_tokens"),
        output_tokens: last("output_tokens"),
        context_window: int("/info/model_context_window"),
        cost_micro_usd: None,
        five_hour_percent: five_hour.filter(|percent| *percent >= 0.).map(|percent| percent.min(100.).round() as u8),
    }
}

/// `install` 改的钩子文件。
pub(crate) fn hooks_path(home: &Path) -> PathBuf {
    home.join(".codex/hooks.json")
}

/// 在 `EVENTS` 每个事件的末尾挂上 runode 的钩子，返回改的钩子文件。
pub(crate) fn install(home: &Path) -> Result<PathBuf> {
    let path = hooks_path(home);
    let mut file = read_object(&path)?;
    let hook = json!({ "type": "command", "command": hook_command(Agent::Codex, None), "timeout": TIMEOUT_SECS });
    add_hooks(object_mut(file.entry("hooks").or_insert(Value::Null)), &EVENTS, Agent::Codex, hook);
    write_object(&path, file)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_takes_the_last_request_and_the_five_hour_window() {
        let event = json!({
            "type": "token_count",
            "info": {
                "total_token_usage": { "input_tokens": 229852, "output_tokens": 604 },
                "last_token_usage": {
                    "input_tokens": 40112,
                    "cached_input_tokens": 39680,
                    "cache_write_input_tokens": 0,
                    "output_tokens": 28
                },
                "model_context_window": 258400
            },
            "rate_limits": {
                "primary": { "used_percent": 12.4, "window_minutes": 300 },
                "secondary": { "used_percent": 3.0, "window_minutes": 10080 }
            }
        });
        assert_eq!(
            usage(Some("gpt-6-luna"), Some(&event)),
            AgentUsage {
                model: Some("gpt-6-luna".into()),
                context_tokens: Some(40112),
                cache_read_tokens: Some(39680),
                cache_write_tokens: Some(0),
                output_tokens: Some(28),
                context_window: Some(258400),
                cost_micro_usd: None,
                five_hour_percent: Some(12),
            }
        );
        // 会话刚开始还没有会话记录：只有模型。
        assert_eq!(
            usage(Some("gpt-6-luna"), None),
            AgentUsage { model: Some("gpt-6-luna".into()), ..AgentUsage::default() }
        );
    }

    #[test]
    fn the_last_token_count_is_found_from_the_end() {
        let path = std::env::temp_dir().join(format!("runode-codex-rollout-{}.jsonl", std::process::id()));
        let lines = [
            r#"{"type":"session_meta","payload":{"id":"x"}}"#,
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"model_context_window":1}}}"#,
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"model_context_window":2}}}"#,
            r#"{"type":"response_item","payload":{"type":"message","content":"\"token_count\""}}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();
        let found = last_token_count(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(found.unwrap().pointer("/info/model_context_window"), Some(&json!(2)));
    }
}
