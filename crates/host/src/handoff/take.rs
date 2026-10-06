//! 接手会话的一方（新宿主），入口是 `Host::take_over`。

use std::path::Path;

use super::{TakeOverError, TakeOverOptions, TakeOverReport};
use crate::Host;

impl Host {
    /// 从 `socket` 上单独跑着的旧宿主手里接过所有会话和监听的 socket。
    pub fn take_over(&self, socket: &Path, options: TakeOverOptions) -> Result<TakeOverReport, TakeOverError> {
        let _ = (socket, options);
        Err(TakeOverError::Failed("not implemented yet".into()))
    }
}
