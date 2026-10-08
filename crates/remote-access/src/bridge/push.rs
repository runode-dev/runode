//! 手机经远程访问登记推送（`ClientMsg::PushRegister`，见 `runode_protocol::push`）：不转给宿主，在这里
//! 按过了门禁的这台设备办。push-to-start token 记进设备表（`devices::set_push`），手机断开后照样推得到，
//! 推送由 `crate::push` 发。办好回 `Done`，办不了回带 `req` 的 `Error`。

use runode_paths::Dirs;
use runode_protocol::{ClientMsg, HostMsg, push::ApnsEnv, remote::DeviceId};

use super::frames::LocalRequests;
use crate::{
    devices::{self, PushRegistration},
    now_unix,
};

/// token 最多这么多个十六进制数字。APNs 的 token 现在是 32 字节，留足余地。
const MAX_TOKEN_LEN: usize = 512;
/// bundle id、电脑的 UUID 和名字最多这么多字节，免得设备表被撑大。
const MAX_FIELD_LEN: usize = 256;

/// 一条过了门禁的连接上登记推送的请求，见模块文档。
pub(crate) struct PushRequests<'a> {
    pub(crate) dirs: &'a Dirs,
    pub(crate) device: DeviceId,
}

impl LocalRequests for PushRequests<'_> {
    fn handle(&mut self, message: &ClientMsg) -> Option<HostMsg> {
        let ClientMsg::PushRegister { req, token, env, bundle, machine, machine_name } = message else {
            return None;
        };
        Some(match self.register(token.as_deref(), *env, bundle, machine, machine_name) {
            Ok(()) => HostMsg::Done { req: *req },
            Err(message) => HostMsg::Error { req: Some(*req), id: None, message },
        })
    }
}

impl PushRequests<'_> {
    /// 记下（`token` 为空时删掉）这台设备的推送登记。
    fn register(
        &self,
        token: Option<&str>,
        env: ApnsEnv,
        bundle: &str,
        machine: &str,
        machine_name: &str,
    ) -> Result<(), String> {
        let device = self.device;
        let Some(token) = token else {
            devices::clear_push(self.dirs, device).map_err(|err| table_error(&err))?;
            tracing::info!("remote device {device} unregistered from push notifications");
            return Ok(());
        };
        check_token(token)?;
        if env == ApnsEnv::Unknown {
            return Err("unknown APNs environment".into());
        }
        for (name, value) in [("bundle", bundle), ("machine", machine), ("machine_name", machine_name)] {
            if value.is_empty() || value.len() > MAX_FIELD_LEN {
                return Err(format!("{name} must be 1 to {MAX_FIELD_LEN} bytes"));
            }
        }
        let registration = PushRegistration {
            device_id: device,
            token: token.to_owned(),
            env,
            bundle: bundle.to_owned(),
            machine: machine.to_owned(),
            machine_name: machine_name.to_owned(),
            updated_at: now_unix(),
        };
        if !devices::set_push(self.dirs, registration).map_err(|err| table_error(&err))? {
            return Err("this device is no longer paired".into());
        }
        tracing::info!("remote device {device} registered for push notifications ({env:?})");
        Ok(())
    }
}

/// token 是 1 到 `MAX_TOKEN_LEN` 个十六进制数字。
fn check_token(token: &str) -> Result<(), String> {
    if token.is_empty() || token.len() > MAX_TOKEN_LEN || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("a push token is 1 to {MAX_TOKEN_LEN} hexadecimal digits"));
    }
    Ok(())
}

/// 读写设备表出错：记日志，回给手机一句。
fn table_error(err: &std::io::Error) -> String {
    tracing::warn!("cannot update push registrations in the paired device table: {err}");
    format!("cannot update the paired device table: {err}")
}
