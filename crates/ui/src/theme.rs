//! 界面的主题 token：各部件的底色、字色、边框色都从这里取，名字照 shadcn（`background`、`popover`、
//! `muted`、`border`、`ring`……）。
//!
//! 每个窗口的配色跟着自己有焦点的终端走，token 却只有全 app 一份（`cx.global::<Theme>()`，经
//! `ActiveTheme::theme` 读）：每个窗口画的时候一开头（桌面 app 的 `WindowView::render`）按本窗口的配色
//! 把它换上，同一次绘制里接着画的缓存视图、悬停提示、拖动预览读到的都是这个窗口的。所以 token
//! 只能在 render 和 paint 里读；事件处理、构造函数里读到的可能是别的窗口刚画时换上的。

use gpui::{App, Global, Hsla};
use runode_shared_types::color::Rgb;

use crate::hsla;

/// 改动和文件状态的颜色，深浅背景上都看得清。
pub const ADDED: Rgb = Rgb(0x57, 0xAB, 0x5A);
pub const REMOVED: Rgb = Rgb(0xE5, 0x53, 0x4B);
pub const MODIFIED: Rgb = Rgb(0xD2, 0xA8, 0x3E);
pub const RENAMED: Rgb = Rgb(0x53, 0x9B, 0xF5);

/// 调色板里没配置的强调色和出错色。
const RING_FALLBACK: Rgb = Rgb(0x3b, 0x82, 0xf6);
const DESTRUCTIVE_FALLBACK: Rgb = Rgb(0xe0, 0x50, 0x50);

/// 一个窗口当前的主题，由终端的前景色 `fg`、背景色 `bg` 和调色板算出来，见 `Theme::new`。
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    /// 终端的背景色和前景色。
    pub background: Hsla,
    pub foreground: Hsla,
    /// 卡片的底色和字色；卡片的边另用 `border` 一类。
    pub card: Hsla,
    pub card_foreground: Hsla,
    /// 菜单、悬停提示这类浮层的底色和字色。
    pub popover: Hsla,
    pub popover_foreground: Hsla,
    /// 主按钮：反色，前景色的底、背景色的字。
    pub primary: Hsla,
    pub primary_foreground: Hsla,
    /// 次要按钮的底色和字色。
    pub secondary: Hsla,
    pub secondary_foreground: Hsla,
    /// 不起眼的底色和淡下去的字。
    pub muted: Hsla,
    pub muted_foreground: Hsla,
    /// 悬停、选中时的底色和字色。
    pub accent: Hsla,
    pub accent_foreground: Hsla,
    /// 出错：调色板里的红色。
    pub destructive: Hsla,
    /// 分隔线和卡片之间的淡线。
    pub border: Hsla,
    /// 输入框、菜单、提示这类浮层的描边。
    pub input: Hsla,
    /// 焦点、打开着的开关这类强调：调色板里的蓝色。
    pub ring: Hsla,
    /// 卡片样式下窗口的底色，衬在卡片后面，见 `frame_color`。
    pub frame: Hsla,
    /// 卡片样式下标签条的底色和当前标签那颗胶囊的颜色，见 `tab_track_colors`。
    pub tab_track: Hsla,
    pub tab_active: Hsla,
    /// 背景比前景亮，是浅色主题。
    pub is_light: bool,
    /// 调色板的 ANSI 16 色。
    pub ansi: [Rgb; 16],
    /// 算 `mix` 用的原色：在 sRGB 里按整数混，和各处原来的写法逐值一样。
    fg: Rgb,
    bg: Rgb,
}

impl Global for Theme {}

impl Theme {
    /// 前景色 `fg`、背景色 `bg` 和调色板 `ansi` 下的主题。
    pub fn new(fg: Rgb, bg: Rgb, ansi: [Rgb; 16]) -> Self {
        let mix = |k: f32| hsla(bg.mix(fg, k));
        let (track, active) = tab_track_colors(fg, bg);
        Self {
            background: hsla(bg),
            foreground: hsla(fg),
            card: hsla(bg),
            card_foreground: hsla(fg),
            popover: mix(0.06),
            popover_foreground: hsla(fg),
            primary: hsla(fg),
            primary_foreground: hsla(bg),
            secondary: mix(0.16),
            secondary_foreground: hsla(fg),
            muted: mix(0.04),
            muted_foreground: hsla(fg).opacity(0.5),
            accent: mix(0.06),
            accent_foreground: hsla(fg),
            destructive: hsla(ansi[1]),
            border: hsla(fg).opacity(0.09),
            input: hsla(fg).opacity(0.15),
            ring: hsla(ansi[4]),
            frame: hsla(frame_color(fg, bg)),
            tab_track: hsla(track),
            tab_active: hsla(active),
            is_light: is_light(fg, bg),
            ansi,
            fg,
            bg,
        }
    }

    /// 背景色往前景色混 `k`（0 到 1）：组件自己的悬停、按下这类状态色这样算。
    pub fn mix(&self, k: f32) -> Hsla {
        hsla(self.bg.mix(self.fg, k))
    }
}

/// 从配置的调色板（`(序号, 颜色)`，后写的算数）取 ANSI 16 色，`Theme::new` 用。没配置的序号用
/// 默认配色里的；强调色（4）和出错色（1）没配置时另有缺省，和设置页一样。
pub fn ansi_colors(palette: &[(u8, Rgb)]) -> [Rgb; 16] {
    let mut ansi = runode_shared_types::theme::ANSI;
    ansi[1] = DESTRUCTIVE_FALLBACK;
    ansi[4] = RING_FALLBACK;
    for &(ix, color) in palette {
        if let Some(slot) = ansi.get_mut(usize::from(ix)) {
            *slot = color;
        }
    }
    ansi
}

/// 读当前窗口的主题，只在 render 和 paint 里读，见模块说明。
pub trait ActiveTheme {
    fn theme(&self) -> &Theme;
}

impl ActiveTheme for App {
    fn theme(&self) -> &Theme {
        self.global::<Theme>()
    }
}

/// 背景比前景亮，是浅色主题。
pub fn is_light(fg: Rgb, bg: Rgb) -> bool {
    let luma = |c: Rgb| 0.299 * f32::from(c.0) + 0.587 * f32::from(c.1) + 0.114 * f32::from(c.2);
    luma(bg) > luma(fg)
}

/// 卡片样式下窗口的底色，衬在卡片后面：浅色主题往前景色混一点，深色主题往黑色压一些，两种都比
/// 终端背景深一档。
pub fn frame_color(fg: Rgb, bg: Rgb) -> Rgb {
    if is_light(fg, bg) { bg.mix(fg, 0.06) } else { bg.mix(Rgb(0, 0, 0), 0.4) }
}

/// 卡片样式下标签条的底色，以及条里当前标签那颗胶囊的颜色。浅色主题里条介于外框
/// 和卡片之间、胶囊就是卡片的白；深色主题里卡片已经比外框亮，条用卡片的颜色，胶囊再亮一档。
pub fn tab_track_colors(fg: Rgb, bg: Rgb) -> (Rgb, Rgb) {
    if is_light(fg, bg) { (frame_color(fg, bg).mix(bg, 0.5), bg) } else { (bg, bg.mix(fg, 0.12)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DARK: (Rgb, Rgb) = (Rgb(0xE6, 0xE1, 0xD8), Rgb(0x17, 0x16, 0x18));
    const LIGHT: (Rgb, Rgb) = (Rgb(0x1d, 0x1d, 0x1f), Rgb(0xff, 0xff, 0xff));

    fn palette() -> [Rgb; 16] {
        std::array::from_fn(|i| Rgb(i as u8 * 10, 0x40, 0x80))
    }

    /// 每个 token 和搬过来以前各处的写法逐值相等：右边照抄原来的算式。
    #[test]
    fn tokens_match_the_colors_they_replace() {
        for (fg, bg) in [DARK, LIGHT] {
            let ansi = palette();
            let theme = Theme::new(fg, bg, ansi);
            let (f, b) = (hsla(fg), hsla(bg));
            // 窗口、卡片（`panel::card`）的底色和字色。
            assert_eq!((theme.background, theme.foreground), (b, f));
            assert_eq!((theme.card, theme.card_foreground), (b, f));
            // 菜单的底色（`render_menu_list` 的 `menu_bg`）。
            assert_eq!(theme.popover, hsla(bg.mix(fg, 0.06)));
            assert_eq!(theme.popover_foreground, f);
            assert_eq!((theme.primary, theme.primary_foreground), (f, b));
            // 对话框主按钮的底色。
            assert_eq!(theme.secondary, hsla(bg.mix(fg, 0.16)));
            assert_eq!(theme.secondary_foreground, f);
            // 面板里的说明（`panel_message`）。
            assert_eq!(theme.muted, hsla(bg.mix(fg, 0.04)));
            assert_eq!(theme.muted_foreground, f.opacity(0.5));
            // 各处的悬停底色。
            assert_eq!(theme.accent, hsla(bg.mix(fg, 0.06)));
            assert_eq!(theme.accent_foreground, f);
            // 分隔线（`divider_color`）和浮层的描边。
            assert_eq!(theme.border, f.opacity(0.09));
            assert_eq!(theme.input, f.opacity(0.15));
            assert_eq!(theme.destructive, hsla(ansi[1]));
            assert_eq!(theme.ring, hsla(ansi[4]));
            assert_eq!(theme.ansi, ansi);
            // 窗口外框和标签条：原来 `WindowView` 里的算式。
            let luma = |c: Rgb| 0.299 * f32::from(c.0) + 0.587 * f32::from(c.1) + 0.114 * f32::from(c.2);
            let light = luma(bg) > luma(fg);
            assert_eq!(theme.is_light, light);
            let frame = if light { bg.mix(fg, 0.06) } else { bg.mix(Rgb(0, 0, 0), 0.4) };
            assert_eq!(theme.frame, hsla(frame));
            let (track, pill) = if light { (frame.mix(bg, 0.5), bg) } else { (bg, bg.mix(fg, 0.12)) };
            assert_eq!((theme.tab_track, theme.tab_active), (hsla(track), hsla(pill)));
            for k in [0.04, 0.08, 0.10, 0.12, 0.14, 0.22] {
                assert_eq!(theme.mix(k), hsla(bg.mix(fg, k)));
            }
        }
        assert!(Theme::new(LIGHT.0, LIGHT.1, palette()).is_light);
        assert!(!Theme::new(DARK.0, DARK.1, palette()).is_light);
    }

    /// 调色板里后写的算数；强调色和出错色没配置时和设置页一样用缺省的蓝和红。
    #[test]
    fn ansi_colors_follow_the_palette() {
        let ansi = ansi_colors(&[(2, Rgb(1, 1, 1)), (2, Rgb(2, 2, 2)), (200, Rgb(3, 3, 3))]);
        assert_eq!(ansi[2], Rgb(2, 2, 2));
        assert_eq!(ansi[0], runode_shared_types::theme::ANSI[0]);
        assert_eq!(ansi[4], RING_FALLBACK);
        assert_eq!(ansi[1], DESTRUCTIVE_FALLBACK);
        let ansi = ansi_colors(&[(1, Rgb(9, 9, 9)), (4, Rgb(8, 8, 8))]);
        assert_eq!((ansi[1], ansi[4]), (Rgb(9, 9, 9), Rgb(8, 8, 8)));
    }
}
