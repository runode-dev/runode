//! 前台 agent 在进行哪场对话，以及在新开的 shell 里怎么接回它。

use std::path::Path;

use runode_types::agent::{AgentKind, Conversation};

/// 对话号最长这么多字节；各家的对话号都是 UUID。
const MAX_ID: usize = 64;

/// `pids`（前台进程组里的进程）中那个 `kind` 正在进行的对话，`home` 是家目录。Claude Code 把每个进程的
/// 对话号写在 `~/.claude/sessions/<pid>.json` 的 `sessionId` 里，`/clear`、`/resume` 后跟着改；别家读不到。
pub fn current(kind: AgentKind, pids: &[u32], home: &Path) -> Option<Conversation> {
    if kind != AgentKind::Claude {
        return None;
    }
    let dir = home.join(".claude").join("sessions");
    pids.iter().find_map(|pid| {
        let text = std::fs::read_to_string(dir.join(format!("{pid}.json"))).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        let id = value.get("sessionId")?.as_str()?;
        valid(id).then(|| Conversation { kind, id: id.to_owned() })
    })
}

/// 在提示符上接回 `conversation` 的命令；那家接不回、或者对话号不像对话号（存档被改过）时为 `None`。
pub fn resume_command(conversation: &Conversation) -> Option<String> {
    if conversation.kind != AgentKind::Claude || !valid(&conversation.id) {
        return None;
    }
    crate::models::resume_command("claude", &conversation.id)
}

/// 对话号要原样打进 shell，只认 ASCII 字母、数字和 `-`。
fn valid(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}
