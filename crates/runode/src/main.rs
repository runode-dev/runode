//! runode：面向 AI 编程 agent 的桌面工作台。终端在进程内用 libghostty-vt 仿真，
//! 窗口和绘制用 GPUI。

mod about;
mod agent;
mod assets;
mod completion;
mod config;
mod file_icons;
mod git;
mod history;
mod i18n;
mod keybinds;
mod keys;
mod menus;
mod pane;
mod persist;
mod prespawn;
mod prompt_input;
mod pty;
mod search_bar;
mod session;
mod shell_integration;
mod sprites;
mod terminal_view;
mod theme;
mod workspace;

// 界面文字的翻译，见 `i18n`；某种语言缺了某个键时取英文。
rust_i18n::i18n!("locales", fallback = "en");

use gpui::{App, AppContext as _, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_platform::application;

use crate::{persist::SavedWindow, prespawn::Prespawned, workspace::WindowView};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    // shell 启动要几十毫秒，先在后台拉起来，和 GPUI 初始化同时进行。
    prespawn::start();

    application().with_assets(assets::Assets).run(|cx: &mut App| {
        config::install(cx);
        menus::install(cx);
        workspace::install(cx);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        // 上次退出时开着的窗口原样恢复，没有时开一个新窗口。提前拉起的 shell 在家目录里，
        // 交给第一个窗口里从家目录开始的终端；没用上就丢掉，丢掉时会结束它。
        let mut shell = prespawn::take();
        let saved = workspace::saved_window_options(cx);
        if saved.is_empty() {
            open_window(cx, None, shell.take());
        }
        for (saved, options) in saved {
            open_window_with(cx, options, Some(saved), shell.take());
        }
        cx.activate(true);
        // 窗口先出来；未打包运行时才需要的图标解码放到最后。
        about::install_icon();
    });
}

/// 新窗口的默认选项：屏幕居中的默认大小。
fn window_options(cx: &App) -> WindowOptions {
    let bounds = Bounds::centered(None, size(px(960.), px(620.)), cx);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(320.), px(200.))),
        titlebar: Some(workspace::titlebar_options()),
        // 标题栏的拖动和双击由 `WindowView` 自己处理；否则 AppKit 会抢先处理标题栏区域的双击，
        // 在标签或新建按钮上双击也会缩放窗口。
        app_owns_titlebar_drag: true,
        ..Default::default()
    }
}

/// 打开一个新窗口，里面一个终端；`shell` 是启动时提前拉起的 shell，没有时现启动一个。
/// `saved` 是存档里的窗口，有时按它恢复。
fn open_window(cx: &mut App, saved: Option<SavedWindow>, shell: Option<Prespawned>) {
    let options = window_options(cx);
    open_window_with(cx, options, saved, shell);
}

fn open_window_with(cx: &mut App, options: WindowOptions, saved: Option<SavedWindow>, shell: Option<Prespawned>) {
    let opened = cx.open_window(options, |window, cx| {
        cx.new(|cx| match saved {
            Some(saved) => WindowView::restore(saved, shell, window, cx),
            None => WindowView::new(shell, window, cx),
        })
    });
    if let Err(err) = opened {
        tracing::error!("failed to open window: {err:#}");
    }
}
