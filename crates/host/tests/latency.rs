//! 按键到回显的延迟基准：输入经宿主（会话线程、写队列）写给程序，程序的回显经宿主（读线程、
//! 会话线程）转回前端。手动跑：
//!
//! ```sh
//! cargo test --release -p runode-host --test latency -- --ignored --nocapture
//! ```
//!
//! 回显的程序是 `cat -u`：先用 `stty raw -echo` 关掉行规程的回显和行缓冲，每个字节都由 `cat`
//! 自己读进来再写回去，和真实程序一样走一趟进程。`RUNODE_LATENCY_N` 改循环次数，默认 1 万次。

use std::{
    io::Write as _,
    sync::mpsc,
    time::{Duration, Instant},
};

use runode_host::{ClientMsg, Host, HostEvent, SpawnOptions};
use runode_shared_types::{grid::GridSize, shell::IntegrationMode};

#[test]
#[ignore = "测量延迟，手动跑"]
fn echo_latency_through_the_host() {
    let iterations: usize = std::env::var("RUNODE_LATENCY_N").ok().and_then(|n| n.parse().ok()).unwrap_or(10_000);
    // 登录 shell 会多带一个 `-l` 参数，用脚本包一层，脚本不看参数。
    let script = std::env::temp_dir().join(format!("runode-latency-{}.sh", std::process::id()));
    std::fs::File::create(&script)
        .and_then(|mut file| file.write_all(b"#!/bin/sh\nstty raw -echo\nexec /bin/cat -u\n"))
        .unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let client = Host::new().connect_in_process();
    let id = client
        .spawn(SpawnOptions {
            size: GridSize { cols: 80, rows: 24, cell_width_px: 8, cell_height_px: 16 },
            cwd: None,
            integration: IntegrationMode::Off,
            start: true,
            shell: Some(script.to_string_lossy().into_owned()),
            settings: None,
        })
        .unwrap();
    let (tx, rx) = mpsc::channel();
    client.attach(id, Box::new(move |event| tx.send(event).is_ok())).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let wait_for = |byte: u8| loop {
        match rx.recv_timeout(Duration::from_secs(10)).expect("no echo") {
            HostEvent::Output(data) if data.contains(&byte) => return,
            _ => {}
        }
    };
    // 先确认已经是 raw 模式：发一个字节，等到回显。
    client.input(id, b"!".to_vec());
    wait_for(b'!');

    let mut samples = Vec::with_capacity(iterations);
    for i in 0..iterations {
        let byte = b'a' + (i % 26) as u8;
        let start = Instant::now();
        client.input(id, vec![byte]);
        wait_for(byte);
        samples.push(start.elapsed());
    }
    client.send(ClientMsg::Kill { id });
    let _ = std::fs::remove_file(&script);

    samples.sort();
    let pct = |p: f64| samples[((samples.len() as f64 * p) as usize).min(samples.len() - 1)];
    let report = format!(
        "host n={} p50={:?} p90={:?} p99={:?} max={:?}",
        samples.len(),
        pct(0.5),
        pct(0.9),
        pct(0.99),
        samples[samples.len() - 1]
    );
    // 基准的结果要给人看，测试框架只在 `--nocapture` 时显示。
    #[allow(clippy::print_stderr)]
    {
        eprintln!("{report}");
    }
}
