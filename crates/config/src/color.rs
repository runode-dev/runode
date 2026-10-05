//! 配置里的颜色写法，与 Ghostty 配置和主题文件里的颜色语法相同。认这几种（前后的空格和
//! 制表符不算）：
//!
//! - `#rgb`、`#rrggbb`、`#rrrgggbbb`、`#rrrrggggbbbb`：每个分量 1 到 4 位十六进制；
//! - `rgb`、`rrggbb`：同上，省略 `#`，只限 3 位和 6 位；
//! - X11 颜色名，比如 `white`、`dark slate gray`、`DarkSlateGray`，不分大小写；
//! - `rgb:<红>/<绿>/<蓝>`：每个分量 1 到 4 位十六进制；
//! - `rgbi:<红>/<绿>/<蓝>`：每个分量是 0 到 1 之间的小数。
//!
//! 3 位、6 位的写法先当颜色名找，找不到再当十六进制。

use std::{collections::HashMap, sync::OnceLock};

use runode_model::color::Rgb;

/// X11 的颜色名表，一行一个：红绿蓝各占三列、空格分隔，从第 13 列起是名字。
static X11_COLORS: &str = include_str!("../x11/rgb.txt");

/// 解析一个颜色，认不出时为 `None`。
pub fn parse(value: &str) -> Option<Rgb> {
    let input = value.trim_matches([' ', '\t']);
    let bytes = input.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    if bytes[0] == b'#' {
        let digits = &bytes[1..];
        let width = match digits.len() {
            3 | 6 | 9 | 12 => digits.len() / 3,
            _ => return None,
        };
        return hex_triple(digits, width);
    }
    if let Some(rgb) = x11(input) {
        return Some(rgb);
    }
    match bytes.len() {
        3 => return hex_triple(bytes, 1),
        6 => return hex_triple(bytes, 2),
        _ => {}
    }
    if bytes.len() < "rgb:a/a/a".len() || !bytes.starts_with(b"rgb") {
        return None;
    }
    let (intensity, rest) = match &bytes[3..] {
        [b'i', b':', rest @ ..] => (true, rest),
        [b':', rest @ ..] => (false, rest),
        _ => return None,
    };
    // 红、绿后面各有一个 `/`，剩下的整个是蓝，蓝里再有 `/` 就不是数字。
    let mut parts = rest.splitn(3, |&b| b == b'/');
    let (r, g, b) = (parts.next()?, parts.next()?, parts.next()?);
    let channel = |part: &[u8]| if intensity { from_intensity(part) } else { from_hex(part) };
    Some(Rgb(channel(r)?, channel(g)?, channel(b)?))
}

/// 三个等宽的十六进制分量依次排开。
fn hex_triple(digits: &[u8], width: usize) -> Option<Rgb> {
    let channel = |i: usize| from_hex(&digits[i * width..(i + 1) * width]);
    Some(Rgb(channel(0)?, channel(1)?, channel(2)?))
}

/// 1 到 4 位十六进制，分别是 4、8、12、16 位精度，按比例换到 0–255，向下取整。
fn from_hex(digits: &[u8]) -> Option<u8> {
    if digits.is_empty() || digits.len() > 4 || !digits.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let value = digits.iter().fold(0usize, |acc, &d| acc * 16 + char::from(d).to_digit(16).unwrap() as usize);
    let max = (1usize << (4 * digits.len())) - 1;
    Some((value * 255 / max) as u8)
}

/// 0 到 1 之间的小数换到 0–255，向下取整。
fn from_intensity(text: &[u8]) -> Option<u8> {
    Some((fraction(text)? * 255.) as u8)
}

/// 0 到 1 之间（含两端）的十进制小数：可带正负号，整数部分和小数部分都可省略但至少有一位
/// 数字，不认指数和十六进制；超出范围的（`-0` 除外）都不认。小数部分只取前 15 位，
/// 更多的位只检查是不是数字，这样除法只舍入一次。
fn fraction(text: &[u8]) -> Option<f64> {
    let (negative, text) = match text {
        [b'+', rest @ ..] => (false, rest),
        [b'-', rest @ ..] => (true, rest),
        _ => (false, text),
    };
    let (int_digits, frac_digits) = match text.iter().position(|&b| b == b'.') {
        Some(dot) => (&text[..dot], Some(&text[dot + 1..])),
        None => (text, None),
    };
    let mut int_part = 0f64;
    for &d in int_digits {
        if !d.is_ascii_digit() {
            return None;
        }
        int_part = int_part * 10. + f64::from(d - b'0');
    }
    let mut digits = int_digits.len();
    let (mut frac, mut scale) = (0u64, 1u64);
    for &d in frac_digits.unwrap_or_default() {
        if !d.is_ascii_digit() {
            return None;
        }
        if scale < 1_000_000_000_000_000 {
            frac = frac * 10 + u64::from(d - b'0');
            scale *= 10;
        }
        digits += 1;
    }
    if digits == 0 {
        return None;
    }
    let magnitude = int_part + frac as f64 / scale as f64;
    let result = if negative { -magnitude } else { magnitude };
    (0. ..=1.).contains(&result).then_some(result)
}

/// 按名字找 X11 颜色，不分大小写。
fn x11(name: &str) -> Option<Rgb> {
    static TABLE: OnceLock<HashMap<String, Rgb>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        X11_COLORS
            .lines()
            .map(|line| line.trim_end_matches('\r'))
            .filter(|line| !line.is_empty())
            .map(|line| {
                let channel = |range: std::ops::Range<usize>| line[range].trim().parse::<u8>().unwrap();
                let rgb = Rgb(channel(0..3), channel(4..7), channel(8..11));
                (line[12..].trim_matches([' ', '\t']).to_ascii_lowercase(), rgb)
            })
            .collect()
    });
    table.get(&name.to_ascii_lowercase()).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(r: u8, g: u8, b: u8) -> Option<Rgb> {
        Some(Rgb(r, g, b))
    }

    #[test]
    fn hex_with_and_without_hash() {
        assert_eq!(parse("#ffffff"), rgb(255, 255, 255));
        assert_eq!(parse("#fff"), rgb(255, 255, 255));
        assert_eq!(parse("#fffffffff"), rgb(255, 255, 255));
        assert_eq!(parse("#ffffffffffff"), rgb(255, 255, 255));
        assert_eq!(parse("#ff0010"), rgb(255, 0, 16));
        assert_eq!(parse("#345"), rgb(51, 68, 85));
        assert_eq!(parse("#800080008000"), rgb(127, 127, 127));
        assert_eq!(parse("#7ff000000"), rgb(127, 0, 0));
        assert_eq!(parse(" #AABBCC   "), rgb(170, 187, 204));
        assert_eq!(parse("0A0B0C"), rgb(10, 11, 12));
        assert_eq!(parse("FFFFFF"), rgb(255, 255, 255));
        assert_eq!(parse("FFF"), rgb(255, 255, 255));
        // 不带 `#` 的只有 3 位和 6 位。
        assert_eq!(parse("fffffffff"), None);
    }

    #[test]
    fn x11_names() {
        assert_eq!(parse("black"), rgb(0, 0, 0));
        assert_eq!(parse("red"), rgb(255, 0, 0));
        assert_eq!(parse("green"), rgb(0, 255, 0));
        assert_eq!(parse("blue"), rgb(0, 0, 255));
        assert_eq!(parse("white"), rgb(255, 255, 255));
        assert_eq!(parse("LawnGreen"), rgb(124, 252, 0));
        assert_eq!(parse("medium spring green"), rgb(0, 250, 154));
        assert_eq!(parse(" Forest Green "), rgb(34, 139, 34));
        assert_eq!(parse("\tForestGreen\t"), rgb(34, 139, 34));
        assert_eq!(parse("FoReStGReen"), rgb(34, 139, 34));
        assert_eq!(parse("snow"), rgb(255, 250, 250));
        // 三个字母的颜色名先于十六进制。
        assert_eq!(parse("tan"), rgb(210, 180, 140));
        assert_eq!(parse("nosuchcolor"), None);
        assert!(x11("ghost white").is_some() && x11("ghostwhite").is_some());
    }

    #[test]
    fn x11_table_is_complete() {
        let names = X11_COLORS.lines().filter(|l| !l.is_empty()).count();
        assert!(names > 700, "{names}");
        for line in X11_COLORS.lines().filter(|l| !l.is_empty()) {
            let name = line[12..].trim();
            assert!(x11(name).is_some(), "{name}");
        }
    }

    #[test]
    fn rgb_specs() {
        assert_eq!(parse("rgb:7f/a0a0/0"), rgb(127, 160, 0));
        assert_eq!(parse("rgb:f/ff/fff"), rgb(255, 255, 255));
        assert_eq!(parse("rgb:12/34/56"), rgb(0x12, 0x34, 0x56));
        assert_eq!(parse("rgbi:1.0/0/0"), rgb(255, 0, 0));
        assert_eq!(parse("rgbi:0.5/.5/+1."), rgb(127, 127, 255));
        assert_eq!(parse("rgbi:-0/0/0"), rgb(0, 0, 0));
    }

    #[test]
    fn rejects_malformed_colors() {
        for value in [
            "",
            "  ",
            "rgb;",
            "rgb:",
            ":a/a/a",
            "a/a/a",
            "rgb:a/a/a/",
            "rgb:00000///",
            "rgb:000/",
            "rgbi:a/a/a",
            "rgb:0.5/0.0/1.0",
            "rgb:not/hex/zz",
            "rgb:f_f/0/0",
            "rgb:+f/0/0",
            "RGB:f/f/f",
            "rgbi:1.5/0/0",
            "rgbi:-0.5/0/0",
            "rgbi:1e-1/0/0",
            "rgbi:./0/0",
            "#",
            "#ff",
            "#ffff",
            "#fffff",
            "#gggggg",
            "#12345",
            "#f_f000000",
            "#+ff",
            "12345",
            "+ff",
            "#ééé",
        ] {
            assert_eq!(parse(value), None, "{value:?}");
        }
    }

    #[test]
    fn fractions() {
        assert_eq!(fraction(b"0"), Some(0.));
        assert_eq!(fraction(b"1"), Some(1.));
        assert_eq!(fraction(b"0.25"), Some(0.25));
        assert_eq!(fraction(b"1.000000"), Some(1.));
        assert_eq!(fraction(b"00.00"), Some(0.));
        assert_eq!(fraction(b"0.3"), Some(0.3));
        assert_eq!(fraction(b"0.123456789012345"), Some(0.123456789012345));
        assert_eq!(fraction(format!("0.{}", "3".repeat(400)).as_bytes()), fraction(b"0.333333333333333"));
        for bad in ["", ".", "+", "-", "+.", "abc", "0.5x", "0..5", " 0.5", "1.0000001", "2", "nan", "-1"] {
            assert_eq!(fraction(bad.as_bytes()), None, "{bad:?}");
        }
        assert_eq!(fraction("1".repeat(400).as_bytes()), None);
    }
}
