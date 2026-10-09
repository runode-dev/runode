//! pi 的用量：`runode setup usage` 往 `~/.pi/agent/extensions` 里装一个扩展（`EXTENSION`）。pi 每条
//! 回复结束、开会话和换模型时，它在 runode 的终端里起 `"$RUNODE_BIN" usage-hook pi`，把模型、上下文、
//! 这条回复的用量和会话累计花费编成 JSON 从标准输入交过来，这里报给宿主。不在 runode 的终端里时它
//! 什么都不做，和别的 agent 的 `usage::hook_command` 一样。

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use runode_shared_types::agent::AgentUsage;
use serde_json::Value;

use crate::{
    Env,
    usage::{read_stdin, report},
};

/// 装进 pi 的扩展。
const EXTENSION: &str = include_str!("pi.ts");

/// 读标准输入，报给宿主。
pub(crate) fn run(env: &Env) {
    if let Some(json) = read_stdin() {
        let _ = report(env, usage(&json));
    }
}

/// 从扩展交来的 JSON 里取出界面要显示的几项。pi 的 `input` 不含读写缓存的那部分。
pub(crate) fn usage(json: &Value) -> AgentUsage {
    let int = |pointer: &str| json.pointer(pointer).and_then(Value::as_u64);
    let cost = json.get("cost").and_then(Value::as_f64).filter(|usd| *usd > 0.);
    AgentUsage {
        model: json.get("model").and_then(Value::as_str).map(str::to_owned),
        context_tokens: int("/context_tokens"),
        cache_read_tokens: int("/usage/cacheRead"),
        cache_write_tokens: int("/usage/cacheWrite"),
        output_tokens: int("/usage/output"),
        context_window: int("/context_window"),
        cost_micro_usd: cost.map(|usd| (usd * 1e6).round() as u64),
        five_hour_percent: None,
    }
}

/// `install` 写的扩展文件。
pub(crate) fn extension_path(home: &Path) -> PathBuf {
    home.join(".pi/agent/extensions/runode-usage.ts")
}

/// 装上扩展，返回写的文件；已经装过时整个换掉。
pub(crate) fn install(home: &Path) -> Result<PathBuf> {
    let path = extension_path(home);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    }
    std::fs::write(&path, EXTENSION).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_takes_the_reply_and_the_session_cost() {
        let json = serde_json::json!({
            "model": "GPT-6.1 Sol",
            "context_tokens": 14438,
            "context_window": 272000,
            "usage": { "input": 819, "output": 51, "cacheRead": 13568, "cacheWrite": 0, "totalTokens": 14438 },
            "cost": 0.0035048
        });
        assert_eq!(
            usage(&json),
            AgentUsage {
                model: Some("GPT-6.1 Sol".into()),
                context_tokens: Some(14438),
                cache_read_tokens: Some(13568),
                cache_write_tokens: Some(0),
                output_tokens: Some(51),
                context_window: Some(272000),
                cost_micro_usd: Some(3505),
                five_hour_percent: None,
            }
        );
        // 刚开会话：只有模型和窗口，压缩后 pi 也不知道上下文多大（`context_tokens` 为 null）。
        let fresh =
            serde_json::json!({ "model": "GPT-6.1 Sol", "context_tokens": null, "context_window": 272000, "cost": 0 });
        assert_eq!(
            usage(&fresh),
            AgentUsage { model: Some("GPT-6.1 Sol".into()), context_window: Some(272000), ..AgentUsage::default() }
        );
    }
}
