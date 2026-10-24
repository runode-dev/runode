//! runode：面向 AI 编程 agent 的桌面工作台。终端用 libghostty-vt 仿真，窗口和绘制用 GPUI。
//! 带子命令启动时是命令行（`runode list` 等），不开窗口，见 `runode_cli`；`runode --host` 是单独
//! 一个进程跑的终端宿主，`runode --host --take-over` 是升级时接手旧宿主会话的新宿主，见
//! `host_process`；`runode --screen` 是远程桌面的采集进程，见 `screen_process`；`runode --speech` 是
//! 语音输入的识别子进程，见 `dictation::worker`。
//!
//! Windows 上这个程序是界面程序，打开时不带控制台窗口；在终端里跑命令行用同一个包里的控制台程序
//! `rn`。

#![cfg_attr(windows, windows_subsystem = "windows")]

mod about;
mod assets;
mod config;
mod dictation;
mod file_tree;
mod fs_probe;
mod git_panel;
mod github_actions;
mod host_client;
mod host_process;
mod i18n;
mod kanban;
mod keybinds;
mod menus;
mod prespawn;
mod preview_panel;
mod process_env;
mod remote_access;
#[cfg(any(target_os = "macos", windows))]
mod screen_process;
mod settings;
mod startup;
mod subprocess;
mod terminal_view;
mod update;
mod window;

// 界面文字的翻译，见 `i18n`；某种语言缺了某个键时取英文。
rust_i18n::i18n!("locales", fallback = "en");

// 共用的界面部件，各模块照旧写 `crate::ui::…`。
use runode_ui as ui;

use gpui::App;
use gpui_platform::application;

use crate::window::{open_window, open_window_with};

fn main() {
    startup::begin();
    process_env::fix();
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    // shell 的补全脚本问候选时先于一切参数：补第一个词时参数是空的，不能当成要开窗口；
    // 敲到 `runode --host` 按 Tab 也不能真的拉起宿主。
    if let Some(index) = runode_cli::completion_index() {
        let args: Vec<String> = args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect();
        let env = runode_cli::Env::from_process(env!("RUNODE_BUILD"));
        std::process::exit(runode_cli::complete(&args, index, &env, &mut std::io::stdout().lock()));
    }
    if args == ["--host"] {
        std::process::exit(host_process::run());
    }
    #[cfg(unix)]
    if args == ["--host", "--take-over"] {
        std::process::exit(host_process::take_over());
    }
    if args == ["--speech"] {
        std::process::exit(dictation::worker::run());
    }
    #[cfg(any(target_os = "macos", windows))]
    if args == ["--screen"] {
        std::process::exit(screen_process::run());
    }
    if runode_cli::wants_cli(&args) {
        // 界面程序没有控制台：接上终端的，输出才看得见（shell 不等它跑完，正经用 `rn`）。
        #[cfg(windows)]
        // SAFETY: 只是接上父进程的控制台，没有指针参数。
        unsafe {
            windows_sys::Win32::System::Console::AttachConsole(
                windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
            )
        };
        let args: Vec<String> = args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect();
        let env = runode_cli::Env::from_process(env!("RUNODE_BUILD"));
        let code = runode_cli::run(&args, &env, &mut std::io::stdout().lock(), &mut std::io::stderr().lock());
        std::process::exit(code);
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                // 启动计时点默认不打，要看时设 `RUST_LOG=runode::startup=info`，见 `startup`。
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(format!("info,{}=off", startup::TARGET))),
        )
        .init();
    startup::mark_main();
    // 每个终端占两个描述符，从 Finder 启动时软上限只有 256。
    #[cfg(unix)]
    if let Err(err) = runode_terminal::pty::raise_fd_limit() {
        tracing::warn!("failed to raise the open file limit: {err}");
    }
    // 在后台线程里读配置、定宿主怎么跑、连上它，再提前拉起第一个 shell（启动要几十毫秒），和 GPUI
    // 初始化同时进行；主线程第一次用到宿主时等它连好。
    host_client::start(prespawn::start());

    application().with_assets(assets::Assets).run(|cx: &mut App| {
        startup::mark("app_run");
        assets::install_fonts(cx);
        startup::mark("fonts_install");
        config::install(cx);
        startup::mark("config_install");
        update::install(cx);
        menus::install(cx);
        startup::mark("menus_install");
        window::install(cx);
        startup::mark("workspace_install");
        window::serve_requests(cx);
        cx.on_window_closed(|cx, _| {
            if window::terminal_windows(cx) == 0 {
                cx.quit();
            }
        })
        .detach();

        // 上次退出时开着的窗口原样恢复，没有时开一个新窗口。提前拉起的 shell 在家目录里，
        // 交给第一个窗口里从家目录开始的终端；没用上就丢掉，丢掉时会结束它。
        let mut shell = prespawn::take();
        startup::mark("prespawn_take");
        let saved = window::saved_window_options(cx);
        if saved.is_empty() {
            open_window(cx, shell.take());
        }
        for (saved, options) in saved {
            open_window_with(cx, options, Some(saved), shell.take());
        }
        startup::mark("open_window");
        // 先结束没用上的 shell，再看宿主里有没有没在窗口里显示的会话，免得把它也列进去。
        drop(shell);
        window::watch_background(cx);
        window::watch_devices(cx);
        window::watch_todos(cx);
        window::watch_status(cx);
        cx.activate(true);
        host_client::show_notice(cx);
        // 窗口先出来；未打包运行时才需要的图标解码放到最后。
        about::install_icon();
    });
}
