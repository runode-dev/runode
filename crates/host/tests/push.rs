//! 登记推送（`ClientMsg::PushRegister`）只有手机经远程访问发，由监听方办；直接连宿主的前端发来时回带
//! `req` 的 `Error`，连接照旧。

mod common;

use common::{Peer, host};
use runode_host::{ClientMsg, HostMsg};
use runode_protocol::{ClientKind, push::ApnsEnv};

#[test]
fn the_host_does_not_take_push_registrations() {
    let host = host();
    let mut phone = Peer::over(host.connect_pair().unwrap()).greet(ClientKind::Mobile, false);
    phone.send(&ClientMsg::PushRegister {
        req: 3,
        token: Some("00ff".into()),
        env: ApnsEnv::Production,
        bundle: "cn.barey.runode".into(),
        machine: "m".into(),
        machine_name: "Mac".into(),
    });
    assert!(matches!(phone.reply(), HostMsg::Error { req: Some(3), id: None, .. }));
    // 连接照旧。
    phone.send(&ClientMsg::ListSessions);
    assert!(matches!(phone.reply(), HostMsg::SessionList { .. }));
}
