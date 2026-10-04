//! runode：面向 AI 编程 agent 的桌面工作台。终端在进程内用 libghostty-vt 仿真，
//! 窗口和绘制用 GPUI。

mod about;
mod config;
mod keys;
mod menus;
mod pane;
mod pty;
mod search_bar;
mod session;
mod sprites;
mod terminal_view;
mod theme;
mod workspace;

use gpui::{App, AppContext as _, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_platform::application;

use crate::{terminal_view::TerminalView, workspace::Workspace};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    application().run(|cx: &mut App| {
        config::install(cx);
        menus::install(cx);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        open_window(cx);
        cx.activate(true);
        // 窗口先出来；未打包运行时才需要的图标解码放到最后。
        about::install_icon();
    });
}

fn open_window(cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(960.), px(620.)), cx);
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(320.), px(200.))),
            titlebar: Some(workspace::titlebar_options()),
            // 标题栏的拖动和双击由 `Workspace` 自己处理；否则 AppKit 会抢先处理标题栏区域的双击，
            // 在标签或新建按钮上双击也会缩放窗口。
            app_owns_titlebar_drag: true,
            ..Default::default()
        },
        |window, cx| {
            let first = TerminalView::spawn(None, window, cx)
                .unwrap_or_else(|err| panic!("failed to start terminal session: {err:#}"));
            cx.new(|cx| Workspace::new(first, window, cx))
        },
    );
    if let Err(err) = opened {
        tracing::error!("failed to open window: {err:#}");
    }
}
