//! 别的电脑上的 Runode 配对成桌面设备：设备表记下种类，门禁后按种类认第一帧，桌面设备能开专用连接。

mod common;

use std::{
    io::{Read as _, Write as _},
    net::{Ipv4Addr, TcpListener},
};

use common::*;
use runode_protocol::{
    ClientKind, ClientMsg, HostMsg,
    remote::{ChannelHello, ChannelRefusal, ChannelReply, DeviceKind, GateHostMsg},
};
use runode_remote::list_devices;

#[test]
fn pairing_records_the_kind_of_device() {
    let harness = Harness::start("kind");
    let ticket = harness.ticket();
    let mut computer = harness.phone();
    let pair =
        computer.pair_message_as(&ticket.secret(), "Ethan 的 MacBook Pro", &Key::generate(), DeviceKind::Desktop);
    let Some(GateHostMsg::RemoteAccepted { device_id }) = computer.ask(&pair) else { panic!("not accepted") };
    let phone = harness.pair(&Key::generate());
    let devices = list_devices(&harness.dirs).unwrap();
    let kinds: Vec<_> = devices.iter().map(|device| (device.device_id, device.kind)).collect();
    assert_eq!(kinds, [(device_id, DeviceKind::Desktop), (phone, DeviceKind::Mobile)]);
}

/// 配对一台 `kind` 的设备，登录上来、还没发第一帧。
fn logged_in(harness: &Harness, kind: DeviceKind) -> Phone {
    let key = Key::generate();
    let device_id = harness.pair_as(&key, kind);
    harness.login(device_id, &key)
}

fn send_channel(device: &mut Phone, hello: &ChannelHello) {
    device.send_control(&serde_json::to_vec(hello).unwrap());
}

#[test]
fn the_terminal_hello_must_match_the_kind_of_device() {
    let harness = Harness::start("hello-kind");
    let mut computer = logged_in(&harness, DeviceKind::Desktop);
    computer.send_control(&Phone::hello("desktop"));
    let host = harness.host();
    assert!(matches!(read_client_msg(&host), Some(ClientMsg::Hello { client: ClientKind::Desktop, .. })));

    let mut computer = logged_in(&harness, DeviceKind::Desktop);
    computer.send_control(&Phone::hello("mobile"));
    assert!(computer.closed());
    let mut phone = logged_in(&harness, DeviceKind::Mobile);
    phone.send_control(&Phone::hello("desktop"));
    assert!(phone.closed());
    // 两条被断开的都没接到宿主上。
    assert!(harness.hosts.try_recv().is_err());
}

#[test]
fn phones_cannot_open_channels() {
    let harness = Harness::start("phone-channel");
    for hello in [ChannelHello::ProjectHello { build: PROJECT_BUILD.into() }, ChannelHello::ForwardHello { port: 1 }] {
        let mut phone = logged_in(&harness, DeviceKind::Mobile);
        send_channel(&mut phone, &hello);
        let refused = ChannelReply::ChannelRefused { reason: ChannelRefusal::NotAllowed };
        assert_eq!(phone.read_control::<ChannelReply>(), Some(refused));
        assert!(phone.closed());
    }
    assert!(harness.projects.try_recv().is_err());
}

#[test]
fn a_project_channel_goes_to_the_project_service_when_the_builds_match() {
    let harness = Harness::start("project");
    let mut computer = logged_in(&harness, DeviceKind::Desktop);
    send_channel(&mut computer, &ChannelHello::ProjectHello { build: "别的构建".into() });
    let mismatch = ChannelRefusal::BuildMismatch { build: PROJECT_BUILD.into() };
    assert_eq!(computer.read_control::<ChannelReply>(), Some(ChannelReply::ChannelRefused { reason: mismatch }));
    assert!(computer.closed());

    let mut computer = logged_in(&harness, DeviceKind::Desktop);
    send_channel(&mut computer, &ChannelHello::ProjectHello { build: PROJECT_BUILD.into() });
    assert_eq!(computer.read_control::<ChannelReply>(), Some(ChannelReply::ChannelReady));
    let service = harness.projects.recv_timeout(PATIENCE).expect("no connection to the project service");
    service.set_read_timeout(Some(PATIENCE)).unwrap();
    // 之后不分帧，两边原样搬字节；`ProjectHello` 本身不转给项目服务。
    computer.tls.write_all(b"{\"id\":1}\n").unwrap();
    computer.tls.flush().unwrap();
    let mut got = [0u8; 9];
    (&service).read_exact(&mut got).unwrap();
    assert_eq!(&got, b"{\"id\":1}\n");
    (&service).write_all(b"ok\n").unwrap();
    let mut back = [0u8; 3];
    computer.tls.read_exact(&mut back).unwrap();
    assert_eq!(&back, b"ok\n");
    // 桌面断开，项目服务那头读到结尾。
    drop(computer);
    assert_eq!((&service).read(&mut got).unwrap(), 0);
}

#[test]
fn a_forward_channel_reaches_a_loopback_port() {
    let harness = Harness::start("forward");
    let echo = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = echo.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = echo.accept().unwrap();
        let mut buf = [0u8; 1024];
        loop {
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => stream.write_all(&buf[..n]).unwrap(),
            }
        }
    });
    let mut computer = logged_in(&harness, DeviceKind::Desktop);
    send_channel(&mut computer, &ChannelHello::ForwardHello { port });
    assert_eq!(computer.read_control::<ChannelReply>(), Some(ChannelReply::ChannelReady));
    // 回声服务器边收边回：一小块一小块地写，写一块读一些，两头的缓冲不会都满。
    let big: Vec<u8> = (0..200_000u32).map(|n| n as u8).collect();
    let mut echoed = Vec::new();
    let mut buf = [0u8; 64 << 10];
    let mut sent = 0;
    while echoed.len() < big.len() {
        if sent < big.len() {
            let end = (sent + (16 << 10)).min(big.len());
            computer.tls.write_all(&big[sent..end]).unwrap();
            computer.tls.flush().unwrap();
            sent = end;
        }
        let n = computer.tls.read(&mut buf).unwrap();
        assert_ne!(n, 0, "closed after {} bytes", echoed.len());
        echoed.extend_from_slice(&buf[..n]);
    }
    assert_eq!(echoed, big);
    // 桌面断开，转发的那条 TCP 跟着关。
    drop(computer);
    server.join().unwrap();

    // 端口上没人在听。
    let closed = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let mut computer = logged_in(&harness, DeviceKind::Desktop);
    send_channel(&mut computer, &ChannelHello::ForwardHello { port });
    let Some(ChannelReply::ChannelRefused { reason: ChannelRefusal::Unavailable { message } }) =
        computer.read_control::<ChannelReply>()
    else {
        panic!("not refused");
    };
    assert!(message.contains(&port.to_string()), "{message}");
    assert!(computer.closed());
}

#[test]
fn only_phones_are_kept_from_changing_the_theme() {
    let harness = Harness::start("theme");
    let theme = serde_json::to_vec(&ClientMsg::SetTheme { settings: Default::default() }).unwrap();
    let mut phone = logged_in(&harness, DeviceKind::Mobile);
    phone.send_control(&Phone::hello("mobile"));
    let host = harness.host();
    assert!(matches!(read_client_msg(&host), Some(ClientMsg::Hello { .. })));
    phone.send_control(&theme);
    assert!(matches!(phone.read_host_msg(), HostMsg::Error { message, .. } if message.contains("theme")));
    phone.send_control(br#"{"type":"list_sessions"}"#);
    assert_eq!(read_client_msg(&host), Some(ClientMsg::ListSessions));

    let mut computer = logged_in(&harness, DeviceKind::Desktop);
    computer.send_control(&Phone::hello("desktop"));
    let host = harness.host();
    assert!(matches!(read_client_msg(&host), Some(ClientMsg::Hello { .. })));
    computer.send_control(&theme);
    assert!(matches!(read_client_msg(&host), Some(ClientMsg::SetTheme { .. })));
}
