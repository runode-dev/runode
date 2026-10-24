//! 看板（见 `runode_types::todo`）的库和定时：任务和定时存在一个 SQLite 文件里（`Store`），定时
//! 按本地时间算下一次到点（`next_fire`）。
//!
//! 宿主拿着库开任务、按定时开新任务，前端经宿主的 `ClientMsg::ListTodos` 这几条消息读写看板。别的程序
//! 也可以直接开库读写，SQLite 管得住几个进程同时开着；但宿主不知道库被改了，不会告诉在看看板的前端，
//! 定时的线程也要到下一轮（最多半分钟）才看到，所以宿主在跑时改看板仍经它的消息。

mod schedule;
mod store;

pub use schedule::{local_tm, mktime, next_fire};
pub use store::{DONE_KEPT, Result, Store};
