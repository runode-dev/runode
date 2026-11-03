//! 会话的尺寸怎么定。
//!
//! 尺寸归属：几个前端带着屏幕看同一个会话时，会话的尺寸由最近交互过的那个（owner）决定，见
//! `Runner::owner`。只有带着屏幕连着、请求过尺寸的连接（`Viewer`）有资格：只看状态的（命令行都是）
//! 和从不说尺寸的（比如手机跟着 Mac 的尺寸看）没有，它们的输入和焦点不算交互，免得抢走 owner 却
//! 没人改尺寸、桌面被裁切着画。交互是带尺寸的 `Inbox::Subscribe`、这条连接的 `Inbox::Input` 和
//! `Inbox::Focus`；命令行的 `Inbox::Keys`、`Inbox::Paste` 不算。owner 的 `Inbox::Resize` 当场应用，
//! 别的只记下，等它当上 owner 再用；还没有 owner 时谁的都应用。尺寸只在宿主插进输出流的
//! `HostMsg::Resized` 处改，两份 VT 不会分叉；owner 换了用 `HostMsg::SizeOwner` 告诉前端。owner 是
//! 连接级的状态，交接后新宿主上的会话从没有 owner 开始。

use runode_protocol::HostMsg;
use runode_types::grid::GridSize;

use super::{Event, Runner};

/// 带着屏幕连着会话的一条连接在尺寸归属里的状态。从这条连接要屏幕的
/// `Subscribe` 起，到它 `Detach`（含连接断开）或者改成只看状态为止。订阅因为读得太慢被丢掉
/// （`Resync`）、还没重新连上时也留着：重新连上不带尺寸，沿用这里记着的。
pub(super) struct Viewer {
    pub(super) connection: u64,
    /// 连接在 `Hello` 里报的设备名。
    pub(super) device: Option<String>,
    /// 这个前端最近一次请求的尺寸（带尺寸的 `Subscribe` 或者 `Inbox::Resize`），轮到它当 owner 时
    /// 应用；还没请求过时为空，这时没资格当 owner。
    pub(super) requested: Option<GridSize>,
    /// 最近一次交互的序号（`Runner::interactions`），没交互过为 0。
    pub(super) last_active: u64,
}

impl Runner {
    /// 连接 `connection` 的视图要尺寸 `size`：是 `Viewer` 的记下来；它是 owner、或者还没有 owner
    /// 时当场应用。
    pub(super) fn request_size(&mut self, connection: u64, size: GridSize) {
        if let Some(viewer) = self.viewers.iter_mut().find(|v| v.connection == connection) {
            viewer.requested = Some(size);
        }
        if self.owner.is_none_or(|owner| owner == connection) {
            self.apply_size(size);
        }
    }

    /// 连接 `connection` 交互了一次：是请求过尺寸的 `Viewer` 的话轮到它决定尺寸。没资格的（只看
    /// 状态、命令行，以及带着屏幕却从没请求过尺寸的）什么都不做，见模块文档。
    pub(super) fn interact(&mut self, connection: u64) {
        let Some(viewer) = self.viewers.iter_mut().find(|v| v.connection == connection && v.requested.is_some()) else {
            return;
        };
        self.interactions += 1;
        viewer.last_active = self.interactions;
        if self.owner != Some(connection) {
            self.make_owner(connection);
        }
    }

    /// 连接 `connection` 不再带着屏幕连着（断开、`Detach`、改成只看状态）：不再有资格；它是 owner
    /// 的话交给剩下的、请求过尺寸的里面最近交互过的，应用那边的尺寸；没有这样的就没有 owner。
    pub(super) fn forget_viewer(&mut self, connection: u64) {
        self.viewers.retain(|v| v.connection != connection);
        if self.owner != Some(connection) {
            return;
        }
        self.owner = None;
        let next =
            self.viewers.iter().filter(|v| v.requested.is_some()).max_by_key(|v| v.last_active).map(|v| v.connection);
        if let Some(next) = next {
            self.make_owner(next);
        }
    }

    /// `connection`（`viewers` 里的）当上 owner：应用它最近请求的尺寸，告诉带着屏幕连着的前端。
    fn make_owner(&mut self, connection: u64) {
        self.owner = Some(connection);
        let requested = self.viewers.iter().find(|v| v.connection == connection).and_then(|v| v.requested);
        if let Some(size) = requested {
            self.apply_size(size);
        }
        let device = self.device_of(connection);
        let (id, viewers) = (self.id, &self.viewers);
        self.subscribers.retain_mut(|subscriber| {
            if !viewers.iter().any(|v| v.connection == subscriber.connection) {
                return true;
            }
            let mine = subscriber.connection == connection;
            (subscriber.sink)(Event::msg(HostMsg::SizeOwner { id, mine, owner: device.clone() }))
        });
    }

    /// 改会话的尺寸，改了的话在输出流里标出 `Resized`，两份 VT 都在这里改。
    fn apply_size(&mut self, size: GridSize) {
        if self.session.resize(size) {
            self.emit(Event::msg(HostMsg::Resized { id: self.id, size }));
        }
    }

    /// 给连接 `connection` 的当前 `SizeOwner`：它带着屏幕连着、也有 owner 时才有。
    pub(super) fn size_owner_for(&self, connection: u64) -> Option<HostMsg> {
        let owner = self.owner?;
        self.viewers.iter().any(|v| v.connection == connection).then(|| HostMsg::SizeOwner {
            id: self.id,
            mine: owner == connection,
            owner: self.device_of(owner),
        })
    }

    /// 连接 `connection` 在 `Hello` 里报的设备名。
    pub(super) fn device_of(&self, connection: u64) -> Option<String> {
        self.viewers.iter().find(|v| v.connection == connection).and_then(|v| v.device.clone())
    }
}
