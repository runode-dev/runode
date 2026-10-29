//! 不依赖本机安装 Codex：缺少随包运行库时不能回退到旧版 conhost；预览栏引发的大幅尺寸变化
//! 之后，控制台必须仍能处理输入并报告新尺寸。
#![cfg(windows)]

use std::{
    io::{Read, Write},
    os::windows::process::CommandExt as _,
    sync::mpsc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use runode_terminal::{
    host_session::HostSession,
    pty::{Pty, PtyEvent},
};
use runode_types::{grid::GridSize, settings::TermSettings, shell::IntegrationMode};

const SIZE: GridSize = GridSize { cols: 132, rows: 47, cell_width_px: 8, cell_height_px: 16 };

#[test]
#[ignore = "helper process for missing_runtime_does_not_fall_back_to_system_conpty"]
fn missing_runtime_child() {
    assert_eq!(std::env::var("RUNODE_MISSING_CONPTY_CHILD").as_deref(), Ok("1"));
    match Pty::open(SIZE, Box::new(|_| true)) {
        Ok(_) => panic!("a terminal must not start with the system ConPTY fallback"),
        Err(err) => assert!(format!("{err:#}").contains("bundled ConPTY runtime"), "{err:#}"),
    }
}

#[test]
fn missing_runtime_does_not_fall_back_to_system_conpty() {
    let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let directory = std::env::temp_dir().join(format!("runode-conpty-runtime-{}-{unique}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let executable = directory.join("runtime-test.exe");
    std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    let run = || {
        std::process::Command::new(&executable)
            .args(["--ignored", "--exact", "missing_runtime_child", "--nocapture"])
            .env("RUNODE_MISSING_CONPTY_CHILD", "1")
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
            .output()
            .unwrap()
    };
    let without_console = run();
    // OpenConsole 在、DLL 不在时也必须报错，不能让 portable-pty 回退到系统后端。
    let console = directory.join("OpenConsole.exe");
    std::fs::write(&console, []).unwrap();
    let without_library = run();
    std::fs::remove_file(console).unwrap();
    std::fs::remove_file(&executable).unwrap();
    std::fs::remove_dir(&directory).unwrap();
    for result in [without_console, without_library] {
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
#[ignore = "helper process for resizing_a_live_console_keeps_input_working"]
fn resize_console_child() {
    use windows_sys::Win32::System::Console::{
        CONSOLE_SCREEN_BUFFER_INFO, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, GetConsoleMode,
        GetConsoleScreenBufferInfo, GetStdHandle, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode,
    };
    assert_eq!(std::env::var("RUNODE_RESIZE_CONPTY_CHILD").as_deref(), Ok("1"));
    // SAFETY: 独立的测试子进程只设置自己的控制台输入句柄。
    unsafe {
        let input = GetStdHandle(STD_INPUT_HANDLE);
        let mut mode = 0;
        assert_ne!(GetConsoleMode(input, &mut mode), 0);
        assert_ne!(SetConsoleMode(input, mode & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT)), 0);
    }
    let mut output = std::io::stdout().lock();
    writeln!(output, "RESIZE_READY").unwrap();
    output.flush().unwrap();
    let mut byte = [0];
    while std::io::stdin().read_exact(&mut byte).is_ok() {
        if byte == *b"q" {
            return;
        }
        // SAFETY: 输出缓冲属于当前子进程，info 是足够大的本地输出结构。
        let info = unsafe {
            let mut info = std::mem::zeroed::<CONSOLE_SCREEN_BUFFER_INFO>();
            assert_ne!(GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &mut info), 0);
            info
        };
        writeln!(output, "RESIZE_SIZE_{}_{}", info.dwSize.X, info.srWindow.Bottom - info.srWindow.Top + 1).unwrap();
        output.flush().unwrap();
    }
}

#[test]
fn resizing_a_live_console_keeps_input_working() {
    let (tx, rx) = mpsc::channel();
    let mut pty = Pty::open(SIZE, Box::new(move |event| tx.send(event).is_ok())).unwrap();
    pty.set_env("RUNODE_RESIZE_CONPTY_CHILD", "1");
    let writer = pty.writer.clone();
    pty.start(Some("cmd.exe"), None, IntegrationMode::Off).unwrap();
    let mut host = HostSession::new(SIZE, pty, None, &TermSettings::default()).unwrap();
    writer.write(
        format!(
            "\"{}\" --ignored --exact resize_console_child --nocapture\r",
            std::env::current_exe().unwrap().display()
        )
        .as_bytes(),
    );
    let wait_for = |host: &mut HostSession, text: &str| {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut output = Vec::new();
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(PtyEvent::Output(bytes)) => {
                    host.feed(&bytes);
                    output.extend_from_slice(&bytes);
                    if output.windows(text.len()).any(|bytes| bytes == text.as_bytes()) {
                        return;
                    }
                }
                Ok(PtyEvent::Exited) => panic!("the console exited while waiting for {text}"),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(err) => panic!("the console disconnected: {err}"),
            }
        }
        panic!("missing {text}: {}", String::from_utf8_lossy(&output));
    };
    wait_for(&mut host, "RESIZE_READY");
    for (cols, rows) in [(316, 80), (72, 47), (132, 47), (291, 80), (132, 47)] {
        assert!(host.resize(GridSize { cols, rows, ..SIZE }));
        writer.write(b"s");
        wait_for(&mut host, &format!("RESIZE_SIZE_{cols}_{rows}"));
    }
    writer.write(b"q");
}
