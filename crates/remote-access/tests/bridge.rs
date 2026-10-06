//! 门禁之后的桥接：宿主挂断时把它最后发的都送到手机，挡下手机的宿主级请求、在两帧之间回 `Error`。

mod common;

use std::{
    io::Write as _,
    thread,
    time::{Duration, Instant},
};

use common::*;
use runode_protocol::{ClientMsg, FrameKind, GoodbyeReason, HostMsg, read_frame, write_frame};

fn control(message: &HostMsg) -> Vec<u8> {
    serde_json::to_vec(message).unwrap()
}

/// 手机读得慢、输出积压着时，宿主发完 `Goodbye` 就关了连接，手机这时又发来一帧：宿主那边写不进去了，
/// 但它最后发的输出和 `Goodbye` 照样全部送到，然后连接正常关闭。
#[test]
fn the_host_hanging_up_with_a_backlog_loses_nothing() {
    let harness = Harness::start("backlog");
    let key = Key::generate();
    let device_id = harness.pair(&key);
    let (mut phone, host) = harness.bridged(device_id, &key);
    const FRAMES: usize = 12;
    let writer = thread::spawn(move || {
        let chunk = vec![b'x'; 60_000];
        for _ in 0..FRAMES {
            write_frame(&mut &host, FrameKind::Output, 1, &chunk).unwrap();
        }
        let bye = HostMsg::Goodbye { reason: GoodbyeReason::Shutdown };
        write_frame(&mut &host, FrameKind::Control, 0, &control(&bye)).unwrap();
    });
    writer.join().unwrap();
    // 宿主那一端已经关了；等桥接那边看到，再从手机发一帧，写给宿主会出错。
    thread::sleep(Duration::from_millis(300));
    phone.send_control(br#"{"type":"list_sessions"}"#);
    thread::sleep(Duration::from_millis(300));
    let mut outputs = 0;
    let mut goodbye = false;
    while let Ok(Some(frame)) = read_frame(&mut phone.tls) {
        match frame.kind {
            FrameKind::Output => {
                assert!(!goodbye, "output after goodbye");
                assert_eq!(frame.payload.len(), 60_000);
                outputs += 1;
            }
            _ => goodbye = matches!(frame.message::<HostMsg>(), Ok(HostMsg::Goodbye { .. })),
        }
    }
    assert_eq!((outputs, goodbye), (FRAMES, true));
}

/// 手机发来管宿主本身的请求（`Shutdown` 等）：不转给宿主，手机收到一条 `Error`，插在宿主的两帧之间，
/// 宿主正在发的大帧不受影响；连接照旧，之后的请求照常转过去。
#[test]
fn host_level_requests_from_the_phone_are_refused_between_frames() {
    let harness = Harness::start("refuse");
    let key = Key::generate();
    let device_id = harness.pair(&key);
    let (mut phone, host) = harness.bridged(device_id, &key);
    let writer = {
        let host = host.try_clone().unwrap();
        thread::spawn(move || {
            for i in 0..40u8 {
                write_frame(&mut &host, FrameKind::Output, 1, &vec![i; 50_000]).unwrap();
            }
        })
    };
    for refused in [
        r#"{"type":"shutdown","kill_sessions":true}"#,
        r#"{"type":"handoff","min_format":1,"max_format":1}"#,
        r#"{"type":"handoff_done"}"#,
        r#"{"type":"set_options","record_history":false}"#,
    ] {
        phone.send_control(refused.as_bytes());
    }
    phone.send_control(br#"{"type":"list_sessions"}"#);
    write_frame(&mut phone.tls, FrameKind::Input, 1, b"ls\r").unwrap();
    phone.tls.flush().unwrap();
    // 宿主只收到放行的那两帧。
    assert_eq!(read_client_msg(&host), Some(ClientMsg::ListSessions));
    let input = read_frame(&mut &host).unwrap().unwrap();
    assert_eq!((input.kind, input.channel, input.payload.as_slice()), (FrameKind::Input, 1, &b"ls\r"[..]));
    // 手机得边读，宿主那边 2 MB 的输出才写得完；写完后宿主再发一条 `Done` 收尾。
    let finisher = thread::spawn(move || {
        writer.join().unwrap();
        write_frame(&mut &host, FrameKind::Control, 0, &control(&HostMsg::Done { req: 9 })).unwrap();
        host
    });
    let (mut outputs, mut errors) = (0u8, 0);
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        let frame = read_frame(&mut phone.tls).unwrap().unwrap();
        match frame.kind {
            FrameKind::Output => {
                // 帧没被拆开：每帧都完整、按顺序。
                assert_eq!(frame.payload, vec![outputs; 50_000]);
                outputs += 1;
            }
            _ => match frame.message::<HostMsg>().unwrap() {
                HostMsg::Error { req: None, id: None, message } => {
                    assert!(message.starts_with("a remote device cannot"), "{message}");
                    errors += 1;
                }
                HostMsg::Done { req: 9 } => break,
                other => panic!("{other:?}"),
            },
        }
    }
    assert_eq!((outputs, errors), (40, 4));
    let host = finisher.join().unwrap();
    // 连接还在：照常往来。
    phone.send_control(br#"{"type":"list_sessions"}"#);
    assert_eq!(read_client_msg(&host), Some(ClientMsg::ListSessions));
}

/// 手机说了 close_notify：它最后发的写给宿主，宿主读到结尾、关掉以后，连接正常结束。
#[test]
fn the_phone_closing_lets_the_host_finish() {
    let harness = Harness::start("close");
    let key = Key::generate();
    let device_id = harness.pair(&key);
    let (mut phone, host) = harness.bridged(device_id, &key);
    phone.send_control(br#"{"type":"list_sessions"}"#);
    phone.tls.conn.send_close_notify();
    phone.tls.flush().unwrap();
    assert_eq!(read_client_msg(&host), Some(ClientMsg::ListSessions));
    // 宿主读到结尾。
    assert_eq!(read_client_msg(&host), None);
    let bye = HostMsg::Goodbye { reason: GoodbyeReason::Shutdown };
    let _ = write_frame(&mut &host, FrameKind::Control, 0, &control(&bye));
    drop(host);
    assert!(phone.closed());
}
