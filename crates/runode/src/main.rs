//! runode: a desktop workspace for AI coding agents, rendering terminals with
//! libghostty-vt in-process and GPUI for the window.

mod keys;
mod pty;
mod session;
mod terminal_view;

use gpui::{
    App, AppContext as _, Bounds, Focusable as _, KeyBinding, WindowBounds, WindowOptions,
    actions, px, size,
};
use gpui_platform::application;

use crate::terminal_view::{Copy, Paste, TerminalView};

actions!(runode, [Quit, NewWindow, CloseWindow]);

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    application().run(|cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-n", NewWindow, None),
            KeyBinding::new("cmd-w", CloseWindow, None),
            KeyBinding::new("cmd-v", Paste, Some("Terminal")),
            KeyBinding::new("cmd-c", Copy, Some("Terminal")),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_action(|_: &NewWindow, cx| open_window(cx));
        cx.on_action(|_: &CloseWindow, cx| {
            if let Some(window) = cx.active_window() {
                window.update(cx, |_, window, _| window.remove_window()).ok();
            }
        });
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
