//! 终端里的程序读写系统剪贴板（OSC 52）：宿主那份 VT 认出请求，整理成 `ClipboardRequest` 交给
//! 宿主，宿主按配置转给桌面去办。界面那份 VT（`Session`）不注册这些，同一条序列不会办两次。
//!
//! 写（`ESC ] 52 ; <目标> ; <base64> BEL` 或以 ST 结尾）由 libghostty 认：序列分几块到达、两种
//! 终止符、base64 解码（不是合法 base64 的整条丢掉）都和 VT 自己的解析一致，解好的内容经
//! `on_clipboard_write` 交到 `take_write`。
//!
//! 读（载荷是 `?`）libghostty 只有同步的回调：回调返回之前就要给出剪贴板的内容，不然它当场回给
//! 程序一个空的剪贴板；可宿主要等桌面去读，问用户时还要等用户点。装上读的回调还会让 VT 对外说
//! 自己支持粘贴事件（mode 5522），之后粘贴改成先发事件、等程序回来读剪贴板，宿主那时同样给不出。
//! 所以读的回调不装（没有它时 VT 不回应 OSC 52 的读），由 `QueryScanner` 在喂给 VT 的字节流旁边
//! 认出读请求，宿主拿到结果后按请求的终止符自己回话（`ClipboardQuery::answer`）。

use libghostty_vt::terminal::{ClipboardWrite, ClipboardWriteError};
use runode_shared_types::clipboard::MAX_CLIPBOARD_BYTES;

use super::effects::Effects;

/// 宿主那份 VT 认出的一个剪贴板请求，由 `HostSession::take_clipboard` 按到达的先后取走。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardRequest {
    /// 程序要把这段文字写进剪贴板，已经解码、不超过 `MAX_CLIPBOARD_BYTES`。
    Write(String),
    /// 程序要读剪贴板，宿主用 `HostSession::answer_clipboard` 回话。
    Read(ClipboardQuery),
}

/// 一个读剪贴板的请求：回话时写回的目标和终止符跟着请求走。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipboardQuery {
    /// 回话里的目标：`s`、`p` 原样，别的（含空目标）都是 `c`，和 VT 归类目标的方式一样。
    target: u8,
    /// 请求以 BEL 结尾；否则以 ST（`ESC \`）结尾。回话用同样的终止符，程序才认得出。
    bel: bool,
}

impl ClipboardQuery {
    /// 回给程序的序列：`ESC ] 52 ; <目标> ; <text 的 base64>`，按请求的终止符结尾。`text` 为空时
    /// 程序读到的是空的剪贴板。
    pub fn answer(&self, text: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(text.len().div_ceil(3) * 4 + 9);
        out.extend_from_slice(b"\x1b]52;");
        out.push(self.target);
        out.push(b';');
        encode_base64(text.as_bytes(), &mut out);
        out.extend_from_slice(if self.bel { b"\x07" } else { b"\x1b\\" });
        out
    }
}

/// `on_clipboard_write` 的回调：取出文字，记进 `effects.clipboard`。
///
/// 目标不看：macOS 只有一个系统剪贴板，没有 selection 和 primary，`s`、`p`、空目标（惯例是
/// `s 0`）和别的目标一律当作系统剪贴板。VT 只认一个字符的目标，`sc` 这种写了几个目标的整条
/// 不办。
///
/// 回给程序的应答（只有带应答的剪贴板协议用得上，OSC 52 没有应答）在这里就给：不让写
/// （`Effects::clipboard_writes` 关着）时是「不允许」，收下了算成功；收下的转给谁由宿主定，VT 这一层
/// 等不到结果。
pub(super) fn take_write(write: ClipboardWrite<'_>, effects: &Effects) {
    let result = if !effects.clipboard_writes.get() {
        tracing::debug!("denied a clipboard write: clipboard-write = deny");
        Err(ClipboardWriteError::Denied)
    } else if write.contents().len() == 0 {
        // 空的载荷是要清空剪贴板。清掉只会弄丢用户自己复制的东西，程序也看不到清没清，不办。
        tracing::debug!("ignored a request to clear the clipboard");
        Err(ClipboardWriteError::Unsupported)
    } else {
        match write.contents().find(|content| is_plain_text(content.mime)) {
            None => {
                tracing::debug!("ignored a clipboard write without plain text");
                Err(ClipboardWriteError::Unsupported)
            }
            // 不记内容本身，只记长度。
            Some(content) if content.data.len() > MAX_CLIPBOARD_BYTES => {
                tracing::warn!("dropped a clipboard write of {} bytes, over the limit", content.data.len());
                Err(ClipboardWriteError::InvalidData)
            }
            Some(content) => match effects.clipboard.try_borrow_mut() {
                // 剪贴板放的是文字，系统剪贴板只收合法的 Unicode：不是 UTF-8 的字节换成 U+FFFD，其余
                // 照写，比整条丢掉更接近程序的本意。
                Ok(mut requests) => {
                    requests.push(ClipboardRequest::Write(String::from_utf8_lossy(content.data).into_owned()));
                    Ok(())
                }
                // 回调运行在 extern "C" 函数里，不能 panic；借不到时丢掉这一条。
                Err(_) => Err(ClipboardWriteError::Busy),
            },
        }
    };
    write.reply(result, false);
}

fn is_plain_text(mime: &str) -> bool {
    mime == "text/plain" || mime.starts_with("text/plain;")
}

/// 读请求 OSC 的内容最长是 `52;c;?`，再长就不是读请求了。
const QUERY_MAX: usize = 6;

const BEL: u8 = 0x07;
const ESC: u8 = 0x1b;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

/// 在喂给宿主 VT 的字节流旁边认 OSC 52 的读请求（`ESC ] 52 ; <目标> ; ? BEL`，或以 ST 结尾），
/// 跨块接着认。按 VT 的状态转移走，只在 VT 也把这些字节当成同一条 OSC 时才认：
///
/// - ESC 在任何状态下都开始一条转义序列，紧跟着 `]` 时是 OSC；中间夹着别的字节（`ESC ( ]`）的不是。
/// - OSC 里 BEL 和 ESC 结束它（ESC 之后的 `\` 是 ST 的另一半，又是一条新的转义序列的开头），
///   CAN、SUB 取消它，别的控制字符跳过不算内容，0x20 起的字节（含 UTF-8 的高位字节）都是内容。
/// - 内容按 VT 的写法拆：`52;` 之后是一个字符的目标和 `;`，或者直接是 `;`（空目标），再之后正好
///   是 `?`。
///
/// 8 位的 C1 控制字符（单个字节的 OSC 开头、ST）不认：VT 也只在一条转义序列中间才把它们当控制字符，
/// 程序不会这样发。和 VT 有出入时只会漏认，不会把不是读请求的当成读请求。
#[derive(Default)]
pub(super) struct QueryScanner {
    state: Scan,
    /// 这条 OSC 到目前为止的内容，最多 `QUERY_MAX` 字节。
    body: [u8; QUERY_MAX],
    len: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Scan {
    /// 不在 OSC 里，等 ESC。
    #[default]
    Ground,
    /// 刚过一个 ESC。
    Escape,
    /// 在一条 OSC 里，内容还没超过 `QUERY_MAX`。
    Osc,
    /// 在一条更长的 OSC 里，只等它结束。
    LongOsc,
}

impl QueryScanner {
    /// 接着往后扫 `data`，扫到一条读请求的结尾（终止它的那个字节）时停下，返回结尾之后的位置和
    /// 请求；扫完都没有时返回 `None`。剩下的字节下次接着扫。
    pub(super) fn scan(&mut self, data: &[u8]) -> Option<(usize, ClipboardQuery)> {
        let mut i = 0;
        while i < data.len() {
            let byte = data[i];
            match self.state {
                Scan::Ground => {
                    i += data[i..].iter().position(|&b| b == ESC)? + 1;
                    self.state = Scan::Escape;
                    continue;
                }
                Scan::Escape => {
                    self.state = match byte {
                        b']' => {
                            self.len = 0;
                            Scan::Osc
                        }
                        ESC => Scan::Escape,
                        _ => Scan::Ground,
                    };
                }
                // 长的 OSC（比如写剪贴板的）只看控制字符。
                Scan::LongOsc if byte >= 0x20 => {
                    i += data[i..].iter().position(|&b| b < 0x20)?;
                    continue;
                }
                Scan::Osc | Scan::LongOsc => match byte {
                    BEL | ESC => {
                        let query = match self.state {
                            Scan::Osc => parse_query(&self.body[..self.len], byte == BEL),
                            _ => None,
                        };
                        self.state = if byte == ESC { Scan::Escape } else { Scan::Ground };
                        if let Some(query) = query {
                            return Some((i + 1, query));
                        }
                    }
                    CAN | SUB => self.state = Scan::Ground,
                    0..0x20 => {}
                    _ if self.len < QUERY_MAX => {
                        self.body[self.len] = byte;
                        self.len += 1;
                    }
                    _ => self.state = Scan::LongOsc,
                },
            }
            i += 1;
        }
        None
    }
}

/// OSC 的内容是不是读剪贴板的请求。
fn parse_query(body: &[u8], bel: bool) -> Option<ClipboardQuery> {
    let target = match body.strip_prefix(b"52;")? {
        [b';', b'?'] => b'c',
        [target, b';', b'?'] if *target != b';' => match target {
            b's' | b'p' => *target,
            _ => b'c',
        },
        _ => return None,
    };
    Some(ClipboardQuery { target, bel })
}

/// 标准 base64（带 `=` 补齐）编码，接在 `out` 后面。
fn encode_base64(data: &[u8], out: &mut Vec<u8>) {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for chunk in data.chunks(3) {
        let b = [chunk[0], chunk.get(1).copied().unwrap_or(0), chunk.get(2).copied().unwrap_or(0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let digits = [(n >> 18) & 63, (n >> 12) & 63, (n >> 6) & 63, n & 63];
        for (k, digit) in digits.iter().enumerate() {
            out.push(if k <= chunk.len() { ALPHABET[*digit as usize] } else { b'=' });
        }
    }
}

#[cfg(test)]
mod tests;
