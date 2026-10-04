//! 没有配置时使用的默认配色。

use libghostty_vt::style::RgbColor;

const fn rgb(hex: u32) -> RgbColor {
    RgbColor {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
    }
}

pub const BACKGROUND: RgbColor = rgb(0x0F0D0E);
pub const FOREGROUND: RgbColor = rgb(0xFFFFFF);

/// ANSI 16 色：0–7 为普通色，8–15 为亮色。其余 240 色沿用终端标准的 256 色默认值。
pub const ANSI: [RgbColor; 16] = [
    rgb(0x393A3D), // black
    rgb(0xFF1261), // red
    rgb(0x2AD947), // green
    rgb(0xFCBA28), // yellow
    rgb(0x2D9AFF), // blue
    rgb(0xDD30FF), // magenta
    rgb(0x17D5DF), // cyan
    rgb(0xE7E7E7), // white
    rgb(0x6B6B6B), // bright black
    rgb(0xC55555), // bright red
    rgb(0xAAC474), // bright green
    rgb(0xFECA88), // bright yellow
    rgb(0x82B8C8), // bright blue
    rgb(0xC28CB8), // bright magenta
    rgb(0x93D3C3), // bright cyan
    rgb(0xF8F8F8), // bright white
];
