//! 各家 agent 的 logo，卡片样式下画在标签和分屏标题条上，一眼看出终端里跑的是哪个 agent。图标
//! 的出处和许可写在图标目录的 `LICENSE` 里。
//!
//! 有品牌色的 logo 用 `img()` 按原色画；只有一种颜色的用 `svg()` 染成前景色，深浅两种背景上都
//! 看得清。没收 logo 的 agent 画成圆角方框里它名字的头一个字母，认不出是哪个的（`Other`）为
//! `None`，由调用方退回状态标记。

use gpui::{AnyElement, FontWeight, Hsla, Pixels, div, img, prelude::*, svg};
use runode_shared_types::agent::AgentKind;

/// 列出所有 logo：给每个 logo 一个资源路径常量，并把它们连同文件内容收进 `FILES`，
/// 路径和文件名只写一遍，免得两边对不上。
macro_rules! logos {
    ($($konst:ident = $name:literal,)*) => {
        $(const $konst: &str = concat!("icons/agents/", $name, ".svg");)*

        /// 所有 agent logo 的资源路径和内容，`Assets` 按路径从这里取。
        pub(crate) const FILES: &[(&str, &[u8])] = &[
            $(($konst, include_bytes!(concat!("../../../assets/icons/agents/", $name, ".svg"))),)*
        ];
    };
}

logos! {
    PI = "pi",
    CLAUDE = "claude",
    CODEX = "codex",
    GEMINI = "gemini",
    CURSOR = "cursor",
    DEVIN = "devin",
    ANTIGRAVITY = "antigravity",
    CLINE = "cline",
    MASTRACODE = "mastracode",
    OPENCODE = "opencode",
    GITHUB_COPILOT = "github-copilot",
    KIMI = "kimi",
    KIRO = "kiro",
    AMP = "amp",
    GROK = "grok",
    HERMES = "hermes",
    KILO = "kilo",
    QODER = "qoder",
    QWEN = "qwen",
    GOOSE = "goose",
    JUNIE = "junie",
    OPENHANDS = "openhands",
    TRAE = "trae",
    CODEBUDDY = "codebuddy",
    MISTRAL_VIBE = "mistral-vibe",
    JULES = "jules",
    OMP = "omp",
}

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
        AgentKind::Droid
        | AgentKind::Letta
        | AgentKind::Maki
        | AgentKind::Muse
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

/// `kind` 自己的颜色：卡片样式下它等回答时的圆点、在标签图标叠里压在后面时铺的颜色都用它。
/// 单色 logo 的 agent 多半没有，为 `None`。
pub(in crate::window) fn brand_color(kind: AgentKind) -> Option<u32> {
    Some(match kind {
        AgentKind::Claude => 0xD97757,
        AgentKind::Codex => 0x10A37F,
        AgentKind::Gemini | AgentKind::Antigravity => 0x3186FF,
        AgentKind::Devin => 0x21C19A,
        AgentKind::GithubCopilot => 0x6E40C9,
        AgentKind::Kimi => 0x1783FF,
        AgentKind::Kiro => 0x9046FF,
        AgentKind::Amp => 0xF34E3F,
        AgentKind::Hermes => 0x7C3AED,
        AgentKind::Qodercli => 0x2ADB5C,
        AgentKind::Qwen => 0x6F69F7,
        AgentKind::Junie => 0x47E054,
        AgentKind::OpenHands => 0xE0C34A,
        AgentKind::Trae => 0x32F08C,
        AgentKind::CodeBuddy => 0x6C4DFF,
        AgentKind::MistralVibe => 0xFA500F,
        AgentKind::Jules => 0x715CD7,
        AgentKind::Omp => 0xF97316,
        _ => return None,
    })
}

/// 标签图标叠里压在后面的那几块铺的颜色：`brand_color`，没有的用灰色；认不出是哪个的 agent 为
/// `None`，和普通程序一样画。
pub(in crate::window) fn accent(kind: AgentKind) -> Option<u32> {
    logo_of(kind)?;
    Some(brand_color(kind).unwrap_or(0x6B7280))
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
        Logo::Colored(path) => img(path).flex_none().size(size).into_any_element(),
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
