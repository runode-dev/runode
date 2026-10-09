//! 门禁从头到尾：用 rustls 客户端当手机，连回环地址上的监听，宿主换成一对 Unix socket 的假连接。

mod common;

use std::{path::Path, time::Duration};

use common::*;
use runode_protocol::{
    ClientMsg, FrameKind, HostMsg,
    remote::{Bytes, DeviceId, GateClientMsg, GateHostMsg, Purpose, RejectReason, signed_bytes},
    write_frame,
};
use runode_remote_access::{PairingProgress, PairingTicket, list_devices, pairing_pending, revoke_device};

fn rejected(reason: RejectReason) -> Option<GateHostMsg> {
    Some(GateHostMsg::RemoteRejected { reason })
}

#[test]
fn a_phone_pairs_then_logs_in_and_talks_to_the_host() {
    let harness = Harness::start("pair");
    let key = Key::generate();
    assert!(!pairing_pending(&harness.dirs));
    let ticket = harness.ticket();
    assert_eq!(ticket.poll().unwrap(), PairingProgress::Waiting);
    assert!(pairing_pending(&harness.dirs));
    let mut phone = harness.phone();
    assert_eq!(phone.host_name, "测试的 Mac");
    let pair = phone.pair_message(&ticket.secret(), "Ethan 的 iPhone", &key);
    let Some(GateHostMsg::RemoteAccepted { device_id }) = phone.ask(&pair) else { panic!("not accepted") };
    assert_eq!(ticket.poll().unwrap(), PairingProgress::Paired { device_id, name: "Ethan 的 iPhone".into() });
    assert!(!pairing_pending(&harness.dirs));
    let devices = list_devices(&harness.dirs).unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!((devices[0].device_id, devices[0].name.as_str()), (device_id, "Ethan 的 iPhone"));
    assert_eq!(devices[0].public_key.0, key.public());
    // 配好以后口令从文件里抹掉了。
    let pairing = std::fs::read_to_string(harness.dirs.remote_access_pairing_file().unwrap()).unwrap();
    assert!(!pairing.contains(&runode_protocol::remote::encode_base64url(&ticket.secret())), "{pairing}");

    // 门禁过了，第一帧是手机的 Hello，原样转给宿主；之后两个方向照常搬。
    phone.send_control(&Phone::hello("mobile"));
    let host = harness.host();
    assert!(
        matches!(read_client_msg(&host), Some(ClientMsg::Hello { device: Some(ref name), .. }) if name == "测试手机")
    );
    let welcome = HostMsg::Done { req: 7 };
    write_frame(&mut &host, FrameKind::Control, 0, &serde_json::to_vec(&welcome).unwrap()).unwrap();
    assert_eq!(phone.read_host_msg(), welcome);
    phone.send_control(br#"{"type":"list_sessions"}"#);
    assert_eq!(read_client_msg(&host), Some(ClientMsg::ListSessions));
    // 大块的输出也完整送到。
    let big = vec![b'x'; 3 << 20];
    let writer = {
        let host = host.try_clone().unwrap();
        let big = big.clone();
        std::thread::spawn(move || write_frame(&mut &host, FrameKind::Output, 3, &big).unwrap())
    };
    let frame = runode_protocol::read_frame(&mut phone.tls).unwrap().unwrap();
    assert_eq!((frame.kind, frame.channel, frame.payload.len()), (FrameKind::Output, 3, big.len()));
    writer.join().unwrap();

    // 再连一次，用配对时的私钥登录。
    let mut again = harness.phone();
    let auth = again.auth_message(device_id, &key);
    assert_eq!(again.ask(&auth), Some(GateHostMsg::RemoteAccepted { device_id }));

    // 口令只能用一次。
    let mut other = harness.phone();
    let pair = other.pair_message(&ticket.secret(), "别的手机", &Key::generate());
    assert_eq!(other.ask(&pair), rejected(RejectReason::PairingInvalid));
    assert!(other.closed());
    drop(ticket);
    assert!(!harness.dirs.remote_access_pairing_file().unwrap().exists());
}

#[test]
fn wrong_secrets_are_refused_but_do_not_burn_the_code() {
    let harness = Harness::start("wrong");
    let ticket = harness.ticket();
    let key = Key::generate();
    for attempt in 0..5 {
        let mut phone = harness.phone();
        let mut secret = ticket.secret();
        secret[attempt] ^= 1;
        let pair = phone.pair_message(&secret, "猜口令", &key);
        assert_eq!(phone.ask(&pair), rejected(RejectReason::PairingInvalid), "{attempt}");
        assert!(phone.closed());
    }
    // 局域网里别人乱试搅不掉正在进行的配对。
    assert_eq!(ticket.poll().unwrap(), PairingProgress::Waiting);
    let mut phone = harness.phone();
    let pair = phone.pair_message(&ticket.secret(), "口令对的", &key);
    assert!(matches!(phone.ask(&pair), Some(GateHostMsg::RemoteAccepted { .. })));
    assert_eq!(list_devices(&harness.dirs).unwrap().len(), 1);
}

#[test]
fn an_expired_code_is_refused() {
    let harness = Harness::start("expired");
    let ticket = PairingTicket::begin(&harness.dirs, Duration::ZERO).unwrap();
    assert_eq!(ticket.poll().unwrap(), PairingProgress::Expired);
    assert!(!pairing_pending(&harness.dirs));
    let mut phone = harness.phone();
    let pair = phone.pair_message(&ticket.secret(), "过期", &Key::generate());
    assert_eq!(phone.ask(&pair), rejected(RejectReason::PairingInvalid));
    // 没有口令文件（没人在配对）也一样。
    drop(ticket);
    let mut phone = harness.phone();
    let pair = phone.pair_message(&[7; 32], "没人配对", &Key::generate());
    assert_eq!(phone.ask(&pair), rejected(RejectReason::PairingInvalid));
}

#[test]
fn a_newer_code_replaces_the_older_one() {
    let harness = Harness::start("replace");
    let first = harness.ticket();
    let second = harness.ticket();
    assert_eq!(first.poll().unwrap(), PairingProgress::Replaced);
    let mut phone = harness.phone();
    let pair = phone.pair_message(&first.secret(), "旧口令", &Key::generate());
    assert_eq!(phone.ask(&pair), rejected(RejectReason::PairingInvalid));
    // 前一个丢掉时不删后一个的文件。
    drop(first);
    assert_eq!(second.poll().unwrap(), PairingProgress::Waiting);
}

#[test]
fn unknown_devices_and_bad_signatures_are_refused() {
    let harness = Harness::start("auth");
    let key = Key::generate();
    let mut phone = harness.phone();
    let auth = phone.auth_message(DeviceId([9; 16]), &key);
    assert_eq!(phone.ask(&auth), rejected(RejectReason::UnknownDevice));
    assert!(phone.closed());

    let device_id = harness.pair(&key);
    // 别的私钥签的。
    drop(phone);
    let mut phone = harness.phone();
    let auth = phone.auth_message(device_id, &Key::generate());
    assert_eq!(phone.ask(&auth), rejected(RejectReason::BadSignature));
    // 用途不对：配对的签名拿来登录。
    drop(phone);
    let mut phone = harness.phone();
    let signature = Bytes(key.sign(&phone.signed(Purpose::Pair)));
    assert_eq!(phone.ask(&GateClientMsg::RemoteAuth { device_id, signature }), rejected(RejectReason::BadSignature));
    // 配对时口令对、签名不对：拒绝，口令还能用。
    let ticket = harness.ticket();
    drop(phone);
    let mut phone = harness.phone();
    let GateClientMsg::RemotePair { secret, device_name, public_key, .. } =
        phone.pair_message(&ticket.secret(), "签错了", &key)
    else {
        unreachable!()
    };
    let forged = GateClientMsg::RemotePair {
        secret,
        device_name,
        public_key,
        signature: Bytes(Key::generate().sign(&phone.signed(Purpose::Pair))),
    };
    assert_eq!(phone.ask(&forged), rejected(RejectReason::BadSignature));
    drop(phone);
    let mut phone = harness.phone();
    let pair = phone.pair_message(&ticket.secret(), "这次对了", &Key::generate());
    assert!(matches!(phone.ask(&pair), Some(GateHostMsg::RemoteAccepted { .. })));
}

#[test]
fn a_signature_only_works_on_the_connection_it_was_made_for() {
    let harness = Harness::start("exporter");
    let key = Key::generate();
    let device_id = harness.pair(&key);
    let mut a = harness.phone();
    let mut b = harness.phone();
    assert_ne!(a.exporter, b.exporter);
    // 中间人把 B 的 nonce 转给连着 A 的设备签：nonce 对，exporter 是 A 的，B 上验不过。
    let relayed = signed_bytes(&b.nonce, &a.exporter, Purpose::Auth);
    let signature = Bytes(key.sign(&relayed));
    assert_eq!(b.ask(&GateClientMsg::RemoteAuth { device_id, signature }), rejected(RejectReason::BadSignature));
    // A 上自己的签名拿到别的连接上也不行。
    let mut c = harness.phone();
    let replayed = a.auth_message(device_id, &key);
    assert_eq!(c.ask(&replayed), rejected(RejectReason::BadSignature));
    // A 自己用照样行。
    assert_eq!(a.ask(&replayed), Some(GateHostMsg::RemoteAccepted { device_id }));
}

#[test]
fn the_first_frame_after_the_gate_must_be_a_mobile_hello() {
    let harness = Harness::start("hello");
    let key = Key::generate();
    let device_id = harness.pair(&key);
    for first in [Phone::hello("cli"), Phone::hello("successor"), br#"{"type":"list_sessions"}"#.to_vec()] {
        let mut phone = harness.phone();
        let auth = phone.auth_message(device_id, &key);
        assert_eq!(phone.ask(&auth), Some(GateHostMsg::RemoteAccepted { device_id }));
        phone.send_control(&first);
        assert!(phone.closed(), "{}", String::from_utf8_lossy(&first));
    }
    // 不是控制帧也不行。
    let mut phone = harness.phone();
    let auth = phone.auth_message(device_id, &key);
    assert_eq!(phone.ask(&auth), Some(GateHostMsg::RemoteAccepted { device_id }));
    write_frame(&mut phone.tls, FrameKind::Input, 1, &Phone::hello("mobile")).unwrap();
    std::io::Write::flush(&mut phone.tls).unwrap();
    assert!(phone.closed());
    // 没有一条连到了宿主。
    assert!(harness.hosts.try_recv().is_err());
}

#[test]
fn revoking_a_device_disconnects_it() {
    let harness = Harness::start("revoke");
    let key = Key::generate();
    let device_id = harness.pair(&key);
    let other_key = Key::generate();
    let other_id = harness.pair(&other_key);
    let connect = |key: &Key, id: DeviceId| {
        let mut phone = harness.phone();
        let auth = phone.auth_message(id, key);
        assert_eq!(phone.ask(&auth), Some(GateHostMsg::RemoteAccepted { device_id: id }));
        phone.send_control(&Phone::hello("mobile"));
        let host = harness.host();
        assert!(matches!(read_client_msg(&host), Some(ClientMsg::Hello { .. })));
        (phone, host)
    };
    let (mut phone, host) = connect(&key, device_id);
    let (mut other, other_host) = connect(&other_key, other_id);
    assert!(revoke_device(&harness.dirs, device_id).unwrap());
    assert!(!revoke_device(&harness.dirs, device_id).unwrap());
    // 几秒内断开，两头都断。
    assert!(phone.closed());
    assert_eq!(read_client_msg(&host), None);
    // 别的设备不受影响。
    let done = HostMsg::Done { req: 1 };
    write_frame(&mut &other_host, FrameKind::Control, 0, &serde_json::to_vec(&done).unwrap()).unwrap();
    assert_eq!(other.read_host_msg(), done);
    // 撤销了的设备再也登录不了。
    let mut phone = harness.phone();
    let auth = phone.auth_message(device_id, &key);
    assert_eq!(phone.ask(&auth), rejected(RejectReason::UnknownDevice));
}

#[test]
fn the_host_hanging_up_closes_the_phone_connection() {
    let harness = Harness::start("hangup");
    let key = Key::generate();
    let device_id = harness.pair(&key);
    let mut phone = harness.phone();
    let auth = phone.auth_message(device_id, &key);
    assert_eq!(phone.ask(&auth), Some(GateHostMsg::RemoteAccepted { device_id }));
    phone.send_control(&Phone::hello("mobile"));
    let host = harness.host();
    read_client_msg(&host).unwrap();
    let bye = HostMsg::Goodbye { reason: runode_protocol::GoodbyeReason::Shutdown };
    write_frame(&mut &host, FrameKind::Control, 0, &serde_json::to_vec(&bye).unwrap()).unwrap();
    drop(host);
    // 宿主最后说的话送到了，然后连接关了。
    assert_eq!(phone.read_host_msg(), bye);
    assert!(phone.closed());
}

#[test]
fn too_many_failures_from_one_address_are_rate_limited() {
    let harness = Harness::start("limit");
    let device_id = harness.pair(&Key::generate());
    // 签名和口令不对的都算。
    let ticket = harness.ticket();
    for attempt in 0..10 {
        let mut phone = harness.phone();
        let (message, reason) = if attempt % 2 == 0 {
            (phone.auth_message(device_id, &Key::generate()), RejectReason::BadSignature)
        } else {
            (phone.pair_message(&[7; 32], "猜口令", &Key::generate()), RejectReason::PairingInvalid)
        };
        assert_eq!(phone.ask(&message), rejected(reason), "{attempt}");
    }
    // 之后连握手都不做就关掉，口令对的配对也进不来。
    assert!(Phone::connect(harness.addr, harness.fingerprint).is_err());
    assert_eq!(ticket.poll().unwrap(), PairingProgress::Waiting);
}

/// 只开 TCP 就关、门禁消息读不出、设备没登记（撤销后手机自动重试）都不算失败：同一网段里谁都能替别人
/// 攒满额度，撤销后马上重新配对也不该被挡住。
#[test]
fn only_wrong_signatures_and_codes_count_as_failures() {
    let harness = Harness::start("garbage");
    for _ in 0..12 {
        drop(std::net::TcpStream::connect(harness.addr).unwrap());
        // 等这条的线程收尾，免得占着门禁的名额。
        std::thread::sleep(Duration::from_millis(20));
    }
    for attempt in 0..12 {
        let mut phone = harness.phone();
        if attempt % 2 == 0 {
            phone.send_control(br#"{"type":"remote_whatever"}"#);
        } else {
            phone.send_control(b"not json");
        }
        assert!(phone.closed(), "{attempt}");
    }
    for _ in 0..12 {
        let mut phone = harness.phone();
        let auth = phone.auth_message(DeviceId([1; 16]), &Key::generate());
        assert_eq!(phone.ask(&auth), rejected(RejectReason::UnknownDevice));
    }
    harness.pair(&Key::generate());
}

#[test]
fn one_source_cannot_fill_the_gate() {
    let harness = Harness::start("gating");
    // 只连上 TCP、不握手的连接占着门禁。
    let idle: Vec<_> = (0..4).map(|_| std::net::TcpStream::connect(harness.addr).unwrap()).collect();
    assert!(Phone::connect(harness.addr, harness.fingerprint).is_err());
    drop(idle);
    // 放开以后又连得上（那几条读到结尾，不算失败）。
    let start = std::time::Instant::now();
    while Phone::connect(harness.addr, harness.fingerprint).is_err() {
        assert!(start.elapsed() < PATIENCE, "still refused after the idle connections closed");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_phone_pinning_another_certificate_does_not_connect() {
    let harness = Harness::start("pin");
    let mut wrong = harness.fingerprint;
    wrong[0] ^= 1;
    assert!(Phone::connect(harness.addr, wrong).is_err());
}

#[test]
fn the_certificate_stays_the_same_across_restarts() {
    let mut harness = Harness::start("restart");
    let cert = harness.dirs.remote_access_cert_file().unwrap();
    let key = harness.dirs.remote_access_key_file().unwrap();
    for path in [&cert, &key] {
        assert_eq!(mode(path), 0o600, "{}", path.display());
    }
    assert_eq!(mode(&harness.dirs.remote_access_dir().unwrap()), 0o700);
    // 同一时刻只有一个监听方。
    let second = runode_remote_access::Listener::start(options(
        &harness.dirs,
        std::sync::Arc::new(|| Err(std::io::Error::other("unused"))),
    ));
    assert_eq!(second.err().map(|err| err.kind()), Some(std::io::ErrorKind::AddrInUse));
    // 停了再开，证书不变，配对过的手机照样认得。
    let before = harness.fingerprint;
    assert_eq!(harness.restart(), before);
    let mut phone = harness.phone();
    assert_eq!(phone.host_name, "测试的 Mac");
    let auth = phone.auth_message(DeviceId([3; 16]), &Key::generate());
    assert_eq!(phone.ask(&auth), rejected(RejectReason::UnknownDevice));
    // 停下以后监听方的状态没了，命令行据此知道远程访问没开。
    harness.listener = None;
    assert_eq!(runode_remote_access::listener_status(&harness.dirs).unwrap(), None);
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn the_reference_signature_verifies() {
    // OpenSSL 签的样例，手机端的测试也读它：DER 签名、X9.63 公钥的写法和这边对得上。
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../protocol/tests/fixtures/remote/signature_p256.json");
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let key = fixture_bytes(&json["public_key"]);
    let signed = fixture_bytes(&json["signed"]);
    let signature = fixture_bytes(&json["signature"]);
    let verify = |signed: &[u8]| {
        ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_ASN1, &key)
            .verify(signed, &signature)
            .is_ok()
    };
    assert!(verify(&signed));
    let mut tampered = signed;
    tampered[0] ^= 1;
    assert!(!verify(&tampered));
}

#[test]
fn a_device_keeps_at_most_eight_connections() {
    let harness = Harness::start("perdevice");
    let key = Key::generate();
    let device_id = harness.pair(&key);
    let mut connections: Vec<_> = (0..8).map(|_| harness.bridged(device_id, &key)).collect();
    // 第九条连上来，最早那条断开，别的照旧。
    let newest = harness.bridged(device_id, &key);
    let (mut oldest, _) = connections.remove(0);
    assert!(oldest.closed());
    let (mut second, host) = connections.remove(0);
    let done = HostMsg::Done { req: 2 };
    write_frame(&mut &host, FrameKind::Control, 0, &serde_json::to_vec(&done).unwrap()).unwrap();
    assert_eq!(second.read_host_msg(), done);
    drop(newest);
}

/// 停掉的监听的连接线程晚收尾时不重写状态文件，不然重开的监听写好的新端口会被换回旧的。
#[test]
fn a_stopped_listener_does_not_bring_its_status_back() {
    use std::{
        os::unix::net::UnixStream,
        sync::{Arc, Mutex, mpsc},
    };

    let mut harness = Harness::start("stopped");
    let key = Key::generate();
    let device_id = harness.pair(&key);
    // 换一个要宿主连接时停住、等测试放行的监听，那条连接的线程就晚于重开的监听收尾。
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let (entered_tx, release_rx) = (Mutex::new(entered_tx), Mutex::new(release_rx));
    let connect = Arc::new(move || {
        entered_tx.lock().unwrap().send(()).unwrap();
        let _ = release_rx.lock().unwrap().recv();
        Ok(UnixStream::pair()?.0)
    });
    harness.listener = None;
    let listener = runode_remote_access::Listener::start(options(&harness.dirs, connect)).unwrap();
    harness.addr = listener.local_addrs()[0];
    harness.listener = Some(listener);
    let mut phone = harness.phone();
    let auth = phone.auth_message(device_id, &key);
    assert_eq!(phone.ask(&auth), Some(GateHostMsg::RemoteAccepted { device_id }));
    phone.send_control(&Phone::hello("mobile"));
    entered.recv_timeout(PATIENCE).unwrap();
    harness.restart();
    release.send(()).unwrap();
    assert!(phone.closed());
    std::thread::sleep(Duration::from_millis(300));
    let status = runode_remote_access::listener_status(&harness.dirs).unwrap().unwrap();
    assert_eq!((status.port, status.connected), (harness.addr.port(), Vec::new()));
}

#[test]
fn the_status_file_lists_the_connected_devices() {
    let harness = Harness::start("connected");
    let key = Key::generate();
    let device_id = harness.pair(&key);
    let connected = || runode_remote_access::listener_status(&harness.dirs).unwrap().unwrap().connected;
    let wait_for = |want: &[DeviceId]| {
        let start = std::time::Instant::now();
        while connected() != want {
            assert!(start.elapsed() < PATIENCE, "connected stays {:?}, want {want:?}", connected());
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let (phone, host) = harness.bridged(device_id, &key);
    let (second, second_host) = harness.bridged(device_id, &key);
    // 同一台设备连着两条也只算一次。
    wait_for(&[device_id]);
    drop((phone, host));
    wait_for(&[device_id]);
    drop((second, second_host));
    wait_for(&[]);
}
