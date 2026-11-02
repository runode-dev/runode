//! 别的电脑上的 Runode 配对成桌面设备：设备表记下种类。

mod common;

use common::*;
use runode_protocol::remote::{DeviceKind, GateHostMsg};
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
