//! 各家 agent 的 logo，卡片样式下画在标签和分屏标题条上，一眼看出终端里跑的是哪个 agent。图标
//! 本身在 `runode_ui` 的 `agent_logos` 里，出处和许可写在图标目录的 `LICENSE` 里。
//!
//! 有品牌色的 logo 用 `img()` 按原色画；只有一种颜色的用 `svg()` 染成前景色，深浅两种背景上都
//! 看得清。没收 logo 的 agent 画成圆角方框里它名字的头一个字母，认不出是哪个的（`Other`）为
//! `None`，由调用方退回状态标记。

use gpui::{AnyElement, FontWeight, Hsla, Pixels, div, img, prelude::*, svg};
use runode_types::agent::AgentKind;

use crate::ui::icons::agent_logos::{
    AMP, ANTIGRAVITY, CLAUDE, CLINE, CODEBUDDY, CODEX, CURSOR, DEVIN, GEMINI, GITHUB_COPILOT, GOOSE, GROK, HERMES,
    JULES, JUNIE, KILO, KIMI, KIRO, MASTRACODE, MISTRAL_VIBE, MUSE, OMP, OPENCODE, OPENHANDS, PI, QODER, QWEN, TRAE,
};

/// 一个 logo 怎么画：按原色，染成前景色，还是没有 logo、写名字的头一个字母。
enum Logo {
    Colored(&'static str),
    Mono(&'static str),
    Initial(char),
}

fn logo_of(kind: AgentKind) -> Option<Logo> {
    use Logo::{Colored, Initial, Mono};
    Some(match kind {
        AgentKind::Pi => Mono(PI),
        AgentKind::Claude => Colored(CLAUDE),
        AgentKind::Codex => Mono(CODEX),
        AgentKind::Gemini => Colored(GEMINI),
        AgentKind::Cursor => Mono(CURSOR),
        AgentKind::Devin => Colored(DEVIN),
        AgentKind::Antigravity => Colored(ANTIGRAVITY),
        AgentKind::Cline => Mono(CLINE),
        AgentKind::Mastracode => Mono(MASTRACODE),
        AgentKind::OpenCode => Mono(OPENCODE),
        AgentKind::GithubCopilot => Mono(GITHUB_COPILOT),
        AgentKind::Kimi => Colored(KIMI),
        AgentKind::Kiro => Colored(KIRO),
        AgentKind::Amp => Colored(AMP),
        AgentKind::Grok => Mono(GROK),
        AgentKind::Hermes => Mono(HERMES),
        AgentKind::Kilo => Mono(KILO),
        AgentKind::Qodercli => Mono(QODER),
        AgentKind::Qwen => Colored(QWEN),
        AgentKind::Goose => Mono(GOOSE),
        AgentKind::Junie => Colored(JUNIE),
        AgentKind::OpenHands => Mono(OPENHANDS),
        AgentKind::Trae => Colored(TRAE),
        AgentKind::CodeBuddy => Colored(CODEBUDDY),
        AgentKind::MistralVibe => Colored(MISTRAL_VIBE),
        AgentKind::Jules => Mono(JULES),
        AgentKind::Omp => Mono(OMP),
        AgentKind::Muse => Colored(MUSE),
        AgentKind::Droid
        | AgentKind::Letta
        | AgentKind::Maki
        | AgentKind::Aider
        | AgentKind::Crush
        | AgentKind::Auggie
        | AgentKind::ContinueCli
        | AgentKind::Iflow
        | AgentKind::Codebuff
        | AgentKind::Plandex => Initial(kind.display_name().chars().next()?),
        AgentKind::Other => return None,
    })
}

/// 按原色画 `path` 这个 logo，边长 `size`。
pub(in crate::window) fn colored_logo(path: &'static str, size: Pixels) -> AnyElement {
    img(path).flex_none().size(size).into_any_element()
}

/// `kind` 的 logo 文件，菜单项前面染成前景色画；没收 logo 的为 `None`。
pub(in crate::window) fn logo_path(kind: AgentKind) -> Option<&'static str> {
    match logo_of(kind)? {
        Logo::Colored(path) | Logo::Mono(path) => Some(path),
        Logo::Initial(_) => None,
    }
}

/// `kind` 的 logo，边长 `size`；单色的和首字母都染成 `fg`。认不出是哪个的 agent 为 `None`。
pub(in crate::window) fn agent_logo(kind: AgentKind, size: Pixels, fg: Hsla) -> Option<AnyElement> {
    Some(match logo_of(kind)? {
        Logo::Colored(path) => colored_logo(path, size),
        Logo::Mono(path) => svg().path(path).flex_none().size(size).text_color(fg).into_any_element(),
        Logo::Initial(initial) => div()
            .flex_none()
            .size(size)
            .rounded(size * 0.25)
            .border_1()
            .border_color(fg)
            .flex()
            .items_center()
            .justify_center()
            .text_size(size * 0.62)
            .line_height(size)
            .font_weight(FontWeight::BOLD)
            .text_color(fg)
            .child(initial.to_string())
            .into_any_element(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::icons::agent_logos::FILES;

    #[test]
    fn every_known_agent_has_a_logo_or_an_initial() {
        let mut files = 0;
        for kind in AgentKind::ALL {
            match logo_of(kind) {
                Some(Logo::Colored(path) | Logo::Mono(path)) => {
                    assert!(FILES.iter().any(|(listed, _)| *listed == path), "{path}");
                    files += 1;
                }
                Some(Logo::Initial(_)) => {}
                None => panic!("{kind:?} has neither a logo nor an initial"),
            }
        }
        assert_eq!(files, FILES.len());
        assert!(logo_of(AgentKind::Other).is_none());
    }
}
