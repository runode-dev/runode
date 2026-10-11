use std::{cell::RefCell, rc::Rc};

use libghostty_vt::Terminal;
use runode_shared_types::grid::GridSize;

use super::*;
use crate::testing::bare_host;

fn write(text: &str) -> ClipboardRequest {
    ClipboardRequest::Write(text.into())
}

fn read(target: u8, bel: bool) -> ClipboardRequest {
    ClipboardRequest::Read(ClipboardQuery { target, bel })
}

/// 把 `bytes` 从每一个位置切成两块分别喂给一个新会话，每种切法都要认出同样的请求。
fn every_split(bytes: &[u8], expected: &[ClipboardRequest]) {
    for at in 0..=bytes.len() {
        let mut session = bare_host();
        session.feed(&bytes[..at]);
        session.feed(&bytes[at..]);
        assert_eq!(session.take_clipboard(), expected, "split at {at} of {:?}", String::from_utf8_lossy(bytes));
    }
}

fn requests(bytes: &[u8]) -> Vec<ClipboardRequest> {
    let mut session = bare_host();
    session.feed(bytes);
    session.take_clipboard()
}

#[test]
fn writes_arrive_decoded_however_they_are_split_or_ended() {
    every_split(b"\x1b]52;c;aGVsbG8=\x07", &[write("hello")]);
    every_split(b"\x1b]52;c;aGVsbG8=\x1b\\", &[write("hello")]);
    // 中文按 UTF-8 编码。
    every_split("\x1b]52;c;5Lit5paH\x07".as_bytes(), &[write("中文")]);
    let mut session = bare_host();
    session.feed(b"\x1b]52;c;aGVs");
    assert!(session.take_clipboard().is_empty(), "not finished yet");
    session.feed(b"bG8=\x07");
    assert_eq!(session.take_clipboard(), [write("hello")]);
    assert!(session.take_clipboard().is_empty(), "taken already");
}

/// macOS 只有一个系统剪贴板：`s`、`p`、空目标和别的单个字符的目标都写进去；一次写几个目标的
/// 写法 VT 不认。
#[test]
fn every_target_writes_the_system_clipboard() {
    for target in ["c", "s", "p", "", "0"] {
        assert_eq!(requests(format!("\x1b]52;{target};aGk=\x07").as_bytes()), [write("hi")], "{target:?}");
    }
    assert!(requests(b"\x1b]52;sc;aGk=\x07").is_empty());
}

#[test]
fn bad_writes_are_dropped() {
    // 不是合法的 base64。
    assert!(requests(b"\x1b]52;c;!!!!\x07").is_empty());
    // 空的载荷是要清空剪贴板，不办。
    assert!(requests(b"\x1b]52;c;\x07").is_empty());
    // 正好到上限的照写，超过的整条丢掉。
    let mut at_limit = Vec::new();
    encode_base64(&vec![b'x'; MAX_CLIPBOARD_BYTES], &mut at_limit);
    let sequence = |payload: &[u8]| [&b"\x1b]52;c;"[..], payload, b"\x07"].concat();
    let written = requests(&sequence(&at_limit));
    assert!(matches!(&written[..], [ClipboardRequest::Write(text)] if text.len() == MAX_CLIPBOARD_BYTES));
    let mut over = Vec::new();
    encode_base64(&vec![b'x'; MAX_CLIPBOARD_BYTES + 1], &mut over);
    let mut session = bare_host();
    session.feed(&sequence(&over));
    assert!(session.take_clipboard().is_empty());
    // 丢掉的不影响之后的。
    session.feed(b"\x1b]52;c;aGk=\x07");
    assert_eq!(session.take_clipboard(), [write("hi")]);
}

#[test]
fn text_that_is_not_utf8_is_converted_lossily() {
    // "f", 0xff, "o" 的 base64。
    assert_eq!(requests(b"\x1b]52;c;Zv9v\x07"), [write("f\u{fffd}o")]);
}

#[test]
fn reads_are_recognized_however_they_are_split_or_ended() {
    every_split(b"\x1b]52;c;?\x07", &[read(b'c', true)]);
    every_split(b"\x1b]52;c;?\x1b\\", &[read(b'c', false)]);
    every_split(b"x\x1b]52;;?\x07y", &[read(b'c', true)]);
    every_split(b"\x1b]52;s;?\x07", &[read(b's', true)]);
    every_split(b"\x1b]52;p;?\x1b\\", &[read(b'p', false)]);
    // 别的单个字符的目标回话时写成 `c`，和 VT 一样。
    every_split(b"\x1b]52;0;?\x07", &[read(b'c', true)]);
    // OSC 里的控制字符 VT 跳过，不算内容。
    every_split(b"\x1b]52;c\n;?\x07", &[read(b'c', true)]);
    // 任何 ESC 都结束 OSC：后面不是 `\` 也算以 ST 结尾，接着开始的是一条新的序列。
    every_split(b"\x1b]52;c;?\x1b]0;title\x07", &[read(b'c', false)]);
    every_split(b"\x1b\x1b]52;c;?\x07", &[read(b'c', true)]);
    // ESC 和 `]` 之间的控制字符 VT 照常执行，仍是 OSC。
    every_split(b"\x1b\n]52;c;?\x07", &[read(b'c', true)]);
    every_split(b"\x1b\x7f]52;c;?\x07", &[read(b'c', true)]);
    // 别的字符串里的 ESC 结束那条字符串，接着的是 OSC。
    for start in [&b"\x1bPq"[..], b"\x1b_G", b"\x1b^", b"\x1bX", b"\x1b]2;title"] {
        every_split(&[start, b"\x1b]52;c;?\x07"].concat(), &[read(b'c', true)]);
    }
}

#[test]
fn only_queries_the_vt_also_sees_count() {
    for bytes in [
        &b"\x1b]52;c;??\x07"[..],
        b"\x1b]52;c;?x\x07",
        b"\x1b]52;;;?\x07",
        b"\x1b]52;sc;?\x07",
        b"\x1b]520;c;?\x07",
        b"\x1b]5;c;?\x07",
        b"\x1b]52;c?\x07",
        // ESC 和 `]` 之间夹着别的字节就不是 OSC 了。
        b"\x1b(]52;c;?\x07",
        b"\x1b\x18]52;c;?\x07",
        // DCS、APC、PM、SOS 的内容里出现的字样（没有 ESC 就还在那条字符串里）。
        b"\x1bPq]52;c;?\x07\x1b\\",
        b"\x1b_G]52;c;?\x07\x1b\\",
        b"\x1b^]52;c;?\x07\x1b\\",
        b"\x1bX]52;c;?\x07\x1b\\",
        // 8 位的 C1 控制字符不认：单个字节的 OSC 开头、ST（在 OSC 里是内容）。
        b"\x9d52;c;?\x07",
        b"\x1b]52;c;?\x9c",
        b"]52;c;?\x07",
        // CAN、SUB 取消正在进行的 OSC。
        b"\x1b]52;c\x18;?\x07",
        b"\x1b]52;c;\x1a?\x07",
        // 在别的 OSC 里面的字样。
        b"\x1b]2;x 52;c;?\x07",
        b"\x1b]2;\x1b]52;c;?",
    ] {
        assert!(requests(bytes).is_empty(), "{:?}", String::from_utf8_lossy(bytes));
    }
}

/// 读写混在一块里时按真实的先后排：读请求在 VT 处理到它的位置记下。
#[test]
fn reads_and_writes_keep_their_order() {
    every_split(
        b"\x1b]52;c;?\x07\x1b]52;c;aGk=\x07\x1b]52;c;?\x1b\\",
        &[read(b'c', true), write("hi"), read(b'c', false)],
    );
}

/// VT 自己不回应读请求（没装读的回调），回话全由宿主经 `answer_clipboard` 写。
#[test]
fn the_vt_itself_does_not_answer_reads() {
    let mut session = bare_host();
    let before = session.replies();
    session.feed(b"\x1b]52;c;?\x07\x1b]52;c;?\x1b\\");
    assert_eq!(session.replies(), before);
    assert_eq!(session.take_clipboard().len(), 2);
}

#[test]
fn answers_use_the_target_and_terminator_of_the_query() {
    let bel = ClipboardQuery { target: b'c', bel: true };
    assert_eq!(bel.answer("hello"), b"\x1b]52;c;aGVsbG8=\x07");
    assert_eq!(bel.answer(""), b"\x1b]52;c;\x07");
    let st = ClipboardQuery { target: b'p', bel: false };
    assert_eq!(st.answer("中文"), b"\x1b]52;p;5Lit5paH\x1b\\");
    for (text, encoded) in [("a", "YQ=="), ("ab", "YWI="), ("abc", "YWJj"), ("abcd", "YWJjZA=="), ("\u{ff}~", "w79+")] {
        let mut out = Vec::new();
        encode_base64(text.as_bytes(), &mut out);
        assert_eq!(out, encoded.as_bytes(), "{text:?}");
    }
}

/// 一份只接了写剪贴板回调的 VT，`writes` 是写剪贴板的开关；返回 VT、记下的请求和它写回程序的字节。
fn writing_terminal(writes: bool) -> (Terminal<'static, 'static>, Rc<Effects>, Rc<RefCell<Vec<u8>>>) {
    let mut terminal =
        crate::vt::new_terminal(GridSize { cols: 20, rows: 4, cell_width_px: 8, cell_height_px: 16 }).unwrap();
    let effects = Rc::new(Effects::default());
    effects.clipboard_writes.set(writes);
    let written = Rc::new(RefCell::new(Vec::new()));
    terminal
        .on_pty_write({
            let written = written.clone();
            move |_, data| written.borrow_mut().extend_from_slice(data)
        })
        .unwrap()
        .on_clipboard_write({
            let effects = effects.clone();
            move |_, write| take_write(write, &effects)
        })
        .unwrap();
    (terminal, effects, written)
}

/// 一次带应答的写事务（OSC 5522）：写 "Ghost"。
const KITTY_WRITE: &[u8] =
    b"\x1b]5522;type=write:id=c1\x1b\\\x1b]5522;type=wdata:mime=dGV4dC9wbGFpbg==;R2hvc3Q=\x1b\\\x1b]5522;type=wdata\x1b\\";

/// 不让写时，每种写剪贴板的序列都当场拒绝、不记下来，带应答的写事务收到「不允许」；让写时照常
/// 记下，应答是成功。
#[test]
fn denied_writes_are_refused_by_the_vt() {
    let sequences: [&[u8]; 3] = [b"\x1b]52;c;aGk=\x07", b"\x1b]1337;Copy=:aGk=\x07", KITTY_WRITE];
    for bytes in sequences {
        let (mut terminal, effects, written) = writing_terminal(false);
        terminal.vt_write(bytes);
        assert!(effects.clipboard.borrow().is_empty(), "{:?}", String::from_utf8_lossy(bytes));
        if bytes == KITTY_WRITE {
            assert_eq!(&*written.borrow(), b"\x1b]5522;type=write:status=EPERM:id=c1\x1b\\");
        }
    }
    let expected = [write("hi"), write("hi"), write("Ghost")];
    for (bytes, expected) in sequences.into_iter().zip(expected) {
        let (mut terminal, effects, written) = writing_terminal(true);
        terminal.vt_write(bytes);
        assert_eq!(*effects.clipboard.borrow(), [expected]);
        if bytes == KITTY_WRITE {
            assert_eq!(&*written.borrow(), b"\x1b]5522;type=write:status=DONE:id=c1\x1b\\");
        }
    }
    // 会话上的开关。
    let mut session = bare_host();
    session.set_clipboard_writes(false);
    session.feed(b"\x1b]52;c;aGk=\x07\x1b]52;c;?\x07");
    assert_eq!(session.take_clipboard(), [read(b'c', true)], "reads are not writes");
    session.set_clipboard_writes(true);
    session.feed(b"\x1b]52;c;aGk=\x07");
    assert_eq!(session.take_clipboard(), [write("hi")]);
}

/// 一份装了读剪贴板回调（一律拒绝）的 VT 回给程序的 OSC 52 回话，按先后：VT 自己认出的读请求，
/// 拒绝时它回空的剪贴板，目标和终止符照请求的。
fn vt_answers(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut terminal =
        crate::vt::new_terminal(GridSize { cols: 20, rows: 4, cell_width_px: 8, cell_height_px: 16 }).unwrap();
    let written = Rc::new(RefCell::new(Vec::new()));
    terminal
        .on_pty_write({
            let written = written.clone();
            move |_, data| written.borrow_mut().push(data.to_vec())
        })
        .unwrap()
        .on_clipboard_read(|_, read| read.reply(Err(libghostty_vt::terminal::ClipboardReadError::Denied), &[], false))
        .unwrap();
    terminal.vt_write(bytes);
    written.take().into_iter().filter(|answer| answer.starts_with(b"\x1b]52;")).collect()
}

/// `QueryScanner` 认出的读请求，按切成的块一块块扫，回空的剪贴板时写的字节。
fn scanner_answers(bytes: &[u8], cuts: &[usize]) -> Vec<Vec<u8>> {
    let mut scanner = QueryScanner::default();
    let mut answers = Vec::new();
    let mut start = 0;
    for end in cuts.iter().copied().chain([bytes.len()]) {
        let mut rest = &bytes[start..end];
        while let Some((at, query)) = scanner.scan(rest) {
            answers.push(query.answer(""));
            rest = &rest[at..];
        }
        start = end;
    }
    answers
}

/// 一个固定种子的伪随机数（xorshift），测试每次跑的都是同样的输入。
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

/// 拿真的 VT 对拍：同一串字节喂给装了读回调的 VT 和 `QueryScanner`（随机切块），扫描器认出的每个
/// 读请求 VT 也认出了，目标和终止符一样、先后一样。输入由容易拼出各种转义序列的片段随机连成；只有
/// 7 位字节时两边应当完全一样，加上 8 位的 C1 控制字符和 UTF-8 的字节时扫描器只许漏认。
#[test]
fn the_scanner_agrees_with_the_vt() {
    let seven_bit: &[&[u8]] = &[
        b"\x1b", b"\x1b", b"]", b"]", b"52;", b"52;", b"5522;", b"c;", b"s;", b";", b"?", b"?", b"\x07", b"\x1b\\",
        b"\\", b"\x18", b"\x1a", b"\n", b"\x01", b"\x7f", b"\x1bP", b"\x1b_", b"\x1b^", b"\x1bX", b"\x1b(", b"q", b"x",
        b"2;t", b" ",
    ];
    let c1: &[&[u8]] = &[b"\x9d", b"\x9c", b"\x90", b"\x9b", b"\xc3", b"\xa9"];
    let with_c1: Vec<&[u8]> = seven_bit.iter().chain(c1).copied().collect();
    // 先对拍几条挑出来的：控制字符夹在 ESC 和 `]` 之间、别的字符串里的 ESC、取消。
    for bytes in [
        &b"\x1b\n]52;c;?\x07"[..],
        b"\x1b\x7f]52;p;?\x1b\\",
        b"\x1b\x18]52;c;?\x07",
        b"\x1b(]52;c;?\x07",
        b"\x1bPq\x1b]52;c;?\x07",
        b"\x1b_G]52;c;?\x07\x1b\\",
        b"\x1b]52;c\x18;?\x07",
        b"\x1b]52;c\n;?\x07",
        b"\x1b]52;c;?\x1b]0;x\x07",
        b"\x1b]52;;;?\x07",
    ] {
        assert_eq!(scanner_answers(bytes, &[]), vt_answers(bytes), "{:?}", String::from_utf8_lossy(bytes));
    }
    let mut rng = Rng(0x5eed_0052_c0de_cafe);
    for round in 0..1000 {
        let pieces = if round % 2 == 0 { seven_bit } else { &with_c1[..] };
        let random = |rng: &mut Rng, count: usize| -> Vec<u8> {
            (0..count).flat_map(|_| pieces[rng.below(pieces.len())].iter().copied()).collect()
        };
        // 一半是随便连的片段；一半是差一点的读请求：每一段之前有一定机会夹进随机的片段。
        let bytes = if round % 4 < 2 {
            let count = 1 + rng.below(16);
            random(&mut rng, count)
        } else {
            let count = rng.below(3);
            let mut bytes = random(&mut rng, count);
            let target: &[u8] = [&b"c;"[..], b";", b"p;", b"0;"][rng.below(4)];
            let end: &[u8] = [&b"\x07"[..], b"\x1b\\"][rng.below(2)];
            for part in [&b"\x1b"[..], b"]", b"52;", target, b"?", end] {
                if rng.below(4) == 0 {
                    let count = 1 + rng.below(2);
                    bytes.extend(random(&mut rng, count));
                }
                bytes.extend_from_slice(part);
            }
            bytes
        };
        let mut cuts: Vec<usize> = (0..rng.below(4)).map(|_| rng.below(bytes.len() + 1)).collect();
        cuts.sort_unstable();
        let vt = vt_answers(&bytes);
        let scanned = scanner_answers(&bytes, &cuts);
        let shown = String::from_utf8_lossy(&bytes).into_owned();
        if pieces.len() == seven_bit.len() {
            assert_eq!(scanned, vt, "round {round}: {shown:?} cut at {cuts:?}");
        } else {
            // 扫描器认出的按先后是 VT 认出的里的一部分。
            let mut vt = vt.iter();
            for answer in &scanned {
                assert!(vt.any(|seen| seen == answer), "round {round}: {shown:?} cut at {cuts:?}");
            }
        }
    }
}
