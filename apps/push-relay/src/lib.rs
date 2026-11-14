//! runode 官方 App 的推送中转服务。手机拿到推送的 token（iPhone 是 APNs 的 device token，Android 是 FCM 的
//! registration token）后为每台电脑换一个句柄（`/v1/register`），电脑只拿得到句柄，推送时交回来
//! （`/v1/notify`），中转解开句柄，用 Apple 给的密钥签名发给 APNs，或者用 Firebase 的服务账号发给 FCM。
//! 线上格式以 `runode_protocol::push` 为准。
//!
//! 中转不存状态：句柄就是加密封好的 token（`handle`），限频的计数只在内存里（`limit`）。日志里不记
//! token、句柄和通知内容。

pub mod apns;
pub mod fcm;
pub mod handle;
pub mod limit;
pub mod server;
