//! 帧：`u32 LE 载荷长度 | u8 类型 | u32 LE 通道 | 载荷`。
//!
//! 通道是会话在这条连接上的编号，宿主在 `HostMsg::Attached` 里给出，比 128 位的 `SessionId`
//! 短，每一帧 PTY 输出都带着它。0 留给不属于哪个会话的帧，控制消息一律走 0，消息里自己带着
//! `SessionId`。

use std::io::{self, Read, Write};

/// 帧头的字节数：载荷长度、类型、通道。
pub const HEADER_LEN: usize = 9;

/// 一帧载荷的上限。默认的回滚上限（每个终端 10 MiB、1 万行）下，回滚历史填满时 200 列的
/// 快照约 5 MiB，一帧放得下；用户把配置项 `scrollback-limit` 调大以后快照可能超过这个上限，
/// 要分成几帧发。再长的帧就是对面出了错或者不怀好意。
pub const MAX_PAYLOAD: u32 = 64 << 20;

/// 帧的类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FrameKind {
    /// 宿主发给前端的 PTY 输出，原样的字节；只有 shell 集成报告给宿主的 OSC 6973（带着 shell
    /// 的口令）抹掉了编号之后的内容，VT 处理起来和原来那条一样，都是不认识的 OSC。
    Output = 0,
    /// 前端发给宿主、要写进 PTY 的输入，原样的字节。
    Input = 1,
    /// 控制消息，JSON，见 `ClientMsg`、`HostMsg`。
    Control = 2,
    /// 快照或 VT 重放的一段，见 `AttachMode`；一份可以分成几帧，以 `HostMsg::SnapshotEnd` 结束。
    Snapshot = 3,
}

impl FrameKind {
    fn from_u8(kind: u8) -> Option<Self> {
        Some(match kind {
            0 => Self::Output,
            1 => Self::Input,
            2 => Self::Control,
            3 => Self::Snapshot,
            _ => return None,
        })
    }
}

/// 读到的一帧。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub kind: FrameKind,
    pub channel: u32,
    pub payload: Vec<u8>,
}

impl Frame {
    /// 一条控制消息，走通道 0。
    pub fn control(message: &impl serde::Serialize) -> serde_json::Result<Self> {
        Ok(Self { kind: FrameKind::Control, channel: 0, payload: serde_json::to_vec(message)? })
    }

    /// 把控制帧的载荷解成消息。
    pub fn message<T: serde::de::DeserializeOwned>(&self) -> serde_json::Result<T> {
        serde_json::from_slice(&self.payload)
    }
}

/// 读写帧失败的原因。
#[derive(Debug)]
pub enum FrameError {
    Io(io::Error),
    /// 载荷超过 `MAX_PAYLOAD`（`read_frame_limited` 时是给的上限）；读的时候是对面声明的长度。
    TooLong(u64),
    /// 不认识的帧类型，对面多半是别的协议版本。
    UnknownKind(u8),
    /// 一帧读到一半连接就断了。
    Truncated,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "frame i/o failed: {err}"),
            Self::TooLong(len) => write!(f, "frame payload of {len} bytes is over the limit"),
            Self::UnknownKind(kind) => write!(f, "unknown frame kind {kind}"),
            Self::Truncated => write!(f, "connection closed in the middle of a frame"),
        }
    }
}

impl std::error::Error for FrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for FrameError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

/// 读一帧。连接在两帧之间正常关闭时返回 `None`；帧读到一半断了、声明的载荷超长或者类型
/// 不认识时报错，这时连接上的数据已经对不齐了，只能断开。
pub fn read_frame(reader: &mut impl Read) -> Result<Option<Frame>, FrameError> {
    read_frame_limited(reader, MAX_PAYLOAD)
}

/// 同 `read_frame`，但载荷超过 `limit` 就报 `FrameError::TooLong`（`limit` 比 `MAX_PAYLOAD` 大时
/// 按 `MAX_PAYLOAD`）。对面还没证明自己是谁时用小的上限，见 `remote::GATE_MAX_PAYLOAD`。
pub fn read_frame_limited(reader: &mut impl Read, limit: u32) -> Result<Option<Frame>, FrameError> {
    let mut header = [0u8; HEADER_LEN];
    if !read_full(reader, &mut header)? {
        return Ok(None);
    }
    let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    if len > limit.min(MAX_PAYLOAD) {
        return Err(FrameError::TooLong(u64::from(len)));
    }
    let kind = FrameKind::from_u8(header[4]).ok_or(FrameError::UnknownKind(header[4]))?;
    let channel = u32::from_le_bytes([header[5], header[6], header[7], header[8]]);
    // 缓冲按实际到达的字节增长，不照着对面声明的长度一上来就分配。
    let mut payload = Vec::new();
    reader.take(u64::from(len)).read_to_end(&mut payload)?;
    if payload.len() != len as usize {
        return Err(FrameError::Truncated);
    }
    Ok(Some(Frame { kind, channel, payload }))
}

/// 写一帧，帧头和载荷一起写出去。载荷超过 `MAX_PAYLOAD` 时什么都不写，返回错误；快照这类
/// 大块数据由调用方分成几帧。
pub fn write_frame(writer: &mut impl Write, kind: FrameKind, channel: u32, payload: &[u8]) -> Result<(), FrameError> {
    let len = u32::try_from(payload.len())
        .ok()
        .filter(|&len| len <= MAX_PAYLOAD)
        .ok_or(FrameError::TooLong(payload.len() as u64))?;
    let mut header = [0u8; HEADER_LEN];
    header[..4].copy_from_slice(&len.to_le_bytes());
    header[4] = kind as u8;
    header[5..].copy_from_slice(&channel.to_le_bytes());
    writer.write_all(&header)?;
    writer.write_all(payload)?;
    Ok(())
}

/// 读满帧头。一个字节都没读到就到了结尾时返回 `false`；读到一部分就到了结尾算截断。
fn read_full(reader: &mut impl Read, buf: &mut [u8]) -> Result<bool, FrameError> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) if filled == 0 => return Ok(false),
            Ok(0) => return Err(FrameError::Truncated),
            Ok(n) => filled += n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err.into()),
        }
    }
    Ok(true)
}
