//! Windows 的真实 ConPTY 链路：程序开启鼠标上报后，上下滚轮都必须能到达它的标准输入。
#![cfg(windows)]

use std::{
    io::{Read, Write},
    sync::mpsc,
    time::{Duration, Instant},
};

use runode_terminal::{
    host_session::HostSession,
    pty::{Pty, PtyEvent},
    session::{Request, Session},
};
use runode_types::{
    grid::{GridPoint, GridSize},
    input::Mods,
    settings::TermSettings,
    shell::IntegrationMode,
};

const WHEEL: &[u8] = b"\x1b[<64;6;6M\x1b[<64;6;6M\x1b[<65;6;6M\x1b[<65;6;6M";

// 由下面的测试在伪终端里启动；不依赖本机装了 Node、Codex 或 pi。
#[test]
#[ignore = "helper process for wheel_events_reach_a_console_program"]
fn conpty_mouse_child() {
    use windows_sys::Win32::System::Console::{
        ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, ENABLE_VIRTUAL_TERMINAL_INPUT,
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
        SetConsoleMode,
    };
    assert_eq!(std::env::var("RUNODE_SCROLL_CHILD").as_deref(), Ok("1"));
    // SAFETY: 此测试在独立的 ConPTY 子进程里，只设置它自己的有效控制台句柄。
    unsafe {
        let input = GetStdHandle(STD_INPUT_HANDLE);
        let output = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode = 0;
        assert_ne!(GetConsoleMode(input, &mut mode), 0);
        assert_ne!(
            SetConsoleMode(
                input,
                (mode & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT))
                    | ENABLE_VIRTUAL_TERMINAL_INPUT,
            ),
            0
        );
        assert_ne!(GetConsoleMode(output, &mut mode), 0);
        assert_ne!(SetConsoleMode(output, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING), 0);
    }
    std::io::stdout().write_all(b"\x1b[?1000h\x1b[?1006hSCROLL_READY\r\n").unwrap();
    std::io::stdout().flush().unwrap();
    let mut bytes = vec![0; WHEEL.len()];
    std::io::stdin().read_exact(&mut bytes).unwrap();
    assert_eq!(bytes, WHEEL);
    std::io::stdout().write_all(b"\x1b[?1000l\x1b[?1006lSCROLL_RECEIVED\r\n").unwrap();
    std::io::stdout().flush().unwrap();
}

#[test]
fn wheel_events_reach_a_console_program() {
    let size = GridSize { cols: 120, rows: 24, cell_width_px: 8, cell_height_px: 16 };
    let settings = TermSettings::default();
    let (tx, rx) = mpsc::channel();
    let mut pty = Pty::open(size, Box::new(move |event| tx.send(event).is_ok())).unwrap();
    pty.set_env("RUNODE_SCROLL_CHILD", "1");
    let writer = pty.writer.clone();
    pty.start(Some("cmd.exe"), None, IntegrationMode::Off).unwrap();
    let mut host = HostSession::new(size, pty, None, &settings).unwrap();
    let sender = writer.clone();
    let mut session = Session::new(
        size,
        &settings,
        Box::new(move |request| {
            if let Request::Input(bytes) = request {
                sender.write(&bytes);
            }
        }),
    )
    .unwrap();
    // 不把标记的字面量放到命令行里，避免 cmd 的回显被误当成子进程已就绪。
    writer.write(
        format!(
            "\"{}\" --ignored --exact conpty_mouse_child --nocapture\r",
            std::env::current_exe().unwrap().display()
        )
        .as_bytes(),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut output = Vec::new();
    let mut sent = false;
    while Instant::now() < deadline {
        let Ok(PtyEvent::Output(bytes)) = rx.recv_timeout(Duration::from_millis(100)) else { continue };
        host.feed(&bytes);
        session.feed(&bytes);
        output.extend_from_slice(&bytes);
        if !sent && output.windows(12).any(|w| w == b"SCROLL_READY") {
            assert!(session.mouse_tracking(), "ConPTY dropped the program's mouse mode request");
            let at = GridPoint { x: 5.5, y: 5.5 };
            session.scroll(-2, at, Mods::default());
            session.scroll(2, at, Mods::default());
            sent = true;
        }
        if output.windows(15).any(|w| w == b"SCROLL_RECEIVED") {
            assert!(sent);
            assert!(!session.mouse_tracking(), "ConPTY dropped the mouse mode reset");
            return;
        }
    }
    panic!("ConPTY did not deliver both wheel directions: {}", String::from_utf8_lossy(&output));
}
