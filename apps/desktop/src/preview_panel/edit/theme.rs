//! 代码编辑器的配色：把窗口的主题换算成 gpui-component 的主题和高亮主题，编辑时和只读预览一个样子。
//!
//! shortcut: gpui-component 的主题全 app 只有一份，几个窗口配色不同时编辑器都跟着最前面那个窗口；它支持按
//! 窗口设主题后改成每个窗口各一份。

use std::sync::Arc;

use gpui::{App, Global, Hsla, Rgba};
use gpui_component::{Theme as EditorTheme, ThemeMode, highlighter::HighlightTheme};
use serde_json::{Value, json};

use crate::ui::{hsla, theme::Theme};

/// 上一次换算用的主题。
struct Applied(Theme);

impl Global for Applied {}

/// 编辑器的配色要按 `theme` 重算：还没算过，或者最前面的窗口（`active`）的主题变了。
pub(super) fn stale(theme: &Theme, active: bool, cx: &App) -> bool {
    match cx.try_global::<Applied>() {
        Some(applied) => active && applied.0 != *theme,
        None => true,
    }
}

/// 编辑器的配色换成 `theme` 的，所有窗口重画。不能在画的中途调。
pub(super) fn apply(theme: &Theme, cx: &mut App) {
    cx.set_global(Applied(theme.clone()));
    let mode = if theme.is_light { ThemeMode::Light } else { ThemeMode::Dark };
    // 换深浅会把颜色重置成那一档的缺省，所以先换，再盖上自己的。
    if EditorTheme::global(cx).mode != mode {
        EditorTheme::change(mode, None, cx);
    }
    let highlight = highlight_theme(theme);
    EditorTheme::update(cx, |editor| {
        let colors = &mut editor.colors;
        colors.background = theme.background;
        colors.foreground = theme.foreground;
        colors.muted = theme.muted;
        colors.muted_foreground = theme.muted_foreground;
        colors.popover = theme.popover;
        colors.popover_foreground = theme.popover_foreground;
        colors.accent = theme.accent_selected;
        colors.accent_foreground = theme.foreground;
        colors.secondary = theme.secondary;
        colors.border = theme.border;
        colors.input = theme.input;
        colors.ring = theme.ring;
        colors.caret = theme.foreground;
        colors.selection = theme.ring.opacity(0.3);
        colors.scrollbar = gpui::transparent_black();
        colors.scrollbar_thumb = theme.foreground.opacity(0.2);
        colors.scrollbar_thumb_hover = theme.foreground.opacity(0.35);
        if let Some(highlight) = highlight {
            editor.highlight_theme = Arc::new(highlight);
        }
    });
}

fn hex(color: Hsla) -> String {
    let Rgba { r, g, b, a } = color.into();
    let byte = |channel: f32| (channel * 255.).round() as u8;
    format!("#{:02x}{:02x}{:02x}{:02x}", byte(r), byte(g), byte(b), byte(a))
}

/// `theme` 下编辑器的高亮主题：底色、行号和各种语法记号的颜色。记号的颜色和 `runode_preview` 给只读预览
/// 定的一样，没列的记号用前景色。
fn highlight_theme(theme: &Theme) -> Option<HighlightTheme> {
    let ansi = |ix: usize| hex(hsla(theme.ansi[ix]));
    let (red, green, yellow, blue, magenta, cyan) = (ansi(1), ansi(2), ansi(3), ansi(4), ansi(5), ansi(6));
    let color = |color: &str| json!({ "color": color });
    let syntax: Value = json!({
        "comment": { "color": ansi(8), "font_style": "italic" },
        "string": color(&green),
        "string.escape": color(&cyan),
        "number": color(&cyan),
        "boolean": color(&cyan),
        "constant": color(&cyan),
        "keyword": color(&magenta),
        "function": color(&blue),
        "type": color(&yellow),
        "constructor": color(&yellow),
        "enum": color(&yellow),
        "variable.special": color(&red),
        "tag": color(&red),
        "label": color(&red),
        "attribute": color(&cyan),
        "preproc": color(&cyan),
        "title": { "color": blue, "font_weight": 700 },
        "text.literal": color(&green),
        "link_text": color(&cyan),
        "link_uri": color(&cyan),
        "punctuation.list_marker": color(&yellow),
        "emphasis": { "font_style": "italic" },
        "emphasis.strong": { "font_weight": 700 },
    });
    serde_json::from_value(json!({
        "name": "runode",
        "appearance": if theme.is_light { "light" } else { "dark" },
        "style": {
            "editor.background": hex(theme.background),
            "editor.foreground": hex(theme.foreground),
            "editor.active_line.background": hex(theme.muted),
            "editor.line_number": hex(theme.muted_foreground),
            "editor.active_line_number": hex(theme.foreground),
            "syntax": syntax,
        },
    }))
    .ok()
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::atomic::AtomicBool};

    use gpui_component::input::HighlightStyleResolver as _;
    use runode_types::color::Rgb;

    use super::*;
    use crate::preview_panel::body::highlight_style;

    /// 同一段代码里的注释、关键字、字符串和数字，编辑器和只读预览上的颜色一样。
    #[test]
    fn syntax_colors_match_the_read_only_preview() {
        let theme = Theme::new(Rgb(0xEE, 0xEE, 0xEE), Rgb(0x11, 0x11, 0x11), runode_types::theme::ANSI);
        let editor = highlight_theme(&theme).unwrap();
        let line = "// note\nlet text = \"a\";\nlet n = 42;".split('\n').map(str::to_owned).collect::<Vec<_>>();
        let spans = runode_preview::highlight(Path::new("a.rs"), &line, &AtomicBool::new(false)).unwrap();
        let preview = |row: usize, word: &str| {
            let at = line[row].find(word).unwrap();
            let span = spans[row].iter().find(|span| span.range.contains(&at)).unwrap();
            highlight_style(span.style, &theme)
        };
        for (name, row, word) in
            [("comment", 0, "note"), ("keyword", 1, "let"), ("string", 1, "a"), ("number", 2, "42")]
        {
            let (editor, preview) = (editor.style(name).unwrap(), preview(row, word));
            assert_eq!(editor.color.map(hex), preview.color.map(hex), "{name}");
            assert_eq!(editor.font_style, preview.font_style, "{name}");
        }
    }
}
