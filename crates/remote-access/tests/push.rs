//! 手机经远程访问登记推送：`PushRegister` 记进设备表（token 为空时删掉），回 `Done`，不转给宿主；撤销
//! 设备时它的登记一起删。

mod common;

use common::*;
use runode_protocol::{ClientMsg, HostMsg, push::ApnsEnv, remote::DeviceId};
use runode_remote_access::{PushRegistration, push_registrations, revoke_device};

const TOKEN: &str = "80f0c8b3a4e2d1c0ffeeddccbbaa99887766554433221100aabbccddeeff0011";

fn register(req: u32, token: Option<&str>) -> Vec<u8> {
    serde_json::to_vec(&ClientMsg::PushRegister {
        req,
        token: token.map(Into::into),
        env: ApnsEnv::Development,
        bundle: "cn.barey.runode".into(),
        machine: "6F9619FF-8B86-D011-B42D-00C04FC964FF".into(),
        machine_name: "Ethan 的 MacBook Pro".into(),
    })
    .unwrap()
}

/// 宿主那一端接下来收到的是手机随后发的 `ListSessions`：前面登记推送的请求没转过去。
fn nothing_else_reached_the_host(phone: &mut Phone, host: &std::os::unix::net::UnixStream) {
    phone.send_control(br#"{"type":"list_sessions"}"#);
    assert_eq!(read_client_msg(host), Some(ClientMsg::ListSessions));
}

fn registration(harness: &Harness, device_id: DeviceId) -> Option<PushRegistration> {
    push_registrations(&harness.dirs).unwrap().into_iter().find(|push| push.device_id == device_id)
}

#[test]
fn registering_writes_the_table_and_unregistering_clears_it() {
    let harness = Harness::start("pushreg");
    let key = Key::generate();
    let device_id = harness.pair(&key);
    let (mut phone, host) = harness.bridged(device_id, &key);

    phone.send_control(&register(1, Some(TOKEN)));
    assert_eq!(phone.read_host_msg(), HostMsg::Done { req: 1 });
    let push = registration(&harness, device_id).expect("registered");
    assert_eq!(
        (push.token.as_str(), push.env, push.bundle.as_str(), push.machine_name.as_str()),
        (TOKEN, ApnsEnv::Development, "cn.barey.runode", "Ethan 的 MacBook Pro")
    );
    assert_eq!(push.machine, "6F9619FF-8B86-D011-B42D-00C04FC964FF");
    // 再登记一次换掉原来的，一台设备只有一条。
    phone.send_control(&register(2, Some("abcd")));
    assert_eq!(phone.read_host_msg(), HostMsg::Done { req: 2 });
    assert_eq!(push_registrations(&harness.dirs).unwrap().len(), 1);
    assert_eq!(registration(&harness, device_id).unwrap().token, "abcd");

    phone.send_control(&register(3, None));
    assert_eq!(phone.read_host_msg(), HostMsg::Done { req: 3 });
    assert_eq!(registration(&harness, device_id), None);
    // 本来就没有时照样回 `Done`。
    phone.send_control(&register(4, None));
    assert_eq!(phone.read_host_msg(), HostMsg::Done { req: 4 });

    nothing_else_reached_the_host(&mut phone, &host);
}

/// token 不是十六进制、环境不认识时回带 `req` 的 `Error`，表里不记，连接照旧。
#[test]
fn bad_registrations_are_refused() {
    let harness = Harness::start("pushbad");
    let key = Key::generate();
    let device_id = harness.pair(&key);
    let (mut phone, host) = harness.bridged(device_id, &key);

    phone.send_control(&register(1, Some("not hex")));
    assert!(matches!(phone.read_host_msg(), HostMsg::Error { req: Some(1), id: None, .. }));
    let newer_env = format!(
        r#"{{"type":"push_register","req":2,"token":"{TOKEN}","env":"sandbox_2","bundle":"b","machine":"m","machine_name":"n"}}"#
    );
    phone.send_control(newer_env.as_bytes());
    assert!(matches!(phone.read_host_msg(), HostMsg::Error { req: Some(2), id: None, .. }));
    assert!(push_registrations(&harness.dirs).unwrap().is_empty());

    nothing_else_reached_the_host(&mut phone, &host);
}

/// 撤销设备时它登记的推送一起删，别的设备的留着。
#[test]
fn revoking_a_device_removes_its_registration() {
    let harness = Harness::start("pushrev");
    let (key, other_key) = (Key::generate(), Key::generate());
    let device_id = harness.pair(&key);
    let other = harness.pair(&other_key);
    for (id, key) in [(device_id, &key), (other, &other_key)] {
        let (mut phone, _host) = harness.bridged(id, key);
        phone.send_control(&register(1, Some(TOKEN)));
        assert_eq!(phone.read_host_msg(), HostMsg::Done { req: 1 });
    }
    assert_eq!(push_registrations(&harness.dirs).unwrap().len(), 2);

    assert!(revoke_device(&harness.dirs, device_id).unwrap());
    assert_eq!(registration(&harness, device_id), None);
    assert!(registration(&harness, other).is_some());
}
