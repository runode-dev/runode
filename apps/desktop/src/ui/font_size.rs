//! 界面文字的字号。窗口的 rem 设成配置里的 `ui-font-size`，各档字号和要装下文字的长度都按 rem
//! 写，改了配置整个界面一起等比缩放。终端和预览栏的字号另外配，不走这里。

use gpui::{AbsoluteLength, App, Pixels, Rems, px, rems};

use crate::config::AppConfig;

/// 各档字号和 `scaled` 的点数都是按这个基准（`ui-font-size` 的默认值）写的。
pub const BASE: f32 = 12.;

/// 界面文字的几档字号，括号里是基准字号下的点数。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontSize {
    /// 计数徽标、引用标签（10）。
    Tiny,
    /// 次要的说明、路径、日期、提示（11）。
    Small,
    /// 正文，面板、菜单、按钮的默认字号（12）。
    Body,
    /// 文件树、设置项的标题（13）。
    Medium,
    /// 输入框、对话框里的正文（14）。
    Large,
    /// 对话框和标题栏的标题（15）。
    Title,
    /// 大号的标题（18）。
    Heading,
    /// 页面标题（22）。
    Display,
}

impl FontSize {
    /// 基准字号下的点数。
    pub const fn points(self) -> f32 {
        match self {
            Self::Tiny => 10.,
            Self::Small => 11.,
            Self::Body => 12.,
            Self::Medium => 13.,
            Self::Large => 14.,
            Self::Title => 15.,
            Self::Heading => 18.,
            Self::Display => 22.,
        }
    }
}

impl From<FontSize> for AbsoluteLength {
    fn from(size: FontSize) -> Self {
        scaled(size.points()).into()
    }
}

/// 基准字号下 `points` 点长、跟着 `ui-font-size` 等比变的长度：要装下文字的行高、栏高、按钮高和
/// 定宽的文字栏用它，图标、留白、分隔线这些不跟着变的仍写 `px`。
pub fn scaled(points: f32) -> Rems {
    rems(points / BASE)
}

/// 配置的界面字号，窗口的 rem 设成它。限在 6 到 20 点：太小时小几档的字看不见；标题栏、标签、
/// 分屏标题条和状态栏的高度跟着窗口按钮定死，再大字就装不下了。
pub fn rem_size(cx: &App) -> Pixels {
    px(cx.global::<AppConfig>().0.ui_font_size.clamp(6., 20.))
}

/// 界面相对基准字号放大的倍数。要拿长度做算术时（估宽度、拖动的上下限）乘上它。
pub fn scale(cx: &App) -> f32 {
    f32::from(rem_size(cx)) / BASE
}
