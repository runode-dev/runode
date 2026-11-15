//! 把 Kitty 图形协议按文件（`t=f`）和临时文件（`t=t`）传的图读出来，改写成直接传（`t=d`）：
//! 认出要改写的命令、写出改写结果，按会话限制改写出的总量；怎么找文件、能读哪些文件在 `read`。

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod read;
#[cfg(test)]
mod tests;

use std::time::Instant;

use crate::vt::{KITTY_CHUNK_BYTES, decode_base64, encode_base64};

const ESC: u8 = 0x1b;
/// 取消正在进行的序列的两个 C0 控制字符。
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;
/// C1 的字符串结束符 ST。
const ST_C1: u8 = 0x9c;

/// 改写时单个文件最多读这么多字节，也是改写预算（`Budget`）的容量，同一时刻改写读进来的总量不超过
/// 它。一次喂入改写出的字节作为一帧转给前端（`Fed::rewritten`），帧的大小因此有界。
pub(super) const IMAGE_FILE_MAX_BYTES: u64 = 16 << 20;

/// 改写预算每秒补回的字节数。改写出的字节要转给每个前端，包括经 SSH 的远端和隔着 Wi-Fi、中继的手机，
/// 一条百来字节的命令不能放大成没有上限的流量。
const REWRITE_REFILL_BYTES_PER_SEC: u64 = IMAGE_FILE_MAX_BYTES / 2;

/// 扣着不喂给 VT 的 APC 内容的上限，装得下控制键加上经 base64 的最长路径（`PATH_MAX`）；比这长的
/// 不会是要改写的那种。
const HELD_MAX_BYTES: usize = 8 << 10;

/// 图从哪里读。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Medium {
    /// `t=f`：普通文件，读完留着。
    File,
    /// `t=t`：临时文件，只认临时目录里名字对的，读完删掉。
    TemporaryFile,
}

/// 一条要改写的图形命令的控制键。
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Control {
    pub(super) medium: Medium,
    /// `O`：从文件的第几个字节读起。
    pub(super) offset: u64,
    /// `S`：读几个字节，0 是读到末尾。
    pub(super) size: u64,
    /// 改写后照写的控制键，逗号分隔、次序不变：去掉了 `t`、`S`、`O` 和 `m`。
    kept: Vec<u8>,
    /// `q` 的原值，续块也要带上，VT 才对整条命令都照样不回话或只回错误。
    quiet: Option<Vec<u8>>,
    /// `a=q`：只问支不支持，数据不分块。
    query: bool,
}

/// 解析 APC 内容里 `G` 之后到 `;` 之前的控制键。只收保守的形状：每个键是一个 ASCII 字母，值是
/// 一串字母、数字或 `-`，逗号分隔，键不重复；而且只在要改写时才是 `Some`：`t` 是 `f` 或 `t`，`a`
/// 没写或是 `t`、`T`、`q`、`f`，`m` 没写或是 `0`，`S`、`O` 是十进制的 32 位无符号数。
pub(super) fn parse_control(keys: &[u8]) -> Option<Control> {
    let mut seen = [false; 128];
    let (mut medium, mut offset, mut size, mut quiet, mut query) = (None, 0, 0, None, false);
    let mut kept = Vec::new();
    for pair in keys.split(|&c| c == b',') {
        let &[key, b'=', ref value @ ..] = pair else { return None };
        if !key.is_ascii_alphabetic()
            || value.is_empty()
            || !value.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
            || std::mem::replace(&mut seen[usize::from(key)], true)
        {
            return None;
        }
        match key {
            b't' => {
                medium = Some(match value {
                    b"f" => Medium::File,
                    b"t" => Medium::TemporaryFile,
                    _ => return None,
                });
            }
            b'S' => size = decimal(value)?,
            b'O' => offset = decimal(value)?,
            b'm' if value != b"0" => return None,
            b'm' => {}
            _ => {
                match key {
                    b'a' if !matches!(value, b"t" | b"T" | b"q" | b"f") => return None,
                    b'a' => query = value == b"q",
                    b'q' => quiet = Some(value.to_vec()),
                    _ => {}
                }
                if !kept.is_empty() {
                    kept.push(b',');
                }
                kept.extend_from_slice(pair);
            }
        }
    }
    Some(Control { medium: medium?, offset, size, kept, quiet, query })
}

fn decimal(value: &[u8]) -> Option<u64> {
    if !value.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(value).ok()?.parse::<u32>().ok().map(u64::from)
}

/// 把读出来的 `data` 写成直接传的命令，接在 VT 已经吃进的 `ESC _` 后面。数据按 `KITTY_CHUNK_BYTES`
/// 分块，续块前面接上一块的 `ESC \` 和自己的 `ESC _`；`a=q` 和只有一块时不分块、不写 `m`。最后一块
/// 不带结束符，由流里原来的结束字节结束。
///
/// shortcut: 程序把一次分块直接传扔在半路、接着按文件传时，改写出的命令会被 VT 当成那次加载的续块；
/// 遇到真有程序这样做时，在改写结果前补一条放弃那次加载的命令。
pub(super) fn rewrite(control: &Control, data: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(data.len().div_ceil(3) * 4);
    encode_base64(data, &mut encoded);
    let count = encoded.len().div_ceil(KITTY_CHUNK_BYTES);
    let mut out = Vec::with_capacity(control.kept.len() + encoded.len() + count * 16 + 8);
    out.push(b'G');
    out.extend_from_slice(&control.kept);
    if control.query || count <= 1 {
        out.push(b';');
        out.extend_from_slice(&encoded);
        return out;
    }
    for (n, chunk) in encoded.chunks(KITTY_CHUNK_BYTES).enumerate() {
        if n == 0 {
            if !control.kept.is_empty() {
                out.push(b',');
            }
            out.extend_from_slice(b"m=1;");
        } else {
            out.extend_from_slice(b"\x1b\\\x1b_Gm=");
            out.push(if n + 1 < count { b'1' } else { b'0' });
            if let Some(quiet) = &control.quiet {
                out.extend_from_slice(b",q=");
                out.extend_from_slice(quiet);
            }
            out.push(b';');
        }
        out.extend_from_slice(chunk);
    }
    out
}

/// 宿主那份 VT 喂字节时，把按文件和临时文件传图的 APC 截下来改写成直接传：VT 吃进 APC 的引导
/// `ESC _` 以后调 `start`，之后的内容先扣着，到结束字节时读出文件、换成 `rewrite` 的结果喂给 VT。
/// 读不成、预算不够或不是要改写的命令时原样放行，VT 照旧回不支持。
///
/// APC 内容里 0x00–0x17、0x19、0x1C–0x7F 都算内容；ESC 和 C1 的 ST 结束它，CAN、SUB 取消，其余
/// C1 中止。命令可能被切在几块输出之间，扣着的内容跨块保留。
#[derive(Debug, Default)]
pub(super) struct MediumRewriter {
    /// VT 已经吃进了 APC 的引导，之后的这些内容还没喂给它。
    held: Option<Vec<u8>>,
    budget: Budget,
}

/// `MediumRewriter::step` 处理完一块输出的结果。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Step {
    /// 整块都扣下了。
    Held,
    /// 扣留结束，这块输出的前这么多字节原样喂给 VT（也原样转发）。
    Pass(usize),
    /// 扣留结束，喂给 VT（也是要转发）的是这些字节，顶替这块输出的前 `usize` 个字节和之前扣下的。
    Replace(Vec<u8>, usize),
}

impl MediumRewriter {
    /// VT 刚吃进 APC 的引导，开始扣着之后的内容。只在 macOS 和 Linux 上改写，别的系统上什么都不做。
    pub(super) fn start(&mut self) {
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            self.held = Some(Vec::new());
        }
    }

    /// 正扣着一条 APC 的内容，接下来的输出要先交给 `step`。
    pub(super) fn holding(&self) -> bool {
        self.held.is_some()
    }

    /// 处理接下来的一块输出，`now` 是现在，按它补改写预算。扣留结束时剩下的字节照常喂给 VT，放行时
    /// 用掉的可能不到这条 APC 的末尾。结束字节不算用掉，留给 VT 自己结束这条 APC。没在扣着时什么都
    /// 不用，是 `Pass(0)`。
    ///
    /// 之前没扣下东西时先直接在 `data` 上看，认出不用改写的（比如直接传的数据）就放行，不拷贝，也
    /// 不往后找结束字节。
    pub(super) fn step(&mut self, data: &[u8], now: Instant) -> Step {
        let Some(mut held) = self.held.take() else { return Step::Pass(0) };
        // 扣着的不会超过 `HELD_MAX_BYTES`，只看够认出超了的那么多。
        let window = &data[..data.len().min((HELD_MAX_BYTES + 1).saturating_sub(held.len()))];
        if held.is_empty() {
            if window.first().is_some_and(|&c| c != b'G') {
                return Step::Pass(0);
            }
            if let Some(semicolon) = memchr::memchr(b';', window)
                && parse_control(&window[1..semicolon]).is_none()
            {
                return Step::Pass(0);
            }
        }
        let end = window.iter().position(|&c| matches!(c, CAN | SUB | ESC | 0x80..=0x9f));
        let content = &window[..end.unwrap_or(window.len())];
        if held.is_empty() {
            if !may_rewrite(content, 0) {
                return Step::Pass(content.len());
            }
            let Some(end) = end else {
                held.extend_from_slice(content);
                self.held = Some(held);
                return Step::Held;
            };
            return match self.finish(content, data[end], now) {
                Some(bytes) => Step::Replace(bytes, end),
                None => Step::Pass(end),
            };
        }
        let start = held.len();
        held.extend_from_slice(content);
        if !may_rewrite(&held, start) {
            return Step::Replace(held, content.len());
        }
        let Some(end) = end else {
            self.held = Some(held);
            return Step::Held;
        };
        let bytes = self.finish(&held, data[end], now).unwrap_or(held);
        Step::Replace(bytes, end)
    }

    /// 由 `terminator` 结束的完整 APC 内容 `content` 改写成直接传的结果；被取消、不是要改写的命令、
    /// 预算不够或读不成文件时是 `None`。
    fn finish(&mut self, content: &[u8], terminator: u8, now: Instant) -> Option<Vec<u8>> {
        if !matches!(terminator, ESC | ST_C1) {
            return None;
        }
        let content = content.strip_prefix(b"G")?;
        let (keys, payload) = match memchr::memchr(b';', content) {
            Some(semicolon) => (&content[..semicolon], &content[semicolon + 1..]),
            None => (content, &[][..]),
        };
        let control = parse_control(keys)?;
        let path = decode_base64(payload)?;
        let data = load(&control, &path, self.budget.left(now))?;
        self.budget.spend(data.len() as u64);
        Some(rewrite(&control, &data))
    }
}

/// 扣着的内容 `held`（从 `start` 起是新到的）还可能是要改写的命令。
fn may_rewrite(held: &[u8], start: usize) -> bool {
    if held.first().is_some_and(|&c| c != b'G') {
        return false;
    }
    let had_semicolon = start > 0 && memchr::memchr(b';', &held[..start]).is_some();
    if !had_semicolon
        && let Some(semicolon) = memchr::memchr(b';', &held[start..])
        && parse_control(&held[1..start + semicolon]).is_none()
    {
        return false;
    }
    held.len() <= HELD_MAX_BYTES && !held[start..].iter().any(|&c| c < 0x20 || c == 0x7f)
}

/// 按会话的改写预算，一个令牌桶：最多攒 `IMAGE_FILE_MAX_BYTES` 个字节，改写读进来多少扣多少，按
/// `REWRITE_REFILL_BYTES_PER_SEC` 补回。
#[derive(Debug)]
struct Budget {
    left: u64,
    /// 上次补到的时刻，还没补过时是 `None`。
    refilled_at: Option<Instant>,
}

impl Default for Budget {
    fn default() -> Self {
        Self { left: IMAGE_FILE_MAX_BYTES, refilled_at: None }
    }
}

impl Budget {
    /// 补到 `now` 以后还剩多少字节。不满一个字节的时间留到下次再补，补得再勤也不会一直补不上。
    fn left(&mut self, now: Instant) -> u64 {
        let refill = self.refilled_at.map_or(u128::MAX, |at| {
            now.saturating_duration_since(at).as_nanos().saturating_mul(u128::from(REWRITE_REFILL_BYTES_PER_SEC))
                / 1_000_000_000
        });
        if refill > 0 {
            let refill = u64::try_from(refill).unwrap_or(u64::MAX);
            self.left = self.left.saturating_add(refill).min(IMAGE_FILE_MAX_BYTES);
            self.refilled_at = Some(now);
        }
        self.left
    }

    fn spend(&mut self, bytes: u64) {
        self.left = self.left.saturating_sub(bytes);
    }
}

/// 读出 `control` 指的文件里要的那段，要读的超过 `max` 个字节时不读。
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn load(control: &Control, path: &[u8], max: u64) -> Option<Vec<u8>> {
    read::read_file(control.medium, path, control.offset, control.size, max)
        .inspect_err(|err| tracing::debug!("not rewriting a Kitty graphics file transmission: {err}"))
        .ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn load(_control: &Control, _path: &[u8], _max: u64) -> Option<Vec<u8>> {
    None
}
