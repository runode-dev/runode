//! 本机回环上走一遍 UDP 通道：打通、按顺序收到帧。丢包和重发由 `datagram` 里的单元测试管。

use std::{
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use runode_remote::datagram::{Event, Key, Path, Receiver, Sender};

fn localhost(port: u16) -> SocketAddr {
    (Ipv4Addr::LOCALHOST, port).into()
}

#[test]
fn keys_read_back_only_from_plain_hex() {
    let hex = "0f".repeat(32);
    assert_eq!(Key::from_hex(&hex).map(|key| key.to_hex()), Some(hex.clone()));
    assert!(Key::from_hex(&format!("+f{}", &hex[2..])).is_none());
    assert!(Key::from_hex(&hex[2..]).is_none());
    assert!(Key::from_hex(&format!("zz{}", &hex[2..])).is_none());
}

#[test]
fn delivers_frames_over_loopback() {
    let receiver = Receiver::bind().unwrap();
    let sender = Sender::start(receiver.key(), vec![localhost(receiver.port())], 8_000_000).unwrap();
    let (tx, rx) = mpsc::channel();
    let _receiving = receiver
        .start(vec![localhost(sender.port())], move |event| {
            let _ = tx.send(event);
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while sender.path() != Path::Ready {
        assert!(Instant::now() < deadline, "the path never became ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    // 第一帧是关键帧（标志位第 0 位），之后的按顺序到；大的帧要切成很多片。
    let frames: Vec<Vec<u8>> = (0..5u8)
        .map(|index| {
            let mut payload = vec![u8::from(index == 0)];
            payload.extend(std::iter::repeat_n(index, 1000 + usize::from(index) * 3000));
            payload
        })
        .collect();
    for frame in &frames {
        assert!(sender.send(frame.clone()));
        // 队列只有几帧长，等它发出去。
        while sender.queued() > 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    for frame in frames {
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), Event::Frame(frame));
    }
}

/// 密钥不对的一头打不通。
#[test]
fn a_wrong_key_never_connects() {
    let receiver = Receiver::bind().unwrap();
    let other = Receiver::bind().unwrap();
    let sender = Sender::start(other.key(), vec![localhost(receiver.port())], 8_000_000).unwrap();
    let _receiving = receiver.start(vec![localhost(sender.port())], |_| {}).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(sender.path(), Path::Connecting);
}

/// 观看端不再发包，电脑过一阵认定断了，调用方据此把画面改回原来的连接。
#[test]
fn the_path_fails_when_the_viewer_goes_quiet() {
    let receiver = Receiver::bind().unwrap();
    let sender = Sender::start(receiver.key(), vec![localhost(receiver.port())], 8_000_000).unwrap();
    let receiving = receiver.start(vec![localhost(sender.port())], |_| {}).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while sender.path() != Path::Ready {
        assert!(Instant::now() < deadline, "the path never became ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(receiving);
    let deadline = Instant::now() + Duration::from_secs(5);
    while sender.path() != Path::Failed {
        assert!(Instant::now() < deadline, "the quiet path never failed");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!sender.send(vec![1]), "no video once the path failed");
}

/// 只有电脑到观看端这个方向不通了：观看端过一阵不再 `Ping`，电脑照样认定断了，画面不会卡在 UDP 上。
#[test]
fn the_path_fails_when_only_the_host_to_viewer_direction_breaks() {
    let receiver = Receiver::bind().unwrap();
    // 两头都只认识中转的地址；按来源端口转给另一头，`cut` 后电脑发来的扔掉。
    let relay = UdpSocket::bind(localhost(0)).unwrap();
    relay.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
    let relay_addr = relay.local_addr().unwrap();
    let sender = Sender::start(receiver.key(), vec![relay_addr], 8_000_000).unwrap();
    let (viewer, host) = (localhost(receiver.port()), localhost(sender.port()));
    let (cut, stop) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    let relaying = {
        let (cut, stop) = (cut.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut buffer = vec![0u8; 65536];
            while !stop.load(Ordering::Relaxed) {
                let Ok((len, from)) = relay.recv_from(&mut buffer) else { continue };
                if from == viewer {
                    let _ = relay.send_to(&buffer[..len], host);
                } else if from == host && !cut.load(Ordering::Relaxed) {
                    let _ = relay.send_to(&buffer[..len], viewer);
                }
            }
        })
    };
    let _receiving = receiver.start(vec![relay_addr], |_| {}).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while sender.path() != Path::Ready {
        assert!(Instant::now() < deadline, "the path never became ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    cut.store(true, Ordering::Relaxed);
    let deadline = Instant::now() + Duration::from_secs(8);
    while sender.path() != Path::Failed {
        assert!(Instant::now() < deadline, "the one-way path never failed");
        std::thread::sleep(Duration::from_millis(50));
    }
    stop.store(true, Ordering::Relaxed);
    relaying.join().unwrap();
}
