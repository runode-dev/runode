//! Gemini CLI 的用量：`runode setup usage` 在 `~/.gemini/settings.json` 的 `hooks` 里给 `SessionStart`、
//! `AfterTool` 和 `AfterAgent` 末尾各挂一条调 runode 的钩子（`usage::hook_command`）。Gemini 跑钩子时把会话的 JSON（含会话
//! 记录的 `transcript_path`）从标准输入交过来，这里从会话记录（JSONL）末尾找最近一条带 `tokens` 的模型
//! 回复，报给宿主。不挂 `AfterModel`：它每个流式片段都跑一次。什么都不输出：Gemini 把
//! 钩子输出的文字当成要给用户看的消息。

use std::path::{Path, PathBuf};

use anyhow::Result;
use runode_shared_types::agent::AgentUsage;
use serde_json::{Value, json};

use crate::{
    Env,
    usage::{Agent, add_hooks, hook_command, object_mut, read_object, read_stdin, report, tail_lines, write_object},
};

/// 挂在哪些事件上：会话开始时报一次（续上旧会话时会话记录里已经有用量），每次用完工具和每轮结束时报。
const EVENTS: [&str; 3] = ["SessionStart", "AfterTool", "AfterAgent"];
/// 钩子的超时，毫秒。
const TIMEOUT_MS: u64 = 5000;

/// 读标准输入，报给宿主。
pub(crate) fn run(env: &Env) {
    let Some(json) = read_stdin() else { return };
    let path = json.get("transcript_path").and_then(Value::as_str).filter(|path| !path.is_empty());
    if let Some(reply) = path.and_then(|path| last_reply(Path::new(path))) {
        let _ = report(env, usage(&reply));
    }
}

/// 会话记录里最近一条带 `tokens` 的模型回复。同一条回复改了会整条再写一行，所以从后往前第一条就是最新的。
// shortcut: 不管 `$rewindTo`，回退之后到下一条回复之前显示的还是回退前的用量。
pub(crate) fn last_reply(path: &Path) -> Option<Value> {
    tail_lines(path).into_iter().filter(|line| line.contains("\"tokens\"")).find_map(|line| {
        let reply = serde_json::from_str::<Value>(&line).ok()?;
        (reply.get("type")?.as_str()? == "gemini" && reply.get("tokens")?.is_object()).then_some(reply)
    })
}

/// 从一条模型回复里取出界面要显示的几项。`input` 已经含着读缓存的那部分；上下文窗口 Gemini 不报，按
/// Gemini CLI 自己的规矩：gemma-4 是 256K，别的都是 1M。
pub(crate) fn usage(reply: &Value) -> AgentUsage {
    let int = |key: &str| reply.get("tokens")?.get(key)?.as_u64();
    let model = reply.get("model").and_then(Value::as_str);
    AgentUsage {
        model: model.map(str::to_owned),
        context_tokens: int("input"),
        cache_read_tokens: int("cached"),
        cache_write_tokens: None,
        output_tokens: int("output"),
        context_window: model.map(|model| if model.starts_with("gemma-4") { 256_000 } else { 1_048_576 }),
        cost_micro_usd: None,
        five_hour_percent: None,
    }
}

/// `install` 改的设置文件。
pub(crate) fn settings_path(home: &Path) -> PathBuf {
    home.join(".gemini/settings.json")
}

/// 在 `EVENTS` 每个事件的末尾挂上 runode 的钩子，返回改的设置文件。
pub(crate) fn install(home: &Path) -> Result<PathBuf> {
    let path = settings_path(home);
    let mut settings = read_object(&path)?;
    let hook = json!({ "name": "runode-usage", "type": "command", "command": hook_command(Agent::Gemini, None), "timeout": TIMEOUT_MS });
    add_hooks(object_mut(settings.entry("hooks").or_insert(Value::Null)), &EVENTS, Agent::Gemini, hook);
    write_object(&path, settings)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_takes_the_last_reply() {
        let path = std::env::temp_dir().join(format!("runode-gemini-session-{}.jsonl", std::process::id()));
        let lines = [
            r#"{"sessionId":"s","projectHash":"h","startTime":"t","kind":"main"}"#,
            r#"{"id":"1","type":"gemini","content":"a","tokens":{"input":1000,"output":5,"cached":800,"thoughts":40,"tool":0,"total":1045},"model":"gemini-3-pro-preview"}"#,
            r#"{"id":"2","type":"user","content":"\"tokens\""}"#,
            r#"{"id":"3","type":"gemini","content":"b","tokens":null}"#,
            r#"{"$set":{"lastUpdated":"t"}}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();
        let reply = last_reply(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            usage(&reply.unwrap()),
            AgentUsage {
                model: Some("gemini-3-pro-preview".into()),
                context_tokens: Some(1000),
                cache_read_tokens: Some(800),
                output_tokens: Some(5),
                context_window: Some(1_048_576),
                ..AgentUsage::default()
            }
        );
    }
}
