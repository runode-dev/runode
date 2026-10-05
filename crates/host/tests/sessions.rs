//! 宿主的黑盒测试：经 `Client` 开会话、连上、发输入和控制消息，看输出流和事件。

use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

use runode_host::{ClientMsg, Host, HostEvent, HostMsg, Sink, SpawnOptions};
use runode_shared_types::{grid::GridSize, settings::TermSettings, shell::IntegrationMode};

const SIZE: GridSize = GridSize { cols: 20, rows: 4, cell_width_px: 8, cell_height_px: 16 };
const WAIT: Duration = Duration::from_secs(10);

fn options(shell: &str) -> SpawnOptions {
    SpawnOptions {
        size: SIZE,
        cwd: None,
        integration: IntegrationMode::Off,
        start: true,
        shell: Some(shell.into()),
        settings: None,
    }
}

fn channel_sink() -> (Sink, mpsc::Receiver<HostEvent>) {
    let (tx, rx) = mpsc::channel();
    (Box::new(move |event| tx.send(event).is_ok()), rx)
}

/// 等到 `found` 认出某件事，返回在那之前（含）收到的全部输出。
fn wait_for(rx: &mpsc::Receiver<HostEvent>, mut found: impl FnMut(&HostEvent, &[u8]) -> bool) -> Vec<u8> {
    let deadline = Instant::now() + WAIT;
    let mut output = Vec::new();
    loop {
        let event = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("timed out");
        if let HostEvent::Output(data) = &event {
            output.extend_from_slice(data);
        }
        if found(&event, &output) {
            return output;
        }
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// `cat` 当 shell：输入经宿主写给它，行规程的回显经宿主转回来。
#[test]
fn input_reaches_the_program_and_output_comes_back() {
    let client = Host::new().connect_in_process();
    let id = client.spawn(options("/bin/cat")).unwrap();
    let (sink, rx) = channel_sink();
    let attached = client.attach(id, sink).unwrap();
    assert_eq!((attached.size, attached.started), (SIZE, true));
    client.input(id, b"hello\r".to_vec());
    wait_for(&rx, |_, output| contains(output, b"hello"));
    client.send(ClientMsg::Kill { id });
}

/// 改尺寸和换主题都在输出流里标出位置；没变的不标。
#[test]
fn resize_and_theme_changes_are_marked_in_the_stream() {
    let host = Host::new();
    let client = host.connect_in_process();
    let id = client.spawn(options("/bin/cat")).unwrap();
    let (sink, rx) = channel_sink();
    client.attach(id, sink).unwrap();
    let size = GridSize { cols: 30, ..SIZE };
    client.send(ClientMsg::Resize { id, size: SIZE });
    client.send(ClientMsg::Resize { id, size });
    wait_for(
        &rx,
        |event, _| matches!(event, HostEvent::Msg(m) if matches!(**m, HostMsg::Resized { size: s, .. } if s == size)),
    );
    client.send(ClientMsg::SetTheme { settings: TermSettings::default() });
    let dark = TermSettings { cursor_blink: Some(false), ..TermSettings::default() };
    client.send(ClientMsg::SetTheme { settings: dark });
    let mut themes = 0;
    client.input(id, b"x\r".to_vec());
    wait_for(&rx, |event, output| {
        themes += usize::from(matches!(event, HostEvent::Msg(m) if matches!(**m, HostMsg::ThemeApplied { .. })));
        contains(output, b"x")
    });
    // 默认主题和会话开出来时的一样，只有第二次算换了。
    assert_eq!(themes, 1);
    client.send(ClientMsg::Kill { id });
}

/// 连上之前的输出攒着，连上时先补发，退出也一样。
#[test]
fn output_before_attaching_is_replayed() {
    let client = Host::new().connect_in_process();
    // `echo` 当 shell：打印启动参数（登录 shell 的 `-l`）后就退出。
    let id = client.spawn(options("/bin/echo")).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let (sink, rx) = channel_sink();
    client.attach(id, sink).unwrap();
    let output = wait_for(&rx, |event, _| matches!(event, HostEvent::Msg(m) if matches!(**m, HostMsg::Exited { .. })));
    assert!(contains(&output, b"-l"), "{output:?}");
    // 只能连一次。
    let (sink, _rx) = channel_sink();
    assert!(client.attach(id, sink).is_err());
    client.send(ClientMsg::Kill { id });
}

/// 清屏的字节当成一段输出发回来；前台是 shell 时还给它发一个 FF，`cat` 把它回显成 `^L`。
#[test]
fn clear_screen_comes_back_as_output() {
    let client = Host::new().connect_in_process();
    let id = client.spawn(options("/bin/cat")).unwrap();
    let (sink, rx) = channel_sink();
    client.attach(id, sink).unwrap();
    client.send(ClientMsg::ClearScreen { id });
    let output = wait_for(&rx, |_, output| contains(output, b"^L"));
    assert!(output.starts_with(b"\x1b[H\x1b[2J\x1b[3J"), "{output:?}");
    client.send(ClientMsg::Kill { id });
}

/// 结束会话后不再有事件，之后发给它的消息都忽略。
#[test]
fn killed_sessions_go_quiet() {
    let client = Host::new().connect_in_process();
    let id = client.spawn(options("/bin/cat")).unwrap();
    let (sink, rx) = channel_sink();
    client.attach(id, sink).unwrap();
    client.send(ClientMsg::Kill { id });
    client.input(id, b"after\r".to_vec());
    // 会话线程退出后 sink 被丢掉，channel 断开。
    let deadline = Instant::now() + WAIT;
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(HostEvent::Output(data)) => assert!(!contains(&data, b"after")),
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("session did not end"),
        }
    }
}

/// 还没启动的会话：标题和目录是起始目录的；`start` 后 shell 才跑起来。
#[test]
fn unstarted_sessions_start_on_request() {
    let client = Host::new().connect_in_process();
    let dir = std::env::temp_dir();
    let id = client.spawn(SpawnOptions { start: false, cwd: Some(dir.clone()), ..options("/bin/cat") }).unwrap();
    let (sink, rx) = channel_sink();
    let attached = client.attach(id, sink).unwrap();
    assert!(!attached.started);
    assert_eq!(attached.meta.cwd, Some(dir));
    client.start(id, IntegrationMode::Off);
    client.input(id, b"go\r".to_vec());
    wait_for(&rx, |_, output| contains(output, b"go"));
    client.send(ClientMsg::Kill { id });
}
