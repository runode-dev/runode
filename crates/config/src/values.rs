//! 把配置里的值按配置文件的写法写出来：配置模板里注释掉的默认值，和设置界面上显示的当前值，
//! 都从这里取，和 `Config::apply` 的解析互为逆运算。

use runode_shared_types::{
    clipboard::{ClipboardRead, ClipboardWrite},
    color::{Rgb, TerminalColor},
    settings::{CursorStyle, OptionAsAlt},
    shell::{IntegrationMode, Shell},
};

use crate::{CellHeight, Config, PreviewClick, TaskPlacement, WindowStyle};

impl Config {
    /// `key` 在这份配置里的值，按配置文件的写法每项一行，写回配置文件再读进来得到同样的值；
    /// 没有值（默认跟随别的设置或不设）时为空。`keybind` 和 `config-file` 读进来后不留原文，
    /// 这里总是空的，写了什么要看配置文件本身（`ConfigFile::values`）。
    pub fn values(&self, key: &str) -> Vec<String> {
        let pair = |(a, b): (f32, f32)| if a == b { a.to_string() } else { format!("{a},{b}") };
        let bool = |on: bool| vec![on.to_string()];
        let sound = |sound: &Option<String>| vec![sound.clone().unwrap_or_else(|| "none".into())];
        match key {
            "language" => self.language.iter().cloned().collect(),
            "font-family" => self.font_family.clone(),
            "font-size" => vec![self.font_size.to_string()],
            "adjust-cell-height" => match self.adjust_cell_height {
                None => Vec::new(),
                Some(CellHeight::Pixels(px)) => vec![px.to_string()],
                Some(CellHeight::Percent(percent)) => vec![format!("{percent}%")],
            },
            "window-padding-x" => vec![pair(self.window_padding_x)],
            "window-padding-y" => vec![pair(self.window_padding_y)],
            "status-bar" => bool(self.status_bar),
            "status-bar-hidden" => self.status_bar_hidden.iter().map(|item| item.name().to_owned()).collect(),
            "window-style" => vec![
                match self.window_style {
                    WindowStyle::Cards => "cards",
                    WindowStyle::Classic => "classic",
                }
                .into(),
            ],
            "file-tree-font-size" => vec![self.file_tree_font_size.to_string()],
            "file-tree-preview-click" => vec![
                match self.file_tree_preview_click {
                    PreviewClick::Single => "single",
                    PreviewClick::Double => "double",
                }
                .into(),
            ],
            "preview-font-size" => vec![self.preview_font_size.to_string()],
            "task-placement" => vec![
                match self.task_placement {
                    TaskPlacement::Right => "right",
                    TaskPlacement::Down => "down",
                    TaskPlacement::Tab => "tab",
                }
                .into(),
            ],
            "theme" => self.theme.iter().cloned().collect(),
            "background" => vec![hex(self.background)],
            "foreground" => vec![hex(self.foreground)],
            "cursor-color" => self.cursor_color.map(color).into_iter().collect(),
            "cursor-text" => self.cursor_text.map(color).into_iter().collect(),
            "selection-background" => self.selection_background.map(color).into_iter().collect(),
            "selection-foreground" => self.selection_foreground.map(color).into_iter().collect(),
            "search-background" => vec![color(self.search_background)],
            "search-foreground" => vec![color(self.search_foreground)],
            "search-selected-background" => vec![color(self.search_selected_background)],
            "search-selected-foreground" => vec![color(self.search_selected_foreground)],
            "palette" => self.palette.iter().map(|(i, c)| format!("{i}={}", hex(*c))).collect(),
            "cursor-style" => vec![
                match self.cursor_style {
                    CursorStyle::Block => "block",
                    CursorStyle::Bar => "bar",
                    CursorStyle::Underline => "underline",
                    CursorStyle::BlockHollow => "block_hollow",
                }
                .into(),
            ],
            "cursor-style-blink" => self.cursor_style_blink.map(|on| on.to_string()).into_iter().collect(),
            // 0 表示一直闪。
            "cursor-style-blink-timeout" => {
                vec![self.cursor_style_blink_timeout.map_or_else(|| "0".into(), |t| t.as_secs_f32().to_string())]
            }
            "macos-option-as-alt" => vec![
                match self.macos_option_as_alt {
                    OptionAsAlt::False => "false",
                    OptionAsAlt::True => "true",
                    OptionAsAlt::Left => "left",
                    OptionAsAlt::Right => "right",
                }
                .into(),
            ],
            "scrollback-limit" => vec![self.scrollback_limit.to_string()],
            "shell-integration" => vec![
                match self.shell_integration {
                    IntegrationMode::Detect => "detect",
                    IntegrationMode::Off => "none",
                    IntegrationMode::Force(Shell::Zsh) => "zsh",
                    IntegrationMode::Force(Shell::Bash) => "bash",
                    IntegrationMode::Force(Shell::Fish) => "fish",
                }
                .into(),
            ],
            "shell-integration-features" => {
                vec![if self.shell_integration_features.cursor { "cursor" } else { "no-cursor" }.into()]
            }
            "command-suggestions" => bool(self.command_suggestions),
            "command-completions" => bool(self.command_completions),
            "command-highlighting" => bool(self.command_highlighting),
            "clipboard-write" => vec![
                match self.clipboard_write {
                    ClipboardWrite::Allow => "allow",
                    ClipboardWrite::Deny => "deny",
                }
                .into(),
            ],
            "clipboard-read" => vec![
                match self.clipboard_read {
                    ClipboardRead::Allow => "allow",
                    ClipboardRead::Ask => "ask",
                    ClipboardRead::Deny => "deny",
                }
                .into(),
            ],
            "terminal-host" => bool(self.terminal_host),
            "auto-update" => bool(self.auto_update),
            "remote-access" => bool(self.remote_access),
            "remote-access-port" => vec![self.remote_access_port.to_string()],
            "remote-access-name" => self.remote_access_name.iter().cloned().collect(),
            "remote-access-push" => bool(self.remote_access_push),
            "remote-access-push-text" => bool(self.remote_access_push_text),
            "remote-access-push-delay" => vec![self.remote_access_push_delay.as_secs().to_string()],
            "apns-key-file" => self.apns_key_file.iter().cloned().collect(),
            "apns-key-id" => self.apns_key_id.iter().cloned().collect(),
            "apns-team-id" => self.apns_team_id.iter().cloned().collect(),
            "apns-bundle-id" => self.apns_bundle_id.iter().cloned().collect(),
            "push-relay-url" => vec![self.push_relay_url.clone()],
            "agent-notifications" => bool(self.agent_notifications),
            "agent-notifications-exclude" => {
                self.agent_notifications_exclude.iter().map(|kind| kind.label().to_owned()).collect()
            }
            "agent-done-sound" => sound(&self.agent_done_sound),
            "agent-blocked-sound" => sound(&self.agent_blocked_sound),
            _ => Vec::new(),
        }
    }
}

/// `#rrggbb`。
pub fn hex(Rgb(r, g, b): Rgb) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn color(color: TerminalColor) -> String {
    match color {
        TerminalColor::Rgb(c) => hex(c),
        TerminalColor::CellForeground => "cell-foreground".into(),
        TerminalColor::CellBackground => "cell-background".into(),
    }
}

#[cfg(test)]
mod tests {
    use crate::{Config, parse::KEYS, parse::tests::load};

    /// 每个键写出来的值再读进来，得到同样的配置。
    #[test]
    fn values_round_trip() {
        let config = load(&["font-family = A\nfont-family = B\nadjust-cell-height = 10%\nwindow-padding-x = 1,2\n\
             cursor-color = cell-foreground\ncursor-style = block_hollow\ncursor-style-blink = false\n\
             cursor-style-blink-timeout = 0\nmacos-option-as-alt = left\nshell-integration = fish\n\
             shell-integration-features = no-cursor\n\
             agent-notifications-exclude = codex,claude\nagent-done-sound = none\nlanguage = en\n\
             file-tree-preview-click = double\npalette = 3=#010203\nremote-access = true\n\
             remote-access-push = false\nremote-access-push-delay = 30\napns-key-file = ~/AuthKey_X.p8\n\
             apns-key-id = X\napns-team-id = T\napns-bundle-id = cn.example.app\npush-relay-url = https://relay.example\n\
             clipboard-write = deny\nclipboard-read = allow"]);
        let text: String = KEYS
            .iter()
            .flat_map(|group| group.iter())
            .flat_map(|key| config.values(key).into_iter().map(move |value| format!("{key} = {value}\n")))
            .collect();
        assert_eq!(load(&[&text]), config);
        assert_eq!(config.values("adjust-cell-height"), ["10%"]);
        assert_eq!(config.values("cursor-style-blink-timeout"), ["0"]);
        assert_eq!(config.values("clipboard-read"), ["allow"]);
        assert_eq!(Config::default().values("clipboard-write"), ["allow"]);
        assert!(Config::default().values("cursor-color").is_empty());
    }
}
