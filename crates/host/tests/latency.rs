//! 宿主的基准，手动跑：
//!
//! ```sh
//! cargo test --release -p runode-host --test latency -- --ignored --nocapture --test-threads=1
//! ```
//!
//! - 按键到回显的延迟：输入经宿主（会话线程、写队列）写给程序，程序的回显经宿主（读线程、会话
//!   线程）转回前端。回显的程序是 `cat -u`：先用 `stty raw -echo` 关掉行规程的回显和行缓冲，每个
//!   字节都由 `cat` 自己读进来再写回去，和真实程序一样走一趟进程。`RUNODE_LATENCY_N` 改循环次数，
//!   默认 1 万次。
//! - `cat` 一个大文件的吞吐：从宿主读到输出到界面那份 VT（`Session::feed`）喂完，`RUNODE_THROUGHPUT_MB`
//!   改大小，默认 100 MiB。
//!
//! 都经 `Host::connect_pair` 按协议收发帧，在同一个线程里写帧、读帧，量的是宿主本身；桌面那一层
//! （经 `Link` 的读线程转手）的基准在桌面的 `session_host::link` 里。

mod common;

use std::{
    io::{BufReader, Write as _},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::{Duration, Instant},
};

use runode_host::{ClientMsg, HostMsg, SessionId};
use runode_protocol::{
    AttachMode, BuildId, Caps, ClientKind, Frame, FrameKind, PROTOCOL_VERSION, read_frame, write_frame,
};
use runode_shared_types::{grid::GridSize, shell::IntegrationMode};
use runode_terminal::session::Session;

const LATENCY_SIZE: GridSize = GridSize { cols: 80, rows: 24, cell_width_px: 8, cell_height_px: 16 };
const THROUGHPUT_SIZE: GridSize = GridSize { cols: 200, rows: 50, cell_width_px: 8, cell_height_px: 16 };
const DONE: &[u8] = b"RUNODE-DONE";

/// 基准的结果要给人看，测试框架只在 `--nocapture` 时显示。
#[allow(clippy::print_stderr)]
fn report(line: &str) {
    eprintln!("{line}");
}

fn iterations() -> usize {
    std::env::var("RUNODE_LATENCY_N").ok().and_then(|n| n.parse().ok()).unwrap_or(10_000)
}

fn percentiles(name: &str, mut samples: Vec<Duration>) {
    samples.sort();
    let pct = |p: f64| samples[((samples.len() as f64 * p) as usize).min(samples.len() - 1)];
    report(&format!(
        "{name} n={} p50={:?} p90={:?} p99={:?} max={:?}",
        samples.len(),
        pct(0.5),
        pct(0.9),
        pct(0.99),
        samples[samples.len() - 1]
    ));
}

/// 回显用的脚本。
fn echo_script(dir: &std::path::Path) -> String {
    common::script(dir, "echo.sh", "stty raw -echo\nexec /bin/cat -u")
}

/// `cat` 大文件用的脚本和文件：等一行输入再开始，`cat` 完打印 `DONE`，之后接着当 `cat`。
fn cat_script(dir: &std::path::Path) -> (String, PathBuf, usize) {
    let mb: usize = std::env::var("RUNODE_THROUGHPUT_MB").ok().and_then(|n| n.parse().ok()).unwrap_or(100);
    let data = dir.join("data.txt");
    let line: String = (0..99).map(|i| char::from(b'!' + (i % 90) as u8)).collect::<String>() + "\n";
    let mut file = std::io::BufWriter::new(std::fs::File::create(&data).unwrap());
    for _ in 0..mb * 1024 * 1024 / line.len() {
        file.write_all(line.as_bytes()).unwrap();
    }
    file.flush().unwrap();
    let body = format!("stty -echo\nread go\ncat {}\nprintf 'RUNODE-%s\\n' DONE\nexec /bin/cat", data.display());
    (common::script(dir, "cat.sh", &body), data, mb)
}

/// 记着输出的最后几个字节，认出跨块的 `DONE`。
#[derive(Default)]
struct Tail(Vec<u8>);

impl Tail {
    fn saw_done(&mut self, chunk: &[u8]) -> bool {
        self.0.extend_from_slice(chunk);
        let found = common::contains(&self.0, DONE);
        let keep = self.0.len().saturating_sub(DONE.len());
        self.0.drain(..keep);
        found
    }
}

/// 经 `Host::connect_pair` 连上的桌面：同一个线程里写帧、读帧，不经别的线程转手。
struct PairDesktop {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}

impl PairDesktop {
    fn connect(host: &runode_host::Host) -> Self {
        let stream = host.connect_pair().unwrap();
        let reader = BufReader::with_capacity(256 << 10, stream.try_clone().unwrap());
        let mut desktop = Self { writer: stream, reader };
        desktop.send(&ClientMsg::Hello {
            protocol: PROTOCOL_VERSION,
            build: BuildId(common::BUILD.into()),
            client: ClientKind::Desktop,
            caps: Caps { snapshot: true, vt_replay: true },
            session: None,
            device: None,
        });
        assert!(matches!(desktop.message(), HostMsg::Welcome { .. }));
        desktop
    }

    fn send(&mut self, message: &ClientMsg) {
        let frame = Frame::control(message).unwrap();
        write_frame(&mut self.writer, frame.kind, 0, &frame.payload).unwrap();
    }

    fn frame(&mut self) -> Frame {
        read_frame(&mut self.reader).unwrap().expect("the host closed the connection")
    }

    fn message(&mut self) -> HostMsg {
        loop {
            let frame = self.frame();
            if frame.kind == FrameKind::Control {
                return frame.message().unwrap();
            }
        }
    }

    fn spawn(&mut self, shell: String, size: GridSize) -> SessionId {
        self.send(&ClientMsg::Spawn {
            req: 1,
            size,
            cwd: None,
            integration: IntegrationMode::Off,
            start: true,
            shell: Some(shell),
            settings: None,
            env: Vec::new(),
        });
        loop {
            if let HostMsg::Spawned { id, .. } = self.message() {
                return id;
            }
        }
    }

    /// 连上，返回通道和解出快照的界面 VT。
    fn attach(&mut self, id: SessionId, size: GridSize) -> (u32, Session) {
        self.send(&ClientMsg::Attach { id, size: Some(size), mode: AttachMode::Snapshot });
        let channel = loop {
            if let HostMsg::Attached { channel, mode, .. } = self.message() {
                assert_eq!(mode, AttachMode::Snapshot);
                break channel;
            }
        };
        let mut snapshot = Vec::new();
        loop {
            let frame = self.frame();
            match frame.kind {
                FrameKind::Snapshot => snapshot.extend_from_slice(&frame.payload),
                FrameKind::Control if matches!(frame.message().unwrap(), HostMsg::SnapshotEnd { .. }) => break,
                _ => {}
            }
        }
        (channel, Session::from_snapshot(&snapshot, Box::new(|_| {})).unwrap())
    }

    fn input(&mut self, channel: u32, data: &[u8]) {
        write_frame(&mut self.writer, FrameKind::Input, channel, data).unwrap();
    }

    /// 下一块输出。
    fn output(&mut self, channel: u32) -> Vec<u8> {
        loop {
            let frame = self.frame();
            if frame.kind == FrameKind::Output && frame.channel == channel {
                return frame.payload;
            }
        }
    }
}

#[test]
#[ignore = "测量延迟，手动跑"]
fn echo_latency_through_a_pair() {
    let dir = common::temp_dir("latency-pair");
    let host = common::host();
    let mut desktop = PairDesktop::connect(&host);
    let id = desktop.spawn(echo_script(&dir), LATENCY_SIZE);
    let (channel, _) = desktop.attach(id, LATENCY_SIZE);
    std::thread::sleep(Duration::from_millis(500));
    // 先确认已经是 raw 模式：发一个字节，等到回显。
    desktop.input(channel, b"!");
    while !desktop.output(channel).contains(&b'!') {}

    let mut samples = Vec::with_capacity(iterations());
    for i in 0..iterations() {
        let byte = b'a' + (i % 26) as u8;
        let start = Instant::now();
        desktop.input(channel, &[byte]);
        while !desktop.output(channel).contains(&byte) {}
        samples.push(start.elapsed());
    }
    desktop.send(&ClientMsg::Kill { id });
    percentiles("pair", samples);
}

#[test]
#[ignore = "测量吞吐，手动跑"]
fn cat_throughput_through_a_pair() {
    let dir = common::temp_dir("throughput-pair");
    let (script, data, mb) = cat_script(&dir);
    let host = common::host();
    let mut desktop = PairDesktop::connect(&host);
    let id = desktop.spawn(script, THROUGHPUT_SIZE);
    let (channel, mut session) = desktop.attach(id, THROUGHPUT_SIZE);
    std::thread::sleep(Duration::from_millis(500));
    let start = Instant::now();
    desktop.input(channel, b"go\r");
    let (mut tail, mut total) = (Tail::default(), 0);
    loop {
        let chunk = desktop.output(channel);
        session.feed(&chunk);
        total += chunk.len();
        if tail.saw_done(&chunk) {
            break;
        }
    }
    let elapsed = start.elapsed();
    desktop.send(&ClientMsg::Kill { id });
    let _ = std::fs::remove_file(data);
    report(&format!(
        "pair cat {mb} MiB: {total} bytes in {elapsed:?} = {:.1} MiB/s",
        total as f64 / 1048576.0 / elapsed.as_secs_f64()
    ));
}
