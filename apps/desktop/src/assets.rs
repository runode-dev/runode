//! 随 app 带的终端字体。图标在 `ui::icons`，GPUI 按路径取图标经 `ui::icons::Assets`。

use std::borrow::Cow;

use gpui::App;

/// 随 app 带的终端字体：默认配置用的 Hack Nerd Font Mono（Nerd Fonts v3.5.1，MIT 和 Bitstream Vera
/// 许可，许可证和字体文件放在一起，打包时一起放进 app）。系统里没装它时也能用上，不至于退回 Menlo。
const FONTS: [&[u8]; 4] = [
    include_bytes!("../assets/fonts/HackNerdFontMono/HackNerdFontMono-Regular.ttf"),
    include_bytes!("../assets/fonts/HackNerdFontMono/HackNerdFontMono-Bold.ttf"),
    include_bytes!("../assets/fonts/HackNerdFontMono/HackNerdFontMono-Italic.ttf"),
    include_bytes!("../assets/fonts/HackNerdFontMono/HackNerdFontMono-BoldItalic.ttf"),
];

/// 把随 app 带的字体登记给 GPUI，要在第一次解析终端字体之前做。
pub fn install_fonts(cx: &App) {
    if let Err(err) = cx.text_system().add_fonts(FONTS.iter().map(|data| Cow::Borrowed(*data)).collect()) {
        tracing::warn!("failed to load bundled fonts: {err}");
    }
}
