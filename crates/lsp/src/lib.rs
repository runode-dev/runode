//! 语言服务器（LSP）的客户端，不碰界面：在调用方给的一对字节流上说 JSON-RPC（`Client`），做握手和文档同步；
//! 进程怎么起由调用方定，本机上是子进程的 stdin、stdout。按文件挑语言服务器的表在 `servers`，服务器只按
//! 程序名起，没装就没有这些功能。文件路径和 URI 的换算在 `uri`。

mod client;
pub mod servers;
mod transport;
pub mod uri;

pub use client::{Client, Error};
pub use lsp_types as types;
