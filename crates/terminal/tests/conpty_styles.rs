//! Windows 的真实 ConPTY 链路必须保留程序输出的颜色、底色和文字样式。
#![cfg(windows)]

use std::{
    io::{Read, Write},
    sync::mpsc,
    time::{Duration, Instant},
};

use runode_terminal::{
    host_session::HostSession,
    pty::{Pty, PtyEvent},
    session::Session,
};
use runode_types::{color::Rgb, grid::GridSize, settings::TermSettings, shell::IntegrationMode};

#[test]
#[ignore = "helper process for application_styles_survive_conpty"]
fn styles_child() {
    use windows_sys::Win32::System::Console::{
        ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle,
        STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode,
    };
    assert_eq!(std::env::var("RUNODE_STYLES_CHILD").as_deref(), Ok("1"));
    // SAFETY: 独立子进程只改自己的控制台句柄。
    unsafe {
        let mut mode = 0;
        let input = GetStdHandle(STD_INPUT_HANDLE);
        assert_ne!(GetConsoleMode(input, &mut mode), 0);
        assert_ne!(SetConsoleMode(input, mode & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT)), 0);
        let output = GetStdHandle(STD_OUTPUT_HANDLE);
        assert_ne!(GetConsoleMode(output, &mut mode), 0);
        assert_ne!(SetConsoleMode(output, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING), 0);
    }
    let mut output = std::io::stdout().lock();
    output
        .write_all(b"\x1b[2J\x1b[H\x1b[38;2;12;123;234;48;2;45;56;67mCOLOR\x1b[0m\r\n\x1b[1mBOLD\x1b[0m\r\n\x1b[2mFAINT\x1b[0m\r\n\x1b[3mITALIC\x1b[0m\r\n\x1b[4mUNDERLINE\x1b[0m\r\nSTYLES_READY")
        .unwrap();
    output.flush().unwrap();
    std::io::stdin().read_exact(&mut [0]).unwrap();
}

#[test]
fn application_styles_survive_conpty() {
    let size = GridSize { cols: 100, rows: 24, cell_width_px: 8, cell_height_px: 16 };
    let settings = TermSettings::default();
    let (tx, rx) = mpsc::channel();
    let mut pty = Pty::open(size, Box::new(move |event| tx.send(event).is_ok())).unwrap();
    pty.set_env("RUNODE_STYLES_CHILD", "1");
    let writer = pty.writer.clone();
    pty.start(Some("cmd.exe"), None, IntegrationMode::Off).unwrap();
    let mut host = HostSession::new(size, pty, None, &settings).unwrap();
    let mut session = Session::new(size, &settings, Box::new(|_| {})).unwrap();
    writer.write(
        format!("\"{}\" --ignored --exact styles_child --nocapture\r", std::env::current_exe().unwrap().display())
            .as_bytes(),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut output = Vec::new();
    while Instant::now() < deadline {
        let Ok(PtyEvent::Output(bytes)) = rx.recv_timeout(Duration::from_millis(100)) else { continue };
        host.feed(&bytes);
        session.feed(&bytes);
        output.extend_from_slice(&bytes);
        let frame = session.frame();
        let row = |text: &str| {
            (0..frame.rows)
                .find(|&y| frame.row(y).iter().map(|c| c.text.as_str()).collect::<String>().starts_with(text))
        };
        if row("STYLES_READY").is_none() {
            continue;
        }
        let cell = |text| &frame.row(row(text).unwrap())[0];
        assert_eq!(cell("COLOR").fg, Rgb(12, 123, 234));
        assert_eq!(cell("COLOR").bg, Some(Rgb(45, 56, 67)));
        assert!(cell("BOLD").attrs.bold, "bold lost: {output:?}");
        assert!(cell("FAINT").attrs.faint, "faint lost: {output:?}");
        assert!(cell("ITALIC").attrs.italic, "italic lost: {output:?}");
        assert!(cell("UNDERLINE").attrs.underline, "underline lost: {output:?}");
        writer.write(b"q");
        return;
    }
    panic!("missing styled output: {}", String::from_utf8_lossy(&output));
}

#[test]
#[ignore = "helper process for color query tests"]
fn color_query_child() {
    use windows_sys::Win32::System::Console::{
        ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, ENABLE_VIRTUAL_TERMINAL_INPUT,
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, INPUT_RECORD, KEY_EVENT, ReadConsoleInputW,
        STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode,
    };
    assert_eq!(std::env::var("RUNODE_COLOR_QUERY_CHILD").as_deref(), Ok("1"));
    let vt_input = std::env::var("RUNODE_COLOR_QUERY_VT").as_deref() == Ok("1");
    // SAFETY: 独立子进程只改自己的控制台句柄。
    unsafe {
        let mut mode = 0;
        let input = GetStdHandle(STD_INPUT_HANDLE);
        assert_ne!(GetConsoleMode(input, &mut mode), 0);
        mode &= !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT | ENABLE_VIRTUAL_TERMINAL_INPUT);
        if vt_input {
            mode |= ENABLE_VIRTUAL_TERMINAL_INPUT;
        }
        assert_ne!(SetConsoleMode(input, mode), 0);
        let output = GetStdHandle(STD_OUTPUT_HANDLE);
        assert_ne!(GetConsoleMode(output, &mut mode), 0);
        assert_ne!(SetConsoleMode(output, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING), 0);
    }
    let mut output = std::io::stdout().lock();
    output.write_all(b"\x1b]10;?\x07\x1b]11;?\x1b\\\x1b]4;1;?\x07").unwrap();
    output.flush().unwrap();
    let expected = b"\x1b]10;rgb:abab/cdcd/efef\x07\x1b]11;rgb:1212/3434/5656\x1b\\\x1b]4;1;rgb:1111/2222/3333\x07";
    let mut received = Vec::new();
    if vt_input {
        received.resize(expected.len(), 0);
        std::io::stdin().read_exact(&mut received).unwrap();
    } else {
        while received.len() < expected.len() {
            // SAFETY: 读当前控制台的输入记录，结构体和数量的输出指针均有效。
            unsafe {
                let mut record = std::mem::zeroed::<INPUT_RECORD>();
                let mut count = 0;
                assert_ne!(ReadConsoleInputW(GetStdHandle(STD_INPUT_HANDLE), &mut record, 1, &mut count), 0);
                if count == 1 && record.EventType == KEY_EVENT as u16 && record.Event.KeyEvent.bKeyDown != 0 {
                    let key = record.Event.KeyEvent;
                    assert_eq!(key.wRepeatCount, 1, "repeated reply characters must not be coalesced");
                    received.push(u8::try_from(key.uChar.UnicodeChar).unwrap());
                }
            }
        }
    }
    assert_eq!(received, expected);
    output.write_all(b"COLOR_QUERY_RECEIVED").unwrap();
    output.flush().unwrap();
}

#[test]
fn color_queries_reach_vt_input() {
    color_query_round_trip(true);
}

#[test]
fn color_queries_reach_console_input_records() {
    color_query_round_trip(false);
}

fn color_query_round_trip(vt_input: bool) {
    let size = GridSize { cols: 100, rows: 24, cell_width_px: 8, cell_height_px: 16 };
    let settings = TermSettings {
        background: Rgb(0x12, 0x34, 0x56),
        foreground: Rgb(0xab, 0xcd, 0xef),
        palette: vec![(1, Rgb(0x11, 0x22, 0x33))],
        ..TermSettings::default()
    };
    let (tx, rx) = mpsc::channel();
    let mut pty = Pty::open(size, Box::new(move |event| tx.send(event).is_ok())).unwrap();
    pty.set_env("RUNODE_COLOR_QUERY_CHILD", "1");
    pty.set_env("RUNODE_COLOR_QUERY_VT", if vt_input { "1" } else { "0" });
    let writer = pty.writer.clone();
    pty.start(Some("cmd.exe"), None, IntegrationMode::Off).unwrap();
    let mut host = HostSession::new(size, pty, None, &settings).unwrap();
    writer.write(
        format!("\"{}\" --ignored --exact color_query_child --nocapture\r", std::env::current_exe().unwrap().display())
            .as_bytes(),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut output = Vec::new();
    while Instant::now() < deadline {
        let Ok(PtyEvent::Output(bytes)) = rx.recv_timeout(Duration::from_millis(100)) else { continue };
        host.feed(&bytes);
        output.extend_from_slice(&bytes);
        if output.windows(b"COLOR_QUERY_RECEIVED".len()).any(|w| w == b"COLOR_QUERY_RECEIVED") {
            return;
        }
    }
    panic!("missing color query reply: {}", String::from_utf8_lossy(&output));
}
