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
//! 所以读的回调不装（没有它时 VT 不回应 OSC 52 的读），由 `QueryScanner` 借 VT 切出的 OSC 认出
//! 读请求，宿主拿到结果后按请求的终止符自己回话（`ClipboardQuery::answer`）。

use libghostty_vt::{
    Terminal,
    terminal::{Boundary, ClipboardWrite, ClipboardWriteError, StringEnd},
};
use runode_types::clipboard::MAX_CLIPBOARD_BYTES;

use super::effects::Effects;
use crate::vt::encode_base64;

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
            // 剪贴板放的是文字，系统剪贴板只收合法的 Unicode：不是 UTF-8 的字节换成 U+FFFD，其余
            // 照写，比整条丢掉更接近程序的本意。一个字节换成的 U+FFFD 占三个字节，换完再查一次上限。
            Some(content) => match String::from_utf8_lossy(content.data) {
                text if text.len() > MAX_CLIPBOARD_BYTES => {
                    tracing::warn!(
                        "dropped a clipboard write of {} bytes once made valid UTF-8, over the limit",
                        text.len()
                    );
                    Err(ClipboardWriteError::InvalidData)
                }
                text => match effects.clipboard.try_borrow_mut() {
                    Ok(mut requests) => {
                        requests.push(ClipboardRequest::Write(text.into_owned()));
                        Ok(())
                    }
                    // 回调运行在 extern "C" 函数里，不能 panic；借不到时丢掉这一条。
                    Err(_) => Err(ClipboardWriteError::Busy),
                },
            },
        }
    };
    write.reply(result, false);
}

fn is_plain_text(mime: &[u8]) -> bool {
    mime == b"text/plain" || mime.starts_with(b"text/plain;")
}

/// 读请求 OSC 的内容最长是 `52;c;?`，再长就不是读请求了。
const QUERY_MAX: usize = 6;

/// 认宿主 VT 里 OSC 52 的读请求（`ESC ] 52 ; <目标> ; ? BEL`，或以 ST 结尾），跨块接着认。每条 OSC
/// 从哪开始、到哪结束由 VT 自己切（`Terminal::vt_write_until`），这里只记下内容的开头几个字节：
/// VT 在 OSC 里跳过的控制字符不算内容。内容按 VT 的写法拆：`52;` 之后是一个字符的目标和 `;`，
/// 或者直接是 `;`（空目标），再之后正好是 `?`。
#[derive(Default)]
pub(super) struct QueryScanner {
    /// 正在一条 OSC 里。
    in_osc: bool,
    /// 这条 OSC 到目前为止的内容，最多 `QUERY_MAX` 字节。
    body: [u8; QUERY_MAX],
    /// 内容的字节数；内容比 `QUERY_MAX` 长时是 `QUERY_MAX + 1`，后面的不再数。
    len: usize,
}

impl QueryScanner {
    /// 把 `data` 喂给 `terminal`，每处理完一条读请求就把它交给 `found`，再接着喂后面的字节。
    pub(super) fn write(
        &mut self,
        terminal: &mut Terminal<'_, '_>,
        mut data: &[u8],
        mut found: impl FnMut(ClipboardQuery),
    ) {
        const OSC: Boundary = Boundary::OSC_START.union(Boundary::OSC_END);
        loop {
            let stop = match terminal.vt_write_until(data, OSC) {
                Ok(Some(stop)) => stop,
                Ok(None) => return self.collect(data),
                Err(err) => {
                    tracing::warn!("vt write until an OSC boundary failed: {err}");
                    self.in_osc = false;
                    return terminal.vt_write(data);
                }
            };
            let (done, rest) = data.split_at(stop.consumed());
            data = rest;
            if stop.reached().contains(Boundary::OSC_START) {
                (self.in_osc, self.len) = (true, 0);
                continue;
            }
            // 结束的那个字节不是内容。
            self.collect(&done[..done.len().saturating_sub(1)]);
            let bel = match stop.end() {
                Ok(StringEnd::Bel) => Some(true),
                Ok(StringEnd::StEsc) => Some(false),
                _ => None,
            };
            if std::mem::take(&mut self.in_osc)
                && self.len <= QUERY_MAX
                && let Some(query) = bel.and_then(|bel| parse_query(&self.body[..self.len], bel))
            {
                found(query);
            }
        }
    }

    fn collect(&mut self, content: &[u8]) {
        if !self.in_osc || self.len > QUERY_MAX {
            return;
        }
        for &byte in content.iter().filter(|&&b| b >= 0x20) {
            let Some(slot) = self.body.get_mut(self.len) else {
                self.len = QUERY_MAX + 1;
                return;
            };
            *slot = byte;
            self.len += 1;
        }
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

#[cfg(test)]
mod tests;
