//! 配置的内容：各项设置和默认值。

use std::{path::PathBuf, time::Duration};

use runode_types::{
    agent::AgentKind,
    clipboard::{ClipboardAccess, ClipboardRead, ClipboardWrite},
    color::{Rgb, TerminalColor},
    settings::{CursorStyle, OptionAsAlt, TermSettings},
    shell::{IntegrationMode, ShellFeatures},
    speech::{DictationCloud, SpeechModel},
};

use crate::keybind::Action;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CellHeight {
    Pixels(f32),
    Percent(f32),
}

/// 文件树里单击还是双击文件在预览栏打开。单击打开时开成临时标签，双击固定下来。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewClick {
    Single,
    Double,
}

/// 标题栏命令菜单里点了一条命令时，在哪开跑它的新终端。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskPlacement {
    /// 当前标签里，分在当前终端右边。
    Right,
    /// 当前标签里，分在当前终端下边。
    Down,
    /// 当前标签右边的新标签。
    Tab,
}

/// 窗口的样子：卡片是每个分屏、右侧面板各是一张圆角卡片，衬在比终端深一档的外框上，分屏顶上有
/// 标题条，标签是胶囊；经典是终端铺满窗口，分屏之间一条细线，标签是
/// 平铺的格子。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowStyle {
    Cards,
    Classic,
}

/// 窗口底部状态栏上的一块，`status-bar-hidden` 里写它的名字（`name`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusItem {
    /// 防止休眠。
    Sleep,
    /// runode 占的内存和终端数。
    Resources,
    /// 终端里的程序在监听的端口。
    Ports,
}

impl StatusItem {
    /// 按状态栏上从左到右的顺序。
    pub const ALL: [Self; 3] = [Self::Sleep, Self::Resources, Self::Ports];

    pub fn name(self) -> &'static str {
        match self {
            Self::Sleep => "sleep",
            Self::Resources => "resources",
            Self::Ports => "ports",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// 依次尝试的字体族，第一个能解析的生效。
    pub font_family: Vec<String>,
    pub font_size: f32,
    pub adjust_cell_height: Option<CellHeight>,
    /// (左, 右)
    pub window_padding_x: (f32, f32),
    /// (上, 下)
    pub window_padding_y: (f32, f32),
    pub window_style: WindowStyle,
    /// 界面文字的基准字号：终端和预览栏以外的字都从它按比例推出来。
    pub ui_font_size: f32,
    /// 窗口底部的状态栏显示不显示。
    pub status_bar: bool,
    /// 状态栏上不显示的几块。
    pub status_bar_hidden: Vec<StatusItem>,
    pub file_tree_preview_click: PreviewClick,
    /// 预览栏的字号。
    pub preview_font_size: f32,
    pub task_placement: TaskPlacement,
    /// 命令菜单里 Makefile、package.json 的分组一开始收着。
    pub task_files_collapsed: bool,
    pub dictation_model: SpeechModel,
    /// 热词：交给云端识别时帮它认出专有名词，润色时也交给大模型。
    pub dictation_words: Vec<String>,
    pub dictation_cloud: DictationCloud,
    /// OpenAI 兼容 API 的根，后面接 `/audio/transcriptions`。
    pub dictation_openai_url: String,
    pub dictation_openai_key: Option<String>,
    pub dictation_openai_model: String,
    /// 豆包旧版控制台的 App ID；为空时 `dictation_doubao_key` 是新版控制台的 API Key。
    pub dictation_doubao_app_id: Option<String>,
    pub dictation_doubao_key: Option<String>,
    /// 润色用的 OpenAI 兼容 API 的根；为空时不润色。
    pub dictation_polish_url: Option<String>,
    pub dictation_polish_key: Option<String>,
    pub dictation_polish_model: Option<String>,
    /// 配置里写的 `theme`，原样保留（可能是 `light:A,dark:B`）；没写时为 `None`。
    pub theme: Option<String>,
    pub cursor_style: CursorStyle,
    /// `None` 表示默认不闪烁，运行中的程序仍可改变。
    pub cursor_style_blink: Option<bool>,
    /// 光标闪烁时，没有键盘输入、终端没有输出、窗口没有重新获得焦点这么久之后停止闪烁、常亮，
    /// 有了就立刻恢复；`None` 表示一直闪。光标不闪时用不上。
    pub cursor_style_blink_timeout: Option<Duration>,
    pub background: Rgb,
    pub foreground: Rgb,
    /// `None` 表示用前景色。
    pub cursor_color: Option<TerminalColor>,
    /// 实心块状光标下文字的颜色，`None` 表示用背景色。
    pub cursor_text: Option<TerminalColor>,
    /// `None` 表示按背景深浅用一种统一的蓝色。
    pub selection_background: Option<TerminalColor>,
    /// `None` 时：配了选区底色就取单元格的背景色（底色设成单元格前景色时就是反色），
    /// 没配就保持文字原来的颜色。
    pub selection_foreground: Option<TerminalColor>,
    /// 搜索匹配的背景色和文字色。
    pub search_background: TerminalColor,
    pub search_foreground: TerminalColor,
    /// 当前选中的那个搜索匹配的背景色和文字色。
    pub search_selected_background: TerminalColor,
    pub search_selected_foreground: TerminalColor,
    /// 覆盖默认 256 色中的若干项。
    pub palette: Vec<(u8, Rgb)>,
    /// 文字和底色的对比度（WCAG 2.0 的算法，1 到 21）至少要多少，不够时画字时把字调深或调浅；
    /// 1 表示不调。
    pub minimum_contrast: f32,
    pub macos_option_as_alt: OptionAsAlt,
    /// 每个终端的回滚历史最多占多少字节。
    pub scrollback_limit: usize,
    pub shell_integration: IntegrationMode,
    /// 新启动的 shell 的集成开哪些功能。
    pub shell_integration_features: ShellFeatures,
    /// 终端里的程序用 OSC 52 写剪贴板时照写还是丢掉。
    pub clipboard_write: ClipboardWrite,
    /// 终端里的程序用 OSC 52 读剪贴板时照读、先问还是回空的。
    pub clipboard_read: ClipboardRead,
    /// 终端里的程序可以用 CSI 8 t 改窗口尺寸；改不改还要看窗口状态，见 `WindowView` 处理
    /// `TerminalEvent::ResizeWindow` 的地方。
    pub vt_window_resize_allowed: bool,
    /// 退出 app 后会话留在单独一个进程的宿主（`runode --host`）里，下次打开接着用；关着时退出 app
    /// 让宿主连会话一起退出。改了马上算数。
    pub terminal_host: bool,
    /// 启动后和之后每隔几个小时查一次新版本，有了就在后台下载好、退出时装上；关着时只在菜单里点
    /// 「检查更新」时查。只有用 Developer ID 签过名的 Runode.app 会自己更新。
    pub auto_update: bool,
    /// 让手机这类别的设备经网络连上宿主（远程访问）。改了几秒内生效，不用重启 app。
    pub remote_access: bool,
    /// 远程访问监听的 TCP 端口。
    pub remote_access_port: u16,
    /// 配对过的手机上显示的这台电脑的名字；`None` 时用系统设置里的电脑名。
    pub remote_access_name: Option<String>,
    /// agent 等回答时给配对过、登记了推送的手机推一条通知。
    pub remote_access_push: bool,
    /// 推送带屏幕上的问题和选项；关着时只有标题和 agent。
    pub remote_access_push_text: bool,
    /// agent 等回答过了这么久还在等才推。
    pub remote_access_push_delay: Duration,
    /// 直连 APNs 用的 .p8 密钥文件，`~/` 开头时从家目录算；和下面三项都配了才直连，推给 bundle id
    /// 是 `apns_bundle_id` 的 App。
    pub apns_key_file: Option<String>,
    pub apns_key_id: Option<String>,
    pub apns_team_id: Option<String>,
    pub apns_bundle_id: Option<String>,
    /// 界面语言的标签，照配置里写的；`None` 表示跟随系统。
    pub language: Option<String>,
    /// agent 等用户回答或者干完了、用户又没在看那个分屏时，发系统通知。
    pub agent_notifications: bool,
    /// 这些 agent 不发通知也不出提示音，标记照常显示。
    pub agent_notifications_exclude: Vec<AgentKind>,
    /// agent 在自己的终端里用 `runode open --right`、`--down` 分出的分屏默认折叠，不抢终端区的地方；
    /// 命令行的 `--fold`、`--no-fold` 优先。
    pub agent_split_fold: bool,
    /// agent 干完了、等用户回答时播放的系统声音名；`None` 表示不出声。
    pub agent_done_sound: Option<String>,
    pub agent_blocked_sound: Option<String>,
    /// 叠在默认快捷键上的 `keybind`，按出现顺序；只认 runode 自己的配置文件。
    pub keybinds: Vec<Keybind>,
    /// 本次读到的全部文件（含主题和 config-file 引入的），供热重载监视。
    pub sources: Vec<PathBuf>,
    /// 加载时系统是否为深色外观，`theme = light:A,dark:B` 据此选了其中一个。
    pub dark: bool,
    /// 主题按系统外观选（`theme = light:A,dark:B`，两边不是同一个）：外观不同时加载出来的配置不同。
    pub theme_follows_appearance: bool,
}

impl Default for Config {
    fn default() -> Self {
        // 交给终端的那部分沿用 `TermSettings` 的默认值，只在一处写。
        let term = TermSettings::default();
        Self {
            font_family: vec!["Hack Nerd Font Mono".into()],
            font_size: 14.,
            adjust_cell_height: None,
            window_padding_x: (2., 2.),
            window_padding_y: (0., 6.),
            window_style: WindowStyle::Cards,
            ui_font_size: 12.,
            status_bar: true,
            status_bar_hidden: Vec::new(),
            file_tree_preview_click: PreviewClick::Single,
            preview_font_size: 13.,
            task_placement: TaskPlacement::Right,
            task_files_collapsed: true,
            dictation_model: SpeechModel::SenseVoice,
            dictation_words: Vec::new(),
            dictation_cloud: DictationCloud::None,
            dictation_openai_url: "https://api.openai.com/v1".into(),
            dictation_openai_key: None,
            dictation_openai_model: "gpt-4o-transcribe".into(),
            dictation_doubao_app_id: None,
            dictation_doubao_key: None,
            dictation_polish_url: None,
            dictation_polish_key: None,
            dictation_polish_model: None,
            theme: None,
            cursor_style: term.cursor_style,
            cursor_style_blink: term.cursor_blink,
            cursor_style_blink_timeout: Some(Duration::from_secs(5)),
            background: term.background,
            foreground: term.foreground,
            cursor_color: term.cursor_color,
            cursor_text: term.cursor_text,
            selection_background: term.selection_background,
            selection_foreground: term.selection_foreground,
            search_background: term.search_background,
            search_foreground: term.search_foreground,
            search_selected_background: term.search_selected_background,
            search_selected_foreground: term.search_selected_foreground,
            palette: term.palette,
            minimum_contrast: 3.,
            macos_option_as_alt: term.option_as_alt,
            scrollback_limit: term.scrollback_limit,
            shell_integration: IntegrationMode::Detect,
            shell_integration_features: term.shell_features,
            clipboard_write: ClipboardWrite::default(),
            clipboard_read: ClipboardRead::default(),
            terminal_host: false,
            auto_update: true,
            vt_window_resize_allowed: false,
            remote_access: false,
            // 和 `runode_protocol::remote::DEFAULT_PORT` 一样；config 不依赖 protocol，桌面的测试对着两边。
            remote_access_port: 7866,
            remote_access_name: None,
            remote_access_push: true,
            remote_access_push_text: true,
            remote_access_push_delay: Duration::from_secs(10),
            apns_key_file: None,
            apns_key_id: None,
            apns_team_id: None,
            apns_bundle_id: None,
            language: None,
            agent_notifications: true,
            agent_notifications_exclude: Vec::new(),
            agent_split_fold: true,
            agent_done_sound: Some("Glass".into()),
            agent_blocked_sound: Some("Ping".into()),
            keybinds: Vec::new(),
            sources: Vec::new(),
            dark: true,
            theme_follows_appearance: false,
        }
    }
}

/// 一条 `keybind`，由 `keybind::parse` 解析并校验。
#[derive(Clone, Debug, PartialEq)]
pub enum Keybind {
    /// `keybind = clear`：去掉此前的全部绑定，包括默认的。
    Clear,
    /// `触发键=unbind`，触发键已转成 GPUI 的写法。
    Unbind(String),
    /// `触发键=动作`，动作已解析好。
    Bind { keys: String, action: Action },
}

impl Config {
    /// 交给宿主的读写剪贴板的规矩，见 `ClientMsg::SetOptions`。
    pub fn clipboard_access(&self) -> ClipboardAccess {
        ClipboardAccess { write: self.clipboard_write, read: self.clipboard_read }
    }

    /// 交给终端的那部分设置。
    pub fn term_settings(&self) -> TermSettings {
        TermSettings {
            background: self.background,
            foreground: self.foreground,
            palette: self.palette.clone(),
            cursor_style: self.cursor_style,
            cursor_blink: self.cursor_style_blink,
            cursor_color: self.cursor_color,
            cursor_text: self.cursor_text,
            selection_background: self.selection_background,
            selection_foreground: self.selection_foreground,
            search_background: self.search_background,
            search_foreground: self.search_foreground,
            search_selected_background: self.search_selected_background,
            search_selected_foreground: self.search_selected_foreground,
            option_as_alt: self.macos_option_as_alt,
            scrollback_limit: self.scrollback_limit,
            shell_features: self.shell_integration_features,
        }
    }
}
