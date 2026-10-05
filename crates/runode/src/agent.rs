//! 识别 claude、codex、pi 这类编程 agent 报告的状态。
//!
//! claude 和 codex 用标题开头的一个字符加空格报告状态：claude 空闲时是「✳」，工作中在
//! 「◐」「◑」之间切换；codex 工作中是一个转圈的盲文点阵字符（「⠋」「⠙」等），空闲时不带前缀。
//! pi 不在标题里报告状态，而是用 OSC 9;4 进度，由 `Session::feed` 处理；它的标题以「π」开头。

use runode_model::agent::{Agent, AgentKind, AgentState};

/// 去掉状态前缀后的标题是不是 pi 的，pi 的标题是「π - 目录名」。
pub fn is_pi_title(title: &str) -> bool {
    title.starts_with("π ")
}

/// 拆出标题开头的状态字符，返回 agent 和去掉它之后的标题；开头不是状态字符时为 `None`。
pub fn split_status(title: &str) -> Option<(Agent, &str)> {
    let mut chars = title.chars();
    let (kind, state) = match chars.next()? {
        '✳' => (AgentKind::Claude, AgentState::Idle),
        '◐' | '◓' | '◑' | '◒' => (AgentKind::Claude, AgentState::Working),
        '\u{2800}'..='\u{28ff}' => (AgentKind::Codex, AgentState::Working),
        _ => return None,
    };
    let rest = chars.as_str().strip_prefix(' ')?.trim_start();
    // 有的 pi 扩展也在标题前加盲文点阵转圈。
    let kind = if is_pi_title(rest) { AgentKind::Pi } else { kind };
    Some((Agent { kind, state }, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(kind: AgentKind, state: AgentState) -> Agent {
        Agent { kind, state }
    }

    #[test]
    fn splits_claude_and_codex_status_prefixes() {
        use AgentKind::*;
        use AgentState::*;
        assert_eq!(split_status("✳ 确认回复"), Some((agent(Claude, Idle), "确认回复")));
        assert_eq!(split_status("◑ Tabs UI/UX 优化"), Some((agent(Claude, Working), "Tabs UI/UX 优化")));
        assert_eq!(split_status("⠴ 美化图标 | runode"), Some((agent(Codex, Working), "美化图标 | runode")));
        assert_eq!(split_status("⠴ π - runode"), Some((agent(Pi, Working), "π - runode")));
    }

    #[test]
    fn leaves_ordinary_titles_alone() {
        assert_eq!(split_status("美化图标 | runode"), None);
        assert_eq!(split_status("vim"), None);
        // 状态字符后面必须跟空格，免得把正文的第一个字吃掉。
        assert_eq!(split_status("◐x"), None);
        assert_eq!(split_status(""), None);
    }
}
