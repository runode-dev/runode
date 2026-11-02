//! 桌面 app 经 ssh 看这台电脑时，远端跑 `runode --ssh-screen`（`run`），把 ssh 的 stdin、stdout 原样接到采集
//! 进程的 socket 上，两头说的都是 `runode_protocol::screen`。

use std::io::{self, Write};

use runode_protocol::{Frame, FrameKind, screen::ScreenHostMsg, write_frame};

use super::client::connect;

/// `runode --ssh-screen`：把 stdin、stdout 接到采集进程上，任一头断开就退出，返回退出码。连不上时往
/// stdout 写一条 `ScreenHostMsg::Stopped` 带上原因再退出，观看端照常显示。
pub fn run() -> i32 {
    // stdout 是协议，出了问题只能看日志。
    crate::logging::init_file(runode_paths::Dirs::from_env().ssh_screen_log_file());
    let stream = match connect() {
        Ok(stream) => stream,
        Err(err) => {
            tracing::warn!("cannot reach the screen process: {err}");
            let reason = format!("cannot start remote desktop on this computer: {err}");
            if let Ok(frame) = Frame::control(&ScreenHostMsg::Stopped {
                reason,
                signed_out: err.kind() == io::ErrorKind::NotConnected,
                no_encoder: false,
            }) {
                let mut out = io::stdout().lock();
                let _ = write_frame(&mut out, FrameKind::Control, 0, &frame.payload);
                let _ = out.flush();
            }
            return 1;
        }
    };
    crate::host_client::bridge::pump_stdio(stream, "screen-bridge-in", 256 * 1024);
    0
}
