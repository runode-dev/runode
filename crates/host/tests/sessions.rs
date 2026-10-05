//! 宿主的黑盒测试：经 `Client` 开会话、连上、发输入和控制消息，看输出流和事件。

use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

use runode_host::{ClientMsg, Host, HostEvent, HostMsg, SessionId, Sink, SpawnOptions};
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
    client.send(ClientMsg::SetTheme { settings: dark.clone() });
    let mut themes = Vec::new();
    client.input(id, b"x\r".to_vec());
    wait_for(&rx, |event, output| {
        if let HostEvent::Msg(m) = event
            && let HostMsg::ThemeApplied { settings, .. } = &**m
        {
            themes.push(settings.clone());
        }
        contains(output, b"x")
    });
    // 默认主题和会话开出来时的一样，只有第二次算换了；标记里带着宿主这时套的那份设置。
    assert_eq!(themes, [dark]);
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

/// 结束会话时最后发一条 `Exited`，之后不再有事件，发给它的消息都忽略。
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

/// shell 的环境里有自己的会话标识和宿主设的变量。
#[test]
fn shells_know_their_session() {
    let host = Host::new();
    host.set_env("RUNODE_TEST", "first");
    host.set_env("RUNODE_TEST", "second");
    let client = host.connect_in_process();
    let id = client.spawn(options("/bin/sh")).unwrap();
    let (sink, rx) = channel_sink();
    client.attach(id, sink).unwrap();
    client.input(id, b"env; exit\r".to_vec());
    let output = wait_for(&rx, |event, _| matches!(event, HostEvent::Msg(m) if matches!(**m, HostMsg::Exited { .. })));
    let output = String::from_utf8_lossy(&output);
    assert!(output.contains(&format!("RUNODE_SESSION={id}")), "{output}");
    assert!(output.contains("RUNODE_TEST=second") && !output.contains("RUNODE_TEST=first"), "{output}");
}

/// 写一个可执行的脚本当 shell 用，返回它的路径。登录 shell 会多带一个 `-l` 参数，脚本不看参数。
fn script(name: &str, body: &str) -> String {
    use std::{io::Write as _, os::unix::fs::PermissionsExt as _};

    let path = std::env::temp_dir().join(format!("runode-host-test-{name}-{}.sh", std::process::id()));
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(format!("#!/bin/sh\n{body}\n").as_bytes()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_string_lossy().into_owned()
}

/// 一个会话，在里面不读启动配置的交互式 zsh 里跑 `yes <marker>` 不停输出：开着作业控制，`yes`
/// 有自己的进程组，标签名能认出它。返回会话、事件和 `marker`。
fn flooding_session(client: &runode_host::Client, name: &str) -> (SessionId, mpsc::Receiver<HostEvent>, String) {
    let id = client.spawn(options(&script(name, "exec /bin/zsh -f -i"))).unwrap();
    let (sink, rx) = channel_sink();
    client.attach(id, sink).unwrap();
    // 等 zsh 出提示符。
    std::thread::sleep(Duration::from_millis(500));
    while rx.try_recv().is_ok() {}
    let marker = format!("runode-{name}-{}", std::process::id());
    client.input(id, format!("yes {marker}\r").into_bytes());
    (id, rx, marker)
}

/// 有没有命令行里带着 `marker` 的进程在跑。
fn running(marker: &str) -> bool {
    std::process::Command::new("pgrep").args(["-f", marker]).output().is_ok_and(|out| out.status.success())
}

/// 输出一直不断时收件箱总有消息，前台进程照样按时重读：`yes` 刷屏期间，标签名一秒左右就变成
/// `yes`，不用等输出停下来。
#[test]
fn the_foreground_is_read_while_output_floods() {
    let client = Host::new().connect_in_process();
    let (id, rx, _) = flooding_session(&client, "flood");
    let started = Instant::now();
    wait_for(&rx, |event, _| {
        matches!(event, HostEvent::Msg(m)
            if matches!(&**m, HostMsg::Meta { meta, .. } if meta.fallback_title.as_deref() == Some("yes")))
    });
    let elapsed = started.elapsed();
    // 这时它还在刷屏。
    wait_for(&rx, |_, output| output.len() > 16 * 1024);
    client.send(ClientMsg::Kill { id });
    assert!(elapsed < Duration::from_millis(1500), "the foreground was read after {elapsed:?}");
}

/// 刷屏到一半结束会话：前端收到一次 `Exited`，之后 channel 断开（会话线程收干净了），`yes` 也被
/// 结束。
#[test]
fn killing_a_flooding_session_cleans_up() {
    let client = Host::new().connect_in_process();
    let (id, rx, marker) = flooding_session(&client, "kill");
    // `yes` 比宿主的 VT 处理得快得多（调试构建尤其慢），这时读线程那边已经积压了一大堆输出。
    wait_for(&rx, |_, output| output.len() > 32 * 1024);
    assert!(running(&marker));
    client.send(ClientMsg::Kill { id });
    let deadline = Instant::now() + WAIT;
    let mut exited = 0;
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(HostEvent::Msg(m)) if matches!(*m, HostMsg::Exited { .. }) => exited += 1,
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("the session thread did not end"),
        }
    }
    assert_eq!(exited, 1);
    while running(&marker) {
        assert!(Instant::now() < deadline, "the flooding program outlived its session");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// shell 退出以后才连上：退出前的输出和退出照样补发，会话留到前端结束它，结束时不再重复报告
/// 退出。
#[test]
fn attaching_after_the_shell_exited() {
    let client = Host::new().connect_in_process();
    let id = client.spawn(options("/bin/echo")).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let (sink, rx) = channel_sink();
    let attached = client.attach(id, sink).unwrap();
    assert!(attached.started);
    let output = wait_for(&rx, |event, _| matches!(event, HostEvent::Msg(m) if matches!(**m, HostMsg::Exited { .. })));
    assert!(contains(&output, b"-l"), "{output:?}");
    client.send(ClientMsg::Kill { id });
    let deadline = Instant::now() + WAIT;
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(HostEvent::Msg(m)) => assert!(!matches!(*m, HostMsg::Exited { .. }), "exit reported twice"),
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("the session thread did not end"),
        }
    }
}

/// 会话线程处理一件事时 panic（这里是前端的 `Sink` 收到某段输出时 panic）：前端收到一条
/// `Error` 和 `Exited`，视图不会一直停在最后一屏；会话线程随之结束。
#[test]
fn a_panicking_session_tells_its_front_end() {
    let client = Host::new().connect_in_process();
    let id = client.spawn(options("/bin/cat")).unwrap();
    let (tx, rx) = mpsc::channel();
    let mut panicked = false;
    let sink: Sink = Box::new(move |event| {
        if !panicked && matches!(&event, HostEvent::Output(data) if contains(data, b"boom")) {
            panicked = true;
            panic!("the sink blew up");
        }
        tx.send(event).is_ok()
    });
    client.attach(id, sink).unwrap();
    client.input(id, b"boom\r".to_vec());
    let deadline = Instant::now() + WAIT;
    let (mut errors, mut exited) = (0, 0);
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(HostEvent::Msg(m)) => match *m {
                HostMsg::Error { id: Some(errored), ref message, .. } if errored == id => {
                    assert!(message.contains("the sink blew up"), "{message}");
                    errors += 1;
                }
                HostMsg::Exited { .. } => exited += 1,
                _ => {}
            },
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("the session thread did not end"),
        }
    }
    assert_eq!((errors, exited), (1, 1));
    client.send(ClientMsg::Kill { id });
}
