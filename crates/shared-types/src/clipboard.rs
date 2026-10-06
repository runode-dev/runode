//! 终端里的程序用 OSC 52 读写系统剪贴板时守的规矩：配置项 `clipboard-write`、`clipboard-read`
//! 的取值和一次读写的大小上限。宿主按它决定转不转给桌面、问不问用户，桌面按它截住过大的内容。

use serde::{Deserialize, Serialize};

/// 一次读写剪贴板的文字最多这么多字节（解码后）。再大的写请求整条丢掉，读请求回空；终端里的
/// 程序往剪贴板放上几 MB 多半是出了错，也免得一次把这么多字节塞进 PTY。
pub const MAX_CLIPBOARD_BYTES: usize = 1 << 20;

/// 程序写剪贴板时怎么办，配置项 `clipboard-write`。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardWrite {
    /// 照写，不问。
    #[default]
    Allow,
    /// 丢掉，程序收不到任何回应。
    Deny,
}

/// 程序读剪贴板时怎么办，配置项 `clipboard-read`。读到的是用户复制过的任何东西（密码也在内），
/// 所以默认先问。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardRead {
    /// 照读，不问。
    Allow,
    /// 先弹框问用户。
    #[default]
    Ask,
    /// 不读，回给程序一个空的剪贴板。
    Deny,
}

/// 读写剪贴板的规矩，一起交给宿主。缺的项按默认值读。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipboardAccess {
    pub write: ClipboardWrite,
    pub read: ClipboardRead,
}
