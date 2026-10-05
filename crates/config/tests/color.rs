//! 配置里的颜色写法：十六进制（带不带 `#`）、`rgb:` 和 `rgbi:`，以及各种写错的样子。

use runode_config::color::parse;
use runode_shared_types::color::Rgb;

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
