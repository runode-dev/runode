//! 手机解除配对：`Unpair` 由监听方办，把这台设备从设备表里撤掉（推送登记一起）、回 `Done`，不转给宿主，
//! 随后断开；别的设备不受影响。

mod common;

use common::*;
use runode_protocol::{
    ClientMsg, HostMsg,
    remote::{GateHostMsg, RejectReason},
};
use runode_remote::list_devices;

#[test]
fn unpairing_removes_the_device_and_disconnects_it() {
    let harness = Harness::start("unpair");
    let key = Key::generate();
    let device_id = harness.pair(&key);
    let other = harness.pair(&Key::generate());
    let (mut phone, host) = harness.bridged(device_id, &key);

    phone.send_control(&serde_json::to_vec(&ClientMsg::Unpair { req: 7 }).unwrap());
    assert_eq!(phone.read_host_msg(), HostMsg::Done { req: 7 });
    let left: Vec<_> = list_devices(&harness.dirs).unwrap().into_iter().map(|device| device.device_id).collect();
    assert_eq!(left, [other]);
    // 请求没到宿主，连接随后两头都断。
    assert!(phone.closed());
    assert_eq!(read_client_msg(&host), None);
    // 再也登录不了。
    let mut phone = harness.phone();
    let auth = phone.auth_message(device_id, &key);
    assert_eq!(phone.ask(&auth), Some(GateHostMsg::RemoteRejected { reason: RejectReason::UnknownDevice }));
}
