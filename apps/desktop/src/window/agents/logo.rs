//! 各家 agent 的 logo，卡片样式下画在标签和分屏标题条上，一眼看出终端里跑的是哪个 agent。图标
//! 的出处和许可写在图标目录的 `LICENSE` 里。
//!
//! 有品牌色的 logo 用 `img()` 按原色画；只有一种颜色的用 `svg()` 染成前景色，深浅两种背景上都
//! 看得清。没收 logo 的 agent 为 `None`，由调用方退回状态标记。

use gpui::{AnyElement, Hsla, Pixels, img, prelude::*, svg};
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
}

/// 一个 logo 怎么画：按原色，还是染成前景色。
enum Logo {
    Colored(&'static str),
    Mono(&'static str),
}

fn logo_of(kind: AgentKind) -> Option<Logo> {
    use Logo::{Colored, Mono};
    Some(match kind {
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
        AgentKind::Pi
        | AgentKind::Omp
        | AgentKind::Droid
        | AgentKind::Letta
        | AgentKind::Maki
        | AgentKind::Muse
        | AgentKind::Other => return None,
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
        _ => return None,
    })
}

/// 标签图标叠里压在后面的那几块铺的颜色：`brand_color`，没有的用灰色；没有 logo 的 agent 为
/// `None`，和普通程序一样画。
pub(in crate::window) fn accent(kind: AgentKind) -> Option<u32> {
    logo_of(kind)?;
    Some(brand_color(kind).unwrap_or(0x6B7280))
}

/// `kind` 的 logo，边长 `size`；单色的染成 `fg`。没有 logo 时为 `None`。
pub(in crate::window) fn agent_logo(kind: AgentKind, size: Pixels, fg: Hsla) -> Option<AnyElement> {
    Some(match logo_of(kind)? {
        Logo::Colored(path) => img(path).flex_none().size(size).into_any_element(),
        Logo::Mono(path) => svg().path(path).flex_none().size(size).text_color(fg).into_any_element(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_logo_is_a_listed_file() {
        let kinds = [
            AgentKind::Claude,
            AgentKind::Codex,
            AgentKind::Gemini,
            AgentKind::Cursor,
            AgentKind::Devin,
            AgentKind::Antigravity,
            AgentKind::Cline,
            AgentKind::Mastracode,
            AgentKind::OpenCode,
            AgentKind::GithubCopilot,
            AgentKind::Kimi,
            AgentKind::Kiro,
            AgentKind::Amp,
            AgentKind::Grok,
            AgentKind::Hermes,
            AgentKind::Kilo,
            AgentKind::Qodercli,
            AgentKind::Qwen,
        ];
        for kind in kinds {
            let Some(Logo::Colored(path) | Logo::Mono(path)) = logo_of(kind) else {
                panic!("{kind:?} has no logo");
            };
            assert!(FILES.iter().any(|(listed, _)| *listed == path), "{path}");
        }
        assert_eq!(FILES.len(), kinds.len());
    }
}
