//! runode：面向 AI 编程 agent 的桌面工作台。终端在进程内用 libghostty-vt 仿真，
//! 窗口和绘制用 GPUI。

mod about;
mod keys;
mod menus;
mod pty;
mod session;
mod terminal_view;

use gpui::{App, AppContext as _, Bounds, Focusable as _, WindowBounds, WindowOptions, px, size};
use gpui_platform::application;

use crate::terminal_view::TerminalView;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    application().run(|cx: &mut App| {
        about::install_icon();
        menus::install(cx);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        open_window(cx);
        cx.activate(true);
    });
}

fn open_window(cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(960.), px(620.)), cx);
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(320.), px(200.))),
            ..Default::default()
        },
        |window, cx| {
            let view = cx.new(|cx| {
                TerminalView::new(window, cx).unwrap_or_else(|err| {
                    panic!("failed to start terminal session: {err:#}")
                })
            });
            window.focus(&view.focus_handle(cx), cx);
            window.set_window_title("runode");
            view
        },
    );
    if let Err(err) = opened {
        tracing::error!("failed to open window: {err:#}");
    }
}
