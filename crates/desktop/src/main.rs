//! runode：面向 AI 编程 agent 的桌面工作台。终端在进程内用 libghostty-vt 仿真，
//! 窗口和绘制用 GPUI。

mod about;
mod agent_alert;
mod assets;
mod config;
mod file_icons;
mod i18n;
mod keybinds;
mod keys;
mod menus;
mod persist;
mod prespawn;
mod scrollbar;
mod search_bar;
mod sprites;
mod terminal_view;
mod text_area;
mod tooltip;
mod window;
mod workspace;

// 界面文字的翻译，见 `i18n`；某种语言缺了某个键时取英文。
rust_i18n::i18n!("locales", fallback = "en");

use gpui::App;
use gpui_platform::application;

use crate::window::{open_window, open_window_with};

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
            open_window(cx, shell.take());
        }
        for (saved, options) in saved {
            open_window_with(cx, options, Some(saved), shell.take());
        }
        cx.activate(true);
        // 窗口先出来；未打包运行时才需要的图标解码放到最后。
        about::install_icon();
    });
}
