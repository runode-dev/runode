//! runode：面向 AI 编程 agent 的桌面工作台。终端用 libghostty-vt 仿真，窗口和绘制用 GPUI。
//! 带子命令启动时是命令行（`runode list` 等），不开窗口，见 `runode_cli`；`runode --host` 是单独
//! 一个进程跑的终端宿主，`runode --host --take-over` 是升级时接手旧宿主会话的新宿主，见
//! `host_process`。

mod about;
mod assets;
mod config;
mod file_tree;
mod git_panel;
mod github_actions;
mod host_client;
mod host_process;
mod i18n;
mod keybinds;
mod menus;
mod prespawn;
mod preview_panel;
mod remote_access;
mod settings;
mod startup;
mod terminal_view;
mod todo_board;
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
    use_login_shell();
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if args == ["--host"] {
        std::process::exit(host_process::run());
    }
    if args == ["--host", "--take-over"] {
        std::process::exit(host_process::take_over());
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
                // 启动计时点默认不打，要看时设 `RUST_LOG=runode::startup=info`，见 `startup`。
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(format!("info,{}=off", startup::TARGET))),
        )
        .init();
    startup::mark_main();
    // 每个终端占两个描述符，从 Finder 启动时软上限只有 256。
    if let Err(err) = runode_terminal::pty::raise_fd_limit() {
        tracing::warn!("failed to raise the open file limit: {err}");
    }
    // 在后台线程里读配置、定宿主怎么跑、连上它，再提前拉起第一个 shell（启动要几十毫秒），和 GPUI
    // 初始化同时进行；主线程第一次用到宿主时等它连好。
    host_client::start(prespawn::start());

    application().with_assets(assets::Assets).run(|cx: &mut App| {
        startup::mark("app_run");
        config::install(cx);
        startup::mark("config_install");
        update::install(cx);
        menus::install(cx);
        menus::refresh_agent_integration(cx);
        startup::mark("menus_install");
        window::install(cx);
        startup::mark("workspace_install");
        window::serve_requests(cx);
        // 终端窗口都关了就退出。
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

/// 由 launchd 直接拉起（从 Dock 或 Finder 打开、登录项、登录时自启的宿主）时继承的 `SHELL` 是这次登录时
/// 定下的，改了登录 shell（chsh）要重新登录才变，终端就照旧开原来的 shell。这时照 Terminal.app 改用账户
/// 记录里的登录 shell，宿主进程和各个终端都继承它。别的进程拉起的（从终端里启动的，app 拉起的宿主）
/// 沿用父进程给的 `SHELL`。
fn use_login_shell() {
    // SAFETY: getppid 总是成功，没有副作用。
    if unsafe { libc::getppid() } != 1 {
        return;
    }
    if let Some(shell) = login_shell() {
        // SAFETY: 在 `main` 最开头调用，还没有别的线程读写环境变量。
        unsafe { std::env::set_var("SHELL", shell) };
    }
}

/// 账户记录（passwd）里的登录 shell，没有时为 `None`。
fn login_shell() -> Option<std::ffi::OsString> {
    use std::os::unix::ffi::OsStrExt as _;
    // SAFETY: `passwd` 只有整数和指针，全零是合法的值。
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0; 4096];
    let mut found = std::ptr::null_mut();
    // SAFETY: 只往传进去的 `entry` 和 `buf` 里写，`entry` 里的字符串都指向 `buf`。
    let code = unsafe { libc::getpwuid_r(libc::getuid(), &raw mut entry, buf.as_mut_ptr(), buf.len(), &raw mut found) };
    if code != 0 || found.is_null() || entry.pw_shell.is_null() {
        return None;
    }
    // SAFETY: 成功时 `pw_shell` 是 `buf` 里以 NUL 结尾的字符串，`buf` 还活着。
    let shell = unsafe { std::ffi::CStr::from_ptr(entry.pw_shell) }.to_bytes();
    (!shell.is_empty()).then(|| std::ffi::OsStr::from_bytes(shell).to_owned())
}
