//! 界面上快捷键的写法，悬停提示、设置页和界面文字里提到的快捷键都照这里：一个键一个键帽，macOS 上修饰键用
//! ⌃⌥⇧⌘、连写（⌃⇧T），别处用 Ctrl、Alt、Shift 和 Win（Windows）或 Super（Linux），加号连起来（Ctrl+Shift+T）；
//! 连按的几个键用空格隔开。

use gpui::{KeybindingKeystroke, Modifiers};

/// Ctrl、Alt、Shift 和 Cmd（Windows 上是 Win 键，Linux 上是 Super 键）的键帽。
pub const MODIFIER_CAPS: [&str; 4] = if cfg!(target_os = "macos") {
    ["⌃", "⌥", "⇧", "⌘"]
} else if cfg!(windows) {
    ["Ctrl", "Alt", "Shift", "Win"]
} else {
    ["Ctrl", "Alt", "Shift", "Super"]
};

/// 一个按键的键帽：按下的修饰键 `m` 依次在前，最后是键本身 `key`。
pub fn caps(m: Modifiers, key: &str) -> Vec<String> {
    let [control, alt, shift, platform] = MODIFIER_CAPS;
    let mut caps: Vec<String> = [(m.control, control), (m.alt, alt), (m.shift, shift), (m.platform, platform)]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, symbol)| symbol.to_owned())
        .collect();
    caps.push(key_symbol(key));
    caps
}

/// 一个按键的键帽连成一段字。
pub fn join(caps: &[String]) -> String {
    caps.join(if cfg!(target_os = "macos") { "" } else { "+" })
}

/// 键位表里连按的几个键写成一段字，按它们显示用的修饰键和键（Windows 上按键盘布局换过）。
pub fn text(keystrokes: &[KeybindingKeystroke]) -> String {
    keystrokes
        .iter()
        .map(|keystroke| join(&caps(*keystroke.modifiers(), keystroke.key())))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 键本身的键帽：macOS 上回车、方向这些用符号，别处写名字；字母和别的键大写。
pub fn key_symbol(key: &str) -> String {
    if !cfg!(target_os = "macos") {
        let name = match key {
            "enter" => "Enter",
            "escape" => "Esc",
            "tab" => "Tab",
            "backspace" => "Backspace",
            "delete" => "Delete",
            "pageup" => "PgUp",
            "pagedown" => "PgDn",
            "home" => "Home",
            "end" => "End",
            "space" => "Space",
            _ => "",
        };
        if !name.is_empty() {
            return name.to_owned();
        }
    }
    let symbol = match key {
        "enter" => "↩",
        "escape" => "⎋",
        "tab" => "⇥",
        "backspace" => "⌫",
        "delete" => "⌦",
        "up" => "↑",
        "down" => "↓",
        "left" => "←",
        "right" => "→",
        "pageup" => "⇞",
        "pagedown" => "⇟",
        "home" => "↖",
        "end" => "↘",
        "space" => "␣",
        key => return key.to_uppercase(),
    };
    symbol.to_owned()
}
