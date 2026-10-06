//! runode：面向 AI 编程 agent 的桌面工作台。终端用 libghostty-vt 仿真，窗口和绘制用 GPUI。
//! 带子命令启动时是命令行（`runode list` 等），不开窗口，见 `runode_cli`；`runode --host` 是单独
//! 一个进程跑的终端宿主，见 `host_process`。

mod about;
mod agent_alert;
mod assets;
mod config;
mod file_icons;
mod host_process;
mod i18n;
mod keybinds;
mod keys;
mod menus;
mod persist;
mod prespawn;
mod scrollbar;
mod search_bar;
mod session_host;
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
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if args == ["--host"] {
        std::process::exit(host_process::run());
    }
    if runode_cli::wants_cli(&args) {
        let args: Vec<String> = args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect();
        let env = runode_cli::Env::from_process(env!("RUNODE_BUILD"));
        let code = runode_cli::run(&args, &env, &mut std::io::stdout().lock(), &mut std::io::stderr().lock());
        std::process::exit(code);
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    // 每个终端占两个描述符，从 Finder 启动时软上限只有 256。
    if let Err(err) = runode_terminal::pty::raise_fd_limit() {
        tracing::warn!("failed to raise the open file limit: {err}");
    }
    // 在后台线程里读配置、定宿主怎么跑、连上它，再提前拉起第一个 shell（启动要几十毫秒），和 GPUI
    // 初始化同时进行；主线程第一次用到宿主时等它连好。
    session_host::start(prespawn::start());

    application().with_assets(assets::Assets).run(|cx: &mut App| {
        config::install(cx);
        menus::install(cx);
        workspace::install(cx);
        workspace::serve_requests(cx);
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
        // 先结束没用上的 shell，再看宿主里有没有没在窗口里显示的会话，免得把它也列进去。
        drop(shell);
        workspace::watch_background(cx);
        cx.activate(true);
        session_host::show_notice(cx);
        // 窗口先出来；未打包运行时才需要的图标解码放到最后。
        about::install_icon();
    });
}
