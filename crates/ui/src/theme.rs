//! 界面的主题 token：各部件的底色、字色、边框色都从这里取，名字照 shadcn（`background`、`popover`、
//! `muted`、`border`、`ring`……）。
//!
//! token 只有全 app 一份（`cx.global::<Theme>()`，经 `ActiveTheme::theme` 读），每个窗口画的时候一开头
//! 按本窗口的配色换上，所以只能在 render 和 paint 里读；事件处理、构造函数里读到的可能是别的窗口的。

use gpui::{App, Global, Hsla};
use runode_types::color::Rgb;

use crate::hsla;

/// 改动和文件状态的颜色，深浅背景上都看得清。
pub const ADDED: Rgb = Rgb(0x57, 0xAB, 0x5A);
pub const REMOVED: Rgb = Rgb(0xE5, 0x53, 0x4B);
pub const MODIFIED: Rgb = Rgb(0x53, 0x9B, 0xF5);
pub const RENAMED: Rgb = Rgb(0x53, 0x9B, 0xF5);

/// 状态色，深浅主题各一套，不跟调色板走：琥珀只用于「agent 在等你」这类要用户出手的提醒，蓝是在干活，
/// 绿是做完和新增。`WARNING_TEXT_LIGHT` 是浅色底上读得清的琥珀字，`WARNING_FOREGROUND` 是琥珀实底上的字。
const WARNING_DARK: Rgb = Rgb(0xF5, 0xA8, 0x3C);
const WARNING_LIGHT: Rgb = Rgb(0xE8, 0x95, 0x1F);
const WARNING_TEXT_LIGHT: Rgb = Rgb(0x9A, 0x5B, 0x00);
const WARNING_FOREGROUND: Rgb = Rgb(0x1B, 0x12, 0x03);
const INFO_DARK: Rgb = Rgb(0x7F, 0xA9, 0xFF);
const INFO_LIGHT: Rgb = Rgb(0x2F, 0x6B, 0xDB);
const SUCCESS_DARK: Rgb = Rgb(0x7F, 0xD4, 0x9B);
const SUCCESS_LIGHT: Rgb = Rgb(0x1F, 0x8A, 0x3E);

/// 调色板里没配置的强调色和出错色。
const RING_FALLBACK: Rgb = Rgb(0x3b, 0x82, 0xf6);
const DESTRUCTIVE_FALLBACK: Rgb = Rgb(0xe0, 0x50, 0x50);

/// 一个窗口当前的主题，由终端的前景色 `fg`、背景色 `bg` 和调色板算出来，见 `Theme::new`。
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    /// 终端的背景色和前景色。
    pub background: Hsla,
    pub foreground: Hsla,
    /// 卡片的底色；字用 `foreground`，边另用 `border` 一类。
    pub card: Hsla,
    /// 菜单、悬停提示这类浮层的底色和字色。
    pub popover: Hsla,
    pub popover_foreground: Hsla,
    /// 浮层里的行悬停、选中（键盘挪到的、当前挑着的）时的底色：浮层底色本身比 `background` 深，
    /// 要比 `accent`、`accent_selected` 再深一档才看得出。
    pub popover_hover: Hsla,
    pub popover_selected: Hsla,
    /// 主按钮：反色，前景色的底、背景色的字。
    pub primary: Hsla,
    pub primary_foreground: Hsla,
    /// 次要按钮的底色和字色。
    pub secondary: Hsla,
    pub secondary_foreground: Hsla,
    /// 不起眼的底色和淡下去的字。
    pub muted: Hsla,
    pub muted_foreground: Hsla,
    /// 次一级的字，比 `foreground` 淡、比 `muted_foreground` 浓。
    pub subtle_foreground: Hsla,
    /// 非终端的卡片（右侧面板、收件箱、看板列、设置卡）的底色。
    pub surface: Hsla,
    /// 嵌在窗口外框上的搜索框的底色。
    pub field: Hsla,
    /// 要用户出手的提醒（agent 在等你）：`warning` 用于标记和实底，`warning_text` 是写在普通底面上的
    /// 琥珀字，`warning_foreground` 是 `warning` 实底上的字。
    pub warning: Hsla,
    pub warning_text: Hsla,
    pub warning_foreground: Hsla,
    /// 在干活。
    pub info: Hsla,
    /// 做完、新增。
    pub success: Hsla,
    /// `background` 底面上的行悬停时的底色，和选中（当前的、打开着的）那行的底色；字用 `foreground`。
    pub accent: Hsla,
    pub accent_selected: Hsla,
    /// 出错：调色板里的红色。
    pub destructive: Hsla,
    /// 分隔线和卡片之间的淡线。
    pub border: Hsla,
    /// 输入框、菜单、提示这类浮层的描边。
    pub input: Hsla,
    /// 设置页这类表单里控件（分段、下拉、小标签、色块）的描边，比 `input` 淡一些。
    pub control_border: Hsla,
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
    /// 算 `mix` 用的原色：在 sRGB 里按整数混。
    fg: Rgb,
    bg: Rgb,
}

impl Global for Theme {}

impl Theme {
    /// 前景色 `fg`、背景色 `bg` 和调色板 `ansi` 下的主题。
    pub fn new(fg: Rgb, bg: Rgb, ansi: [Rgb; 16]) -> Self {
        let mix = |k: f32| hsla(bg.mix(fg, k));
        let (track, active) = tab_track_colors(fg, bg);
        let light = is_light(fg, bg);
        let pick = |light_color: Rgb, dark_color: Rgb| hsla(if light { light_color } else { dark_color });
        let warning = pick(WARNING_LIGHT, WARNING_DARK);
        Self {
            background: hsla(bg),
            foreground: hsla(fg),
            card: hsla(bg),
            popover: mix(0.06),
            popover_foreground: hsla(fg),
            popover_hover: mix(0.09),
            popover_selected: mix(0.14),
            primary: hsla(fg),
            primary_foreground: hsla(bg),
            secondary: mix(0.16),
            secondary_foreground: hsla(fg),
            muted: mix(0.04),
            muted_foreground: hsla(fg).opacity(0.5),
            subtle_foreground: hsla(fg).opacity(0.7),
            surface: if light { hsla(bg) } else { mix(0.07) },
            field: if light { hsla(bg) } else { mix(0.016) },
            warning,
            warning_text: if light { hsla(WARNING_TEXT_LIGHT) } else { warning },
            warning_foreground: hsla(WARNING_FOREGROUND),
            info: pick(INFO_LIGHT, INFO_DARK),
            success: pick(SUCCESS_LIGHT, SUCCESS_DARK),
            accent: mix(0.06),
            accent_selected: mix(0.10),
            destructive: hsla(ansi[1]),
            border: hsla(fg).opacity(0.09),
            input: hsla(fg).opacity(0.15),
            control_border: hsla(fg).opacity(0.13),
            ring: hsla(ansi[4]),
            frame: hsla(frame_color(fg, bg)),
            tab_track: hsla(track),
            tab_active: hsla(active),
            is_light: light,
            ansi,
            fg,
            bg,
        }
    }

    /// 背景色往前景色混 `k`（0 到 1）：组件自己的悬停、按下这类状态色这样算。
    pub fn mix(&self, k: f32) -> Hsla {
        hsla(self.bg.mix(self.fg, k))
    }

    /// 终端前景色的原值：要拿 `Rgb` 算的地方（对比度、亮度、原生控件）用，不必从 `foreground` 换回来。
    pub fn fg_rgb(&self) -> Rgb {
        self.fg
    }

    /// 终端背景色的原值，同 `fg_rgb`。
    pub fn bg_rgb(&self) -> Rgb {
        self.bg
    }

    /// 前景色往背景色混 `k`：反色的浮层（看板的提示条）的底色这样算。
    pub fn mix_inverse(&self, k: f32) -> Hsla {
        hsla(self.fg.mix(self.bg, k))
    }
}

/// 从配置的调色板（`(序号, 颜色)`，后写的算数）取 ANSI 16 色，`Theme::new` 用。没配置的序号用
/// 默认配色里的；强调色（4）和出错色（1）没配置时另有缺省。
pub fn ansi_colors(palette: &[(u8, Rgb)]) -> [Rgb; 16] {
    let mut ansi = runode_types::theme::ANSI;
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

/// 卡片样式下窗口的底色，衬在卡片后面：两种主题都往前景色混一点。浅色主题里比终端背景深一档，
/// 深色主题里比终端背景亮一档。
pub fn frame_color(fg: Rgb, bg: Rgb) -> Rgb {
    if is_light(fg, bg) { bg.mix(fg, 0.06) } else { bg.mix(fg, 0.04) }
}

/// 卡片样式下标签条的底色，以及条里当前标签那颗胶囊的颜色。浅色主题里条介于外框
/// 和卡片之间、胶囊就是卡片的白；深色主题里条和外框同色，胶囊亮出来。
pub fn tab_track_colors(fg: Rgb, bg: Rgb) -> (Rgb, Rgb) {
    let frame = frame_color(fg, bg);
    if is_light(fg, bg) { (frame.mix(bg, 0.5), bg) } else { (frame, bg.mix(fg, 0.11)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DARK: (Rgb, Rgb) = (Rgb(0xE6, 0xE1, 0xD8), Rgb(0x17, 0x16, 0x18));
    const LIGHT: (Rgb, Rgb) = (Rgb(0x1d, 0x1d, 0x1f), Rgb(0xff, 0xff, 0xff));

    fn palette() -> [Rgb; 16] {
        std::array::from_fn(|i| Rgb(i as u8 * 10, 0x40, 0x80))
    }

    /// 每个 token 逐值等于它的算式。
    #[test]
    fn tokens_match_the_colors_they_replace() {
        for (fg, bg) in [DARK, LIGHT] {
            let ansi = palette();
            let theme = Theme::new(fg, bg, ansi);
            let (f, b) = (hsla(fg), hsla(bg));
            // 窗口、卡片（`Card`）的底色和字色。
            assert_eq!((theme.background, theme.foreground), (b, f));
            assert_eq!(theme.card, b);
            // 菜单和各个浮层（`Popover`）的底色。
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
            // 分隔线和浮层的描边。
            assert_eq!(theme.border, f.opacity(0.09));
            assert_eq!(theme.input, f.opacity(0.15));
            assert_eq!(theme.destructive, hsla(ansi[1]));
            assert_eq!(theme.ring, hsla(ansi[4]));
            assert_eq!(theme.ansi, ansi);
            // 窗口外框和标签条。
            let luma = |c: Rgb| 0.299 * f32::from(c.0) + 0.587 * f32::from(c.1) + 0.114 * f32::from(c.2);
            let light = luma(bg) > luma(fg);
            assert_eq!(theme.is_light, light);
            let frame = if light { bg.mix(fg, 0.06) } else { bg.mix(fg, 0.04) };
            assert_eq!(theme.frame, hsla(frame));
            let (track, pill) = if light { (frame.mix(bg, 0.5), bg) } else { (frame, bg.mix(fg, 0.11)) };
            assert_eq!((theme.tab_track, theme.tab_active), (hsla(track), hsla(pill)));
            // 次一级的字和非终端的底面。
            assert_eq!(theme.subtle_foreground, f.opacity(0.7));
            let (surface, field) = if light { (bg, bg) } else { (bg.mix(fg, 0.07), bg.mix(fg, 0.016)) };
            assert_eq!((theme.surface, theme.field), (hsla(surface), hsla(field)));
            // 状态色是固定值，不跟调色板。
            let rgb = |c: u32| hsla(Rgb((c >> 16) as u8, (c >> 8) as u8, c as u8));
            let (warning, warning_text, info, success) =
                if light { (0xE8951F, 0x9A5B00, 0x2F6BDB, 0x1F8A3E) } else { (0xF5A83C, 0xF5A83C, 0x7FA9FF, 0x7FD49B) };
            assert_eq!((theme.warning, theme.warning_text), (rgb(warning), rgb(warning_text)));
            assert_eq!(theme.warning_foreground, rgb(0x1B1203));
            assert_eq!((theme.info, theme.success), (rgb(info), rgb(success)));
            for k in [0.04, 0.08, 0.10, 0.12, 0.14, 0.22] {
                assert_eq!(theme.mix(k), hsla(bg.mix(fg, k)));
            }
            assert_eq!(theme.mix_inverse(0.15), hsla(fg.mix(bg, 0.15)));
        }
        assert!(Theme::new(LIGHT.0, LIGHT.1, palette()).is_light);
        assert!(!Theme::new(DARK.0, DARK.1, palette()).is_light);
    }

    /// 调色板里后写的算数；强调色和出错色没配置时用缺省的蓝和红。
    #[test]
    fn ansi_colors_follow_the_palette() {
        let ansi = ansi_colors(&[(2, Rgb(1, 1, 1)), (2, Rgb(2, 2, 2)), (200, Rgb(3, 3, 3))]);
        assert_eq!(ansi[2], Rgb(2, 2, 2));
        assert_eq!(ansi[0], runode_types::theme::ANSI[0]);
        assert_eq!(ansi[4], RING_FALLBACK);
        assert_eq!(ansi[1], DESTRUCTIVE_FALLBACK);
        let ansi = ansi_colors(&[(1, Rgb(9, 9, 9)), (4, Rgb(8, 8, 8))]);
        assert_eq!((ansi[1], ansi[4]), (Rgb(9, 9, 9), Rgb(8, 8, 8)));
    }
}
