//! 语法高亮：按文件名、扩展名或第一行认出语言，逐行给出各段文字用调色板里的哪种颜色。
//!
//! 配色表只把作用域映射到 ANSI 16 色的序号，颜色值本身在 syntect 的 `Color` 里编码：
//! 透明度为 0 时红色分量是 ANSI 序号，否则是默认前景色。

use std::{
    ops::Range,
    path::Path,
    str::FromStr as _,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use syntect::{
    easy::HighlightLines,
    highlighting::{self, FontStyle, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings},
    parsing::{SyntaxReference, SyntaxSet},
};

/// 比这长的行多半是压缩过的代码，逐字匹配太慢，这样的行不高亮。
const MAX_LINE_BYTES: usize = 4096;
/// 每高亮这么多行看一次是否已经取消。
const CANCEL_CHECK_LINES: usize = 128;

/// 调色板里的颜色。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Color {
    /// 默认前景色。
    Foreground,
    /// ANSI 16 色的序号，0 到 15。
    Ansi(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Style {
    pub color: Color,
    pub bold: bool,
    pub italic: bool,
}

/// 一行里的一段，范围是这一行里的字节位置。没列出的部分用默认前景色。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    pub style: Style,
}

const FOREGROUND: highlighting::Color = highlighting::Color { r: 0, g: 0, b: 0, a: 0xFF };

fn ansi(index: u8) -> highlighting::Color {
    highlighting::Color { r: index, g: 0, b: 0, a: 0 }
}

fn decode(color: highlighting::Color) -> Color {
    if color.a == 0 { Color::Ansi(color.r.min(15)) } else { Color::Foreground }
}

const BLACK_BRIGHT: u8 = 8;
const RED: u8 = 1;
const GREEN: u8 = 2;
const YELLOW: u8 = 3;
const BLUE: u8 = 4;
const MAGENTA: u8 = 5;
const CYAN: u8 = 6;

/// 作用域到颜色和字形的映射。同一段文字匹配多条时 syntect 取最具体的那条。
const RULES: &[(&str, Option<u8>, FontStyle)] = &[
    ("comment, punctuation.definition.comment", Some(BLACK_BRIGHT), FontStyle::ITALIC),
    ("string, punctuation.definition.string", Some(GREEN), FontStyle::empty()),
    ("constant.character.escape, constant.other.placeholder", Some(CYAN), FontStyle::empty()),
    ("constant.numeric, constant.language, constant.character, constant.other, support.constant", Some(CYAN), FontStyle::empty()),
    ("keyword, storage, keyword.control, storage.type, storage.modifier", Some(MAGENTA), FontStyle::empty()),
    ("keyword.operator", None, FontStyle::empty()),
    (
        "entity.name.function, support.function, variable.function, meta.function-call variable.function, support.macro, entity.name.macro",
        Some(BLUE),
        FontStyle::empty(),
    ),
    (
        "entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, entity.name.trait, entity.name.interface, entity.name.union, entity.other.inherited-class, support.type, support.class",
        Some(YELLOW),
        FontStyle::empty(),
    ),
    ("variable.language, entity.name.tag, support.type.property-name, meta.mapping.key string", Some(RED), FontStyle::empty()),
    ("entity.other.attribute-name", Some(YELLOW), FontStyle::empty()),
    (
        "meta.attribute, meta.annotation, entity.name.function.decorator, punctuation.definition.annotation, meta.preprocessor, keyword.other.preprocessor",
        Some(CYAN),
        FontStyle::empty(),
    ),
    ("markup.heading, markup.heading entity.name", Some(BLUE), FontStyle::BOLD),
    ("markup.bold", None, FontStyle::BOLD),
    ("markup.italic", None, FontStyle::ITALIC),
    ("markup.raw, markup.inline.raw", Some(GREEN), FontStyle::empty()),
    ("markup.underline.link, markup.link, string.other.link", Some(CYAN), FontStyle::empty()),
    ("markup.quote", Some(BLACK_BRIGHT), FontStyle::ITALIC),
    ("punctuation.definition.list, markup.list punctuation.definition", Some(YELLOW), FontStyle::empty()),
    ("markup.inserted", Some(GREEN), FontStyle::empty()),
    ("markup.deleted, invalid", Some(RED), FontStyle::empty()),
    ("markup.changed", Some(YELLOW), FontStyle::empty()),
];

fn theme() -> Theme {
    let scopes = RULES
        .iter()
        .map(|&(scope, color, font_style)| ThemeItem {
            scope: ScopeSelectors::from_str(scope).expect("scope selectors in RULES parse"),
            style: StyleModifier { foreground: color.map(ansi), background: None, font_style: Some(font_style) },
        })
        .collect();
    Theme { settings: ThemeSettings { foreground: Some(FOREGROUND), ..ThemeSettings::default() }, scopes, ..Theme::default() }
}

/// 语法定义和配色表，第一次用时加载，几十毫秒，所以只在后台线程里用。
fn assets() -> &'static (SyntaxSet, Theme) {
    static ASSETS: OnceLock<(SyntaxSet, Theme)> = OnceLock::new();
    ASSETS.get_or_init(|| (two_face::syntax::extra_newlines(), theme()))
}

/// 先按完整文件名（`Dockerfile`、`Makefile` 这类），再按扩展名，最后按第一行（`#!` 之类）找语言。
fn find_syntax<'a>(set: &'a SyntaxSet, path: &Path, first_line: &str) -> Option<&'a SyntaxReference> {
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    let ext = path.extension().and_then(|ext| ext.to_str());
    Some(name)
        .filter(|name| !name.is_empty())
        .and_then(|name| set.find_syntax_by_extension(name))
        .or_else(|| ext.and_then(|ext| set.find_syntax_by_extension(ext)))
        .or_else(|| set.find_syntax_by_first_line(first_line))
        .filter(|syntax| syntax.name != "Plain Text")
}

/// 认出的语言名，认不出时为空。
pub fn syntax_name(path: &Path, first_line: &str) -> Option<&'static str> {
    let (set, _) = assets();
    find_syntax(set, path, first_line).map(|syntax| syntax.name.as_str())
}

/// 逐行高亮 `lines`，返回和 `lines` 一样多的行，每行里非默认样式的若干段。认不出语言或者
/// `cancel` 被置上时为空。长于 `MAX_LINE_BYTES` 的行和高亮出错的行没有分段，从下一行起
/// 按文件开头的状态重新高亮，所以长行后面的行照样有颜色，只是跨行的结构（比如多行注释）
/// 可能断开。
pub fn highlight(path: &Path, lines: &[String], cancel: &AtomicBool) -> Option<Vec<Vec<Span>>> {
    let (set, theme) = assets();
    let syntax = find_syntax(set, path, lines.first().map_or("", String::as_str))?;
    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut result = Vec::with_capacity(lines.len());
    let mut buffer = String::new();
    for (ix, line) in lines.iter().enumerate() {
        if ix % CANCEL_CHECK_LINES == 0 && cancel.load(Ordering::Relaxed) {
            return None;
        }
        if line.len() > MAX_LINE_BYTES {
            result.push(Vec::new());
            highlighter = HighlightLines::new(syntax, theme);
            continue;
        }
        buffer.clear();
        buffer.push_str(line);
        buffer.push('\n');
        let Ok(pieces) = highlighter.highlight_line(&buffer, set) else {
            result.push(Vec::new());
            highlighter = HighlightLines::new(syntax, theme);
            continue;
        };
        let mut spans: Vec<Span> = Vec::new();
        let mut start = 0;
        for (style, piece) in pieces {
            let end = (start + piece.len()).min(line.len());
            let style = Style {
                color: decode(style.foreground),
                bold: style.font_style.contains(FontStyle::BOLD),
                italic: style.font_style.contains(FontStyle::ITALIC),
            };
            let plain = style.color == Color::Foreground && !style.bold && !style.italic;
            if start < end && !plain {
                match spans.last_mut() {
                    Some(last) if last.range.end == start && last.style == style => last.range.end = end,
                    _ => spans.push(Span { range: start..end, style }),
                }
            }
            start += piece.len();
        }
        result.push(spans);
    }
    result.resize_with(lines.len(), Vec::new);
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    #[test]
    fn comments_and_strings_get_their_colors() {
        let spans = highlight(Path::new("a.rs"), &lines("// note\nlet s = \"x\";"), &AtomicBool::new(false)).unwrap();
        assert_eq!(spans[0][0].range, 0..7);
        assert_eq!(spans[0][0].style, Style { color: Color::Ansi(BLACK_BRIGHT), bold: false, italic: true });
        assert!(spans[1].iter().any(|span| span.style.color == Color::Ansi(GREEN) && span.range == (8..11)));
    }

    /// 编进二进制的语法定义里，要求附带许可声明的那些都抄在 `LICENSE-syntaxes` 里。升级
    /// two-face 后这里失败时，设上 `RUNODE_UPDATE_LICENSES=1` 再跑一次，文件会按新的清单重写。
    #[test]
    fn syntax_licenses_are_listed() {
        let listing = two_face::acknowledgement::listing();
        // 同一份许可文字（比如 Apache 2.0 全文）只抄一次，前面列出用它的各个路径。
        let mut groups: Vec<(Vec<String>, &str)> = Vec::new();
        for license in listing.for_syntaxes().iter().filter(|license| license.needs_acknowledgement()) {
            let path = license.rel_path.display().to_string();
            let text = license.text.trim_end();
            match groups.iter_mut().find(|(_, existing)| *existing == text) {
                Some((paths, _)) => paths.push(path),
                None => groups.push((vec![path], text)),
            }
        }
        let mut expected = String::from(LICENSES_HEADER);
        for (paths, text) in groups {
            expected.push_str("\n================================================================\n");
            for path in paths {
                expected.push_str(&path);
                expected.push('\n');
            }
            expected.push('\n');
            expected.push_str(text);
            expected.push('\n');
        }
        let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("LICENSE-syntaxes");
        let actual = std::fs::read_to_string(&file).unwrap_or_default();
        if actual != expected && std::env::var_os("RUNODE_UPDATE_LICENSES").is_some() {
            std::fs::write(&file, &expected).unwrap();
            return;
        }
        assert!(actual == expected, "LICENSE-syntaxes is out of date; rerun with RUNODE_UPDATE_LICENSES=1");
    }

    const LICENSES_HEADER: &str = "\
文件预览的语法高亮用到的语法定义取自 two-face（https://codeberg.org/CosmicHarper/two-face），
随 runode 的二进制一起分发。下面是其中要求附带许可声明的语法定义，每段先列出它们在 two-face
语法集里的路径，再附上许可全文。这份文件由 runode-preview 的测试 syntax_licenses_are_listed
生成并核对。
";

    #[test]
    fn skips_very_long_lines_and_goes_on() {
        let mut text = lines("let a = 1;");
        text.push("x".repeat(MAX_LINE_BYTES + 1));
        text.push("let b = 2;".to_owned());
        let spans = highlight(Path::new("a.rs"), &text, &AtomicBool::new(false)).unwrap();
        assert_eq!(spans.len(), 3);
        assert!(!spans[0].is_empty());
        assert!(spans[1].is_empty());
        assert!(!spans[2].is_empty());
    }
}
