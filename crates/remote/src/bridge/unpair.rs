//! 手机解除配对（`ClientMsg::Unpair`）：不转给宿主，在这里把过了门禁的这台设备从配对设备表里撤掉，
//! 和 `runode remote revoke` 一样；撤掉以后这条连接随后断开。办好回 `Done`，办不了回带 `req` 的 `Error`。

use runode_paths::Dirs;
use runode_protocol::{ClientMsg, HostMsg, remote::DeviceId};

use super::frames::LocalRequests;
use crate::devices;

/// 一条过了门禁的连接上解除配对的请求，见模块文档。
pub(crate) struct UnpairRequests<'a> {
    pub(crate) dirs: &'a Dirs,
    pub(crate) device: DeviceId,
}

impl LocalRequests for UnpairRequests<'_> {
    fn handle(&mut self, message: &ClientMsg) -> Option<HostMsg> {
        let ClientMsg::Unpair { req } = message else { return None };
        let device = self.device;
        Some(match devices::revoke_device(self.dirs, device) {
            Ok(_) => {
                tracing::info!("remote device {device} unpaired itself");
                HostMsg::Done { req: *req }
            }
            Err(err) => HostMsg::Error {
                req: Some(*req),
                id: None,
                message: format!("cannot update the device table: {err}"),
                reason: None,
            },
        })
    }
}
