//! 这台电脑当客户端（`runode_remote::client`）：对着进程内的监听配对、连上，交回的 socket 接到对面的远程桌面
//! 上（监听方现在只认手机的终端 `Hello`，远程桌面的 `ScreenHello` 不分设备种类）；撤销、指纹不对、链接过期、
//! 忘掉以后各报什么，死地址不拖慢。

mod common;

use std::{
    io::{Read as _, Write as _},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    time::{Duration, Instant},
};

use common::*;
use runode_paths::Dirs;
use runode_protocol::{
    FrameKind, read_frame,
    remote::{PAIRING_TTL, PairingUri, RejectReason},
    screen::{SCREEN_VERSION, ScreenClientMsg},
    write_frame,
};
use runode_remote::{
    PairingProgress, PairingTicket,
    client::{self, DialError, Dialed},
    list_devices, now_unix, revoke_device,
};

/// 这边（客户端）的数据目录，丢掉时删掉。
struct Client {
    root: PathBuf,
    dirs: Dirs,
}

impl Client {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("rrc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let dirs = Dirs::from_vars(|_| Some(root.clone().into()));
        Self { root, dirs }
    }

    fn machines_file(&self) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(self.dirs.paired_machines_file().unwrap()).unwrap()).unwrap()
    }

    fn write_machines_file(&self, value: &serde_json::Value) {
        std::fs::write(self.dirs.paired_machines_file().unwrap(), serde_json::to_vec(value).unwrap()).unwrap();
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
/// 文档用的地址（TEST-NET-1），连不上：要么马上报网络不可达，要么一直没有回应。
const DEAD: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));

/// 监听方 `harness` 的配对链接，口令是 `ticket` 的，地址是 `addrs`。
fn uri(harness: &Harness, ticket: &PairingTicket, addrs: &[IpAddr]) -> PairingUri {
    PairingUri {
        host_name: "测试的 Mac".into(),
        fingerprint: harness.fingerprint,
        secret: ticket.secret(),
        port: harness.addr.port(),
        addrs: addrs.to_vec(),
        expires_at: ticket.expires_at(),
        node: None,
    }
}

fn pair(harness: &Harness, client: &Client) -> Dialed {
    let ticket = harness.ticket();
    client::pair(&client.dirs, &uri(harness, &ticket, &[LOOPBACK]), "测试的电脑").unwrap()
}

/// 在交回的 socket 上开远程桌面：发 `ScreenHello`，对面的采集进程收到；它回的字节这边原样读到。返回采集进程
/// 那一端。
fn open_screen(harness: &Harness, dialed: &Dialed) -> UnixStream {
    let hello = serde_json::to_vec(&ScreenClientMsg::ScreenHello { version: SCREEN_VERSION }).unwrap();
    write_frame(&mut &dialed.stream, FrameKind::Control, 0, &hello).unwrap();
    let screen = harness.screens.recv_timeout(PATIENCE).expect("no connection to the screen process");
    screen.set_read_timeout(Some(PATIENCE)).unwrap();
    let frame = read_frame(&mut &screen).unwrap().expect("the bridge closed");
    assert_eq!(frame.message::<ScreenClientMsg>().unwrap(), ScreenClientMsg::ScreenHello { version: SCREEN_VERSION });
    (&screen).write_all(b"ready").unwrap();
    dialed.stream.set_read_timeout(Some(PATIENCE)).unwrap();
    let mut reply = [0u8; 5];
    (&dialed.stream).read_exact(&mut reply).unwrap();
    assert_eq!(&reply, b"ready");
    screen
}

/// `stream` 读到了结尾（读超时、出错时测试失败）。
fn reads_to_end(mut stream: &UnixStream) -> bool {
    stream.set_read_timeout(Some(PATIENCE)).unwrap();
    let mut rest = Vec::new();
    stream.read_to_end(&mut rest).unwrap();
    rest.is_empty()
}

#[test]
fn pairing_remembers_the_machine() {
    let harness = Harness::start("client-pair");
    let client = Client::new("pair");
    let ticket = harness.ticket();
    let dialed = client::pair(&client.dirs, &uri(&harness, &ticket, &[LOOPBACK]), "测试的电脑").unwrap();

    let devices = list_devices(&harness.dirs).unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, "测试的电脑");
    assert_eq!(
        ticket.poll().unwrap(),
        PairingProgress::Paired { device_id: devices[0].device_id, name: "测试的电脑".into() }
    );
    let machine = &dialed.machine;
    assert_eq!(machine.id.len(), 32);
    assert!(machine.id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "{}", machine.id);
    assert_eq!(machine.host_name, "测试的 Mac");
    assert_eq!(machine.fingerprint, harness.fingerprint);
    assert_eq!((machine.port, machine.addrs.as_slice()), (harness.addr.port(), [LOOPBACK].as_slice()));
    assert_eq!((machine.last_addr, dialed.addr), (Some(harness.addr), Some(harness.addr)));
    assert_eq!(machine.device_id, devices[0].device_id);
    assert!(machine.paired_at.abs_diff(now_unix()) < 60);
    assert_eq!(client::list_machines(&client.dirs), std::slice::from_ref(machine));
    assert_eq!(client::find_machine(&client.dirs, &machine.id).as_ref(), Some(machine));

    // 文件里带着私钥，只有自己能读。
    let file = client.machines_file();
    assert_eq!(file["version"], 1);
    assert!(file["machines"][0]["key"].as_str().is_some_and(|key| !key.is_empty()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(client.dirs.paired_machines_file().unwrap()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // 对面的采集进程关了，这边读到结尾。
    let screen = open_screen(&harness, &dialed);
    drop(screen);
    assert!(reads_to_end(&dialed.stream));
}

#[test]
fn dialing_connects_to_a_paired_machine() {
    let harness = Harness::start("client-dial");
    let client = Client::new("dial");
    let paired = pair(&harness, &client);
    drop(paired.stream);

    let dialed = client::dial(&client.dirs, &paired.machine.id).unwrap();
    assert_eq!(dialed.machine, paired.machine);
    assert_eq!(dialed.addr, Some(harness.addr));
    // 这边关了，对面的采集进程读到结尾。
    let screen = open_screen(&harness, &dialed);
    drop(dialed);
    assert!(reads_to_end(&screen));
}

/// 大块的数据两个方向都原样到，不丢不乱。
#[test]
fn large_transfers_arrive_intact() {
    let harness = Harness::start("client-bulk");
    let client = Client::new("bulk");
    let dialed = pair(&harness, &client);
    let screen = open_screen(&harness, &dialed);
    // 监听方照帧头数边界，发去的得是一帧帧的。
    let noise: Vec<u8> = (0..64 << 10).map(|n: u32| (n.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
    let mut data = Vec::new();
    for n in 0..64 {
        write_frame(&mut data, FrameKind::Input, n, &noise[..noise.len() - n as usize]).unwrap();
    }
    let read_all = |mut stream: &UnixStream| {
        let mut got = vec![0u8; data.len()];
        stream.read_exact(&mut got).unwrap();
        got == data
    };
    std::thread::scope(|scope| {
        scope.spawn(|| (&screen).write_all(&data).unwrap());
        scope.spawn(|| (&dialed.stream).write_all(&data).unwrap());
        let up = scope.spawn(|| read_all(&screen));
        assert!(read_all(&dialed.stream));
        assert!(up.join().unwrap());
    });
}

#[test]
fn a_revoked_device_is_rejected() {
    let harness = Harness::start("client-revoked");
    let client = Client::new("revoked");
    let paired = pair(&harness, &client);
    drop(paired.stream);
    assert!(revoke_device(&harness.dirs, paired.machine.device_id).unwrap());

    match client::dial(&client.dirs, &paired.machine.id) {
        Err(DialError::Rejected(RejectReason::UnknownDevice)) => {}
        other => panic!("expected UnknownDevice, got {:?}", other.map(|dialed| dialed.machine)),
    }
}

#[test]
fn a_wrong_fingerprint_is_refused() {
    let harness = Harness::start("client-fingerprint");
    let client = Client::new("fingerprint");
    let ticket = harness.ticket();
    let mut wrong = uri(&harness, &ticket, &[LOOPBACK]);
    wrong.fingerprint[0] ^= 1;
    assert!(matches!(client::pair(&client.dirs, &wrong, "测试的电脑"), Err(DialError::FingerprintMismatch)));
    // 口令没交出去，还能用。
    assert_eq!(ticket.poll().unwrap(), PairingProgress::Waiting);
    let paired = client::pair(&client.dirs, &uri(&harness, &ticket, &[LOOPBACK]), "测试的电脑").unwrap();
    drop(paired.stream);

    // 存下的指纹和那边的证书对不上时，连也一样报。
    let mut file = client.machines_file();
    let fingerprint = file["machines"][0]["fingerprint"].as_str().unwrap().to_owned();
    let flipped = if fingerprint.starts_with('A') { "B" } else { "A" };
    file["machines"][0]["fingerprint"] = format!("{flipped}{}", &fingerprint[1..]).into();
    client.write_machines_file(&file);
    assert!(matches!(client::dial(&client.dirs, &paired.machine.id), Err(DialError::FingerprintMismatch)));
}

#[test]
fn expired_links_and_wrong_codes_are_reported() {
    let harness = Harness::start("client-expired");
    let client = Client::new("expired");
    let ticket = harness.ticket();
    let mut expired = uri(&harness, &ticket, &[LOOPBACK]);
    expired.expires_at = now_unix() - 1;
    assert!(matches!(client::pair(&client.dirs, &expired, "测试的电脑"), Err(DialError::Expired)));

    // 那边已经过期、链接上写的还没到：按那边的拒绝报。
    let stale = PairingTicket::begin(&harness.dirs, Duration::ZERO).unwrap();
    let mut early = uri(&harness, &stale, &[LOOPBACK]);
    early.expires_at = now_unix() + PAIRING_TTL.as_secs();
    assert!(matches!(
        client::pair(&client.dirs, &early, "测试的电脑"),
        Err(DialError::Rejected(RejectReason::PairingInvalid))
    ));
    assert!(client::list_machines(&client.dirs).is_empty());
}

#[test]
fn forgetting_drops_the_machine() {
    let harness = Harness::start("client-forget");
    let client = Client::new("forget");
    let first = pair(&harness, &client);
    let second = pair(&harness, &client);
    assert_ne!(first.machine.id, second.machine.id);
    assert_eq!(client::list_machines(&client.dirs), [first.machine.clone(), second.machine.clone()]);

    client::forget_machine(&client.dirs, &first.machine.id).unwrap();
    assert_eq!(client::list_machines(&client.dirs), std::slice::from_ref(&second.machine));
    assert_eq!(client::find_machine(&client.dirs, &first.machine.id), None);
    assert!(matches!(client::dial(&client.dirs, &first.machine.id), Err(DialError::Io(_))));
    // 忘掉一台不认识的不算错。
    client::forget_machine(&client.dirs, &first.machine.id).unwrap();
    client::dial(&client.dirs, &second.machine.id).unwrap();
}

/// 排在前面的死地址不拖着后面通的那个：配对时链接里的、连的时候上次的地址死了都一样，连上以后上次的地址
/// 换成通的那个。
#[test]
fn dead_addresses_do_not_hold_up_live_ones() {
    let harness = Harness::start("client-dead");
    let client = Client::new("dead");
    let ticket = harness.ticket();
    let started = Instant::now();
    let paired = client::pair(&client.dirs, &uri(&harness, &ticket, &[DEAD, LOOPBACK]), "测试的电脑").unwrap();
    assert!(started.elapsed() < Duration::from_secs(2), "pairing took {:?}", started.elapsed());
    assert_eq!(paired.addr, Some(harness.addr));
    drop(paired.stream);

    let mut file = client.machines_file();
    let dead = SocketAddr::new(DEAD, harness.addr.port());
    file["machines"][0]["last_addr"] = dead.to_string().into();
    client.write_machines_file(&file);
    assert_eq!(client::find_machine(&client.dirs, &paired.machine.id).unwrap().last_addr, Some(dead));
    let started = Instant::now();
    let dialed = client::dial(&client.dirs, &paired.machine.id).unwrap();
    assert!(started.elapsed() < Duration::from_secs(2), "dialing took {:?}", started.elapsed());
    assert_eq!(dialed.addr, Some(harness.addr));
    assert_eq!(client::find_machine(&client.dirs, &paired.machine.id).unwrap().last_addr, Some(harness.addr));
}

/// 哪个地址都不通时报连不上，不等满 `DIAL_TIMEOUT`。
#[test]
fn unreachable_machines_fail_quickly() {
    let harness = Harness::start("client-unreachable");
    let client = Client::new("unreachable");
    let paired = pair(&harness, &client);
    drop(paired.stream);
    drop(harness);

    let started = Instant::now();
    match client::dial(&client.dirs, &paired.machine.id) {
        Err(DialError::Unreachable { last }) => assert!(last.contains("127.0.0.1"), "{last}"),
        other => panic!("expected Unreachable, got {:?}", other.map(|dialed| dialed.machine)),
    }
    assert!(started.elapsed() < Duration::from_secs(5), "giving up took {:?}", started.elapsed());
}

/// 经 iroh 跨网络配对、再连上：链接里没有地址，只有 `node`。要连得上 n0 的中继和 DNS，手动跑：
/// `cargo test -p runode-remote --test client -- --ignored`。
#[test]
#[ignore = "needs the internet"]
fn pairs_and_dials_across_networks() {
    let mut harness = Harness::start("client-tunnel");
    // 同一个数据目录只能开一个监听：换成开着跨网络连接的。
    harness.listener = None;
    let client = Client::new("tunnel");
    let (tx, screens) = std::sync::mpsc::channel();
    let screen_tx = std::sync::Mutex::new(tx);
    let options = runode_remote::Options {
        tunnel: true,
        connect_screen: std::sync::Arc::new(move || {
            let (ours, theirs) = UnixStream::pair()?;
            screen_tx.lock().unwrap().send(theirs).map_err(std::io::Error::other)?;
            Ok(ours)
        }),
        ..options(&harness.dirs, std::sync::Arc::new(|| Err(std::io::Error::other("no host"))))
    };
    let listener = runode_remote::Listener::start(options).unwrap();
    let status = runode_remote::listener_status(&harness.dirs).unwrap().unwrap();
    let ticket = harness.ticket();
    let link = PairingUri {
        host_name: status.host_name.clone(),
        fingerprint: status.fingerprint.0.clone().try_into().unwrap(),
        secret: ticket.secret(),
        port: listener.port(),
        addrs: Vec::new(),
        expires_at: ticket.expires_at(),
        node: status.node.clone(),
    };
    assert!(link.node.is_some());
    // 刚开的端点要过一会儿才把地址发布到 n0 的 DNS 上，在那之前查不到它。
    let started = Instant::now();
    let paired = loop {
        match client::pair(&client.dirs, &link, "测试的电脑") {
            Err(DialError::Unreachable { .. }) if started.elapsed() < Duration::from_secs(60) => {
                std::thread::sleep(Duration::from_secs(2));
            }
            other => break other.unwrap(),
        }
    };
    assert_eq!((paired.addr, paired.machine.last_addr), (None, None));
    assert_eq!(paired.machine.node, status.node);
    drop(paired.stream);

    let dialed = client::dial(&client.dirs, &paired.machine.id).unwrap();
    assert_eq!(dialed.addr, None);
    let hello = serde_json::to_vec(&ScreenClientMsg::ScreenHello { version: SCREEN_VERSION }).unwrap();
    write_frame(&mut &dialed.stream, FrameKind::Control, 0, &hello).unwrap();
    let screen = screens.recv_timeout(Duration::from_secs(20)).expect("no connection to the screen process");
    screen.set_read_timeout(Some(PATIENCE)).unwrap();
    assert!(read_frame(&mut &screen).unwrap().is_some());
    drop(dialed);
    assert!(reads_to_end(&screen));
}
