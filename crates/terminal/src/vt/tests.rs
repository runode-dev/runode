//! `vt` 的测试，以及差分测试（见 `Session::from_snapshot` 的测试）共用的测试数据。

use std::time::{Duration, Instant};

use super::*;
use libghostty_vt::terminal::Mode;

/// 测试数据用的伪随机数，固定种子，结果可重现。
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub(crate) fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// `0..n` 里的一个数；`n` 为 0 时为 0。
    pub(crate) fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }

    /// 百分之 `percent` 的机会。
    pub(crate) fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    pub(crate) fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

pub(crate) fn size(cols: u16, rows: u16) -> GridSize {
    GridSize { cols, rows, cell_width_px: 8, cell_height_px: 16 }
}

/// 把 `data` 切成随机长短（1 到 `max` 字节）的几段依次喂进去，像从 PTY 一次次读到的那样。
pub(crate) fn feed_chunked(terminal: &mut Terminal<'_, '_>, data: &[u8], rng: &mut Rng, max: usize) {
    let mut rest = data;
    while !rest.is_empty() {
        let n = (1 + rng.below(max)).min(rest.len());
        terminal.vt_write(&rest[..n]);
        rest = &rest[n..];
    }
}

/// 像真实程序输出的 `lines` 行：彩色文字、中文、超链接，偶尔有光标移动、清行、标题和 OSC 133
/// 语义标记；有的行比 `cols` 长，会软折行。
pub(crate) fn mixed_output(rng: &mut Rng, lines: usize, cols: usize) -> Vec<u8> {
    const WORDS: [&str; 8] = ["build", "cargo", "error[E0425]:", "warning", "ok", "src/main.rs:12:5", "-->", "|"];
    const WIDE: [&str; 4] = ["中文", "终端会话", "宽字符", "日本語"];
    let mut out = Vec::new();
    for line in 0..lines {
        if rng.chance(3) {
            out.extend_from_slice(format!("\x1b]2;title {line}\x07").as_bytes());
        }
        if rng.chance(3) {
            out.extend_from_slice(b"\x1b]133;A\x07$ \x1b]133;B\x07ls -la\r\n\x1b]133;C\x07");
        }
        let target = cols * (50 + rng.below(80)) / 100;
        let mut width = 0;
        while width < target {
            match rng.below(10) {
                0 => out.extend_from_slice(format!("\x1b[3{}m", rng.below(8)).as_bytes()),
                1 => out.extend_from_slice(format!("\x1b[38;5;{}m", rng.below(256)).as_bytes()),
                2 => out.extend_from_slice(
                    format!("\x1b[1;38;2;{};{};{}m", rng.below(256), rng.below(256), rng.below(256)).as_bytes(),
                ),
                3 => out.extend_from_slice(b"\x1b[0m"),
                4 => {
                    let word = rng.pick(&WIDE);
                    out.extend_from_slice(word.as_bytes());
                    width += word.chars().count() * 2;
                }
                5 if rng.chance(10) => {
                    out.extend_from_slice(format!("\x1b]8;;https://example.com/{line}\x1b\\link\x1b]8;;\x1b\\").as_bytes());
                    width += 4;
                }
                _ => {
                    let word = rng.pick(&WORDS);
                    out.extend_from_slice(word.as_bytes());
                    out.push(b' ');
                    width += word.len() + 1;
                }
            }
        }
        if rng.chance(2) {
            out.extend_from_slice(b"\x1b[2A\x1b[3C\x1b[K\x1b[2B");
        }
        if rng.chance(3) {
            out.extend_from_slice(b"\x1b]133;D;0\x07");
        }
        out.extend_from_slice(b"\x1b[0m\r\n");
    }
    out
}

/// 随机字节，偏向转义序列里常见的字节，好让 VT 停在各种序列的各种位置。
pub(crate) fn random_bytes(rng: &mut Rng, len: usize) -> Vec<u8> {
    const COMMON: &[u8] = b"\x1b\x1b\x1b[[]]];;;0123456789?>=<$\" PqmhlHJKrstuABCD\x07\x08\t\n\r\\P_^Xabcxyz";
    (0..len)
        .map(|_| match rng.below(10) {
            0..=5 => *rng.pick(COMMON),
            6 => 0xE4 + rng.below(3) as u8,
            7 => 0x80 + rng.below(0x40) as u8,
            _ => rng.next() as u8,
        })
        .collect()
}

/// 录下来的真实会话：隔离了 HOME 和 SHELL 的 zsh（带 OSC 133 和标题的提示符）里跑 `ls -G`、
/// 用 vim 改文件，再用 less 翻页、搜索，80 列 24 行。
pub(crate) const RECORDED_ZSH_VIM_LESS: &[u8] = include_bytes!("../../testdata/zsh-vim-less.typescript");

#[test]
fn snapshot_round_trips_the_screen() {
    let mut a = new_terminal(size(20, 4)).unwrap();
    a.vt_write("\x1b]0;标题\x07\x1b[31mred\x1b[0m 中文\r\nsecond".as_bytes());
    let mut b = decode_snapshot(&encode_snapshot(&a).unwrap()).unwrap();
    assert_eq!(b.title().unwrap(), "标题");
    assert_eq!((b.cols().unwrap(), b.rows().unwrap()), (20, 4));
    assert_eq!((b.cursor_x().unwrap(), b.cursor_y().unwrap()), (6, 1));
    assert_eq!(format_replay(&a).unwrap(), format_replay(&b).unwrap());
    // 解出来的 VT 设好了共同的选项，也接着记录没写完的序列，能再编快照。
    assert_eq!(b.scrollback_max_lines().unwrap(), Some(SCROLLBACK_LINES));
    assert_eq!(b.continuation_max_bytes().unwrap(), CONTINUATION_MAX_BYTES);
    b.vt_write(b"\x1b]2;x");
    assert!(decode_snapshot(&encode_snapshot(&b).unwrap()).is_ok());
}

#[test]
fn an_unfinished_sequence_resumes_after_decoding() {
    // 停在 OSC、CSI、UTF-8 的中间：解出来接着喂剩下的字节，结果和一口气喂完一样。
    for (head, tail) in [
        (&b"\x1b]2;half"[..], &b" title\x07after"[..]),
        (b"abc\x1b[3", b"1mred"),
        (b"\x1b[?104", b"9halt"),
        (b"x\xe4\xb8", b"\xad\xe6\x96\x87"),
        (b"\x1b", b"[2J\x1b[Hcleared"),
    ] {
        let mut whole = new_terminal(size(20, 4)).unwrap();
        whole.vt_write(head);
        whole.vt_write(tail);
        let mut split = new_terminal(size(20, 4)).unwrap();
        split.vt_write(head);
        assert!(!split.is_vt_ground().unwrap());
        let mut resumed = decode_snapshot(&encode_snapshot(&split).unwrap()).unwrap();
        resumed.vt_write(tail);
        assert_eq!(format_replay(&resumed).unwrap(), format_replay(&whole).unwrap(), "{head:?} | {tail:?}");
    }
}

#[test]
fn a_sequence_longer_than_the_limit_waits_until_it_ends() {
    let mut terminal = new_terminal(size(20, 4)).unwrap();
    terminal.vt_write(b"\x1b]6973;");
    terminal.vt_write(&vec![b'a'; CONTINUATION_MAX_BYTES]);
    assert!(matches!(encode_snapshot(&terminal), Err(SnapshotError::Unfinished)));
    // 这条序列一结束就又能编了。
    terminal.vt_write(b"\x07done");
    let decoded = decode_snapshot(&encode_snapshot(&terminal).unwrap()).unwrap();
    assert_eq!(format_replay(&decoded).unwrap(), format_replay(&terminal).unwrap());
}

/// 乱码也会让 VT 暂时编不出快照：ESC 后面跟着 UTF-8 和 C1 字节时，续接取得到，libghostty
/// 却不肯编。和太长的序列一样，等它回到 ground 再编。
#[test]
fn malformed_input_can_block_encoding_until_ground() {
    let mut terminal = new_terminal(size(20, 4)).unwrap();
    terminal.vt_write(&[0x1b, 0xc6, 0x9f, b'[', b'r', 0x9d]);
    assert!(terminal.continuation_alloc(None).is_ok());
    assert!(matches!(encode_snapshot(&terminal), Err(SnapshotError::Unfinished)));
    terminal.vt_write(b"\x1b\\");
    assert!(terminal.is_vt_ground().unwrap());
    assert!(encode_snapshot(&terminal).is_ok());
}

#[test]
fn without_tracking_only_a_grounded_terminal_encodes() {
    let mut terminal = Terminal::new(20, 4).unwrap();
    configure_common(&mut terminal, CommonOptions::default()).unwrap();
    terminal.vt_write(b"done");
    assert!(encode_snapshot(&terminal).is_ok());
    terminal.vt_write(b"\x1b[3");
    assert!(matches!(encode_snapshot(&terminal), Err(SnapshotError::Unfinished)));
    // 开始记录时已经停在序列中间，这条序列还是编不出来；它结束后就好了。
    track_continuation(&mut terminal).unwrap();
    assert!(matches!(encode_snapshot(&terminal), Err(SnapshotError::Unfinished)));
    terminal.vt_write(b"1m\x1b[3");
    assert!(encode_snapshot(&terminal).is_ok());
}

#[test]
fn damaged_snapshots_are_rejected() {
    let mut terminal = new_terminal(size(20, 4)).unwrap();
    terminal.vt_write(b"hello");
    let mut bytes = encode_snapshot(&terminal).unwrap();
    assert!(matches!(decode_snapshot(b"not a snapshot"), Err(SnapshotError::Vt(_))));
    assert!(matches!(decode_snapshot(&bytes[..bytes.len() - 1]), Err(SnapshotError::Vt(_))));
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x40;
    assert!(matches!(decode_snapshot(&bytes), Err(SnapshotError::Vt(_))));
}

/// 重放后的 VT 按同样的字节往下走。
fn replayed(terminal: &Terminal<'_, '_>) -> Terminal<'static, 'static> {
    let mut replayed = new_terminal(size(terminal.cols().unwrap(), terminal.rows().unwrap())).unwrap();
    replayed.vt_write(&format_replay(terminal).unwrap());
    replayed
}

#[test]
fn replay_keeps_soft_wraps_and_pending_wrap() {
    let mut a = new_terminal(size(10, 4)).unwrap();
    // 第一行软折行到第二行；第三行写满，光标停在行尾等着折行。
    a.vt_write(b"0123456789abc\r\n0123456789");
    assert!(a.is_cursor_pending_wrap().unwrap());
    let mut b = replayed(&a);
    let wrapped = |t: &Terminal<'_, '_>, y| {
        t.grid_ref(Point::Active(PointCoordinate { x: 0, y })).unwrap().row().unwrap().is_wrapped().unwrap()
    };
    assert!(wrapped(&b, 0));
    assert!(!wrapped(&b, 1));
    assert!(b.is_cursor_pending_wrap().unwrap());
    // 两边接着写，折到同一个地方。
    a.vt_write(b"X");
    b.vt_write(b"X");
    assert_eq!(screen_lines(&a, 0, 3).unwrap(), screen_lines(&b, 0, 3).unwrap());
    assert_eq!((b.cursor_x().unwrap(), b.cursor_y().unwrap()), (1, 3));
}

#[test]
fn replay_keeps_modes_title_palette_and_keyboard() {
    let mut a = new_terminal(size(20, 4)).unwrap();
    a.vt_write(b"\x1b]4;1;rgb:12/34/56\x07\x1b]0;hello\x07\x1b]7;file:///tmp/x\x07\x1b[?2004h\x1b[?1002h\x1b[?1006h\x1b[>5u\x1b[5 q\x1b[3;4r\x1b[1;31mred");
    let b = replayed(&a);
    for mode in [Mode::BRACKETED_PASTE, Mode::BUTTON_MOUSE, Mode::SGR_MOUSE] {
        assert!(b.mode(mode).unwrap(), "{mode:?}");
    }
    assert_eq!(b.title().unwrap(), "hello");
    assert_eq!(b.pwd().unwrap(), "file:///tmp/x");
    assert_eq!(b.color_palette().unwrap().0[1], a.color_palette().unwrap().0[1]);
    assert_eq!(b.kitty_keyboard_flags().unwrap(), a.kitty_keyboard_flags().unwrap());
    assert_eq!(format_replay(&b).unwrap(), format_replay(&a).unwrap());
}

/// 重放丢掉的东西。快照都带着；版本对不上、只能重放时，界面上会看到这些差别。哪天这里的
/// 断言不成立了，说明格式化能带上它了，改注释和测试。
#[test]
fn replay_loses_what_the_formatter_cannot_express() {
    // 备用屏幕上只重放备用屏幕：主屏幕和回滚历史都没了，退出备用屏幕后是空的。
    let mut a = new_terminal(size(20, 4)).unwrap();
    a.vt_write(b"primary\r\n\x1b[?1049halt");
    let mut b = replayed(&a);
    a.vt_write(b"\x1b[?1049l");
    b.vt_write(b"\x1b[?1049l");
    assert_eq!(screen_lines(&a, 0, 0).unwrap(), ["primary"]);
    assert_eq!(screen_lines(&b, 0, 0).unwrap(), [""]);

    // DECSC 存的光标、单元格上的超链接（只带光标当前的那个）、OSC 10/11/12 改的颜色，以及
    // Kitty 键盘协议的栈（只带当前的标志）。
    let mut a = new_terminal(size(20, 4)).unwrap();
    a.vt_write(b"\x1b[2;5H\x1b7\x1b[H\x1b]8;;https://x\x07link\x1b]8;;\x07\x1b]11;rgb:01/02/03\x07\x1b[>1u\x1b[>3u");
    let mut b = replayed(&a);
    let uri = |t: &Terminal<'_, '_>| {
        let mut buf = [0u8; 64];
        let len = t.grid_ref(Point::Active(PointCoordinate { x: 0, y: 0 })).unwrap().hyperlink_uri(&mut buf).unwrap();
        buf[..len].to_vec()
    };
    assert_eq!(uri(&a), b"https://x");
    assert_eq!(uri(&b), b"");
    assert!(a.bg_color().unwrap().is_some());
    assert_eq!(b.bg_color().unwrap(), None);
    assert_eq!(b.kitty_keyboard_flags().unwrap(), a.kitty_keyboard_flags().unwrap());
    for t in [&mut a, &mut b] {
        t.vt_write(b"\x1b[<u\x1b8");
    }
    assert_ne!(b.kitty_keyboard_flags().unwrap(), a.kitty_keyboard_flags().unwrap());
    assert_eq!((a.cursor_x().unwrap(), a.cursor_y().unwrap()), (4, 1));
    assert_eq!((b.cursor_x().unwrap(), b.cursor_y().unwrap()), (0, 0));

    // 调色板 256 项全用 OSC 4 写出，在重放出的 VT 里都成了程序改过的颜色：之后换主题，
    // 这些颜色不再跟着变。
    let a = new_terminal(size(20, 4)).unwrap();
    let mut b = replayed(&a);
    let mut theme = b.default_color_palette().unwrap();
    theme.0[2] = libghostty_vt::style::RgbColor { r: 1, g: 2, b: 3 };
    b.set_default_color_palette(Some(theme)).unwrap();
    assert_ne!(b.color_palette().unwrap().0[2], theme.0[2]);
}

/// 第 0 阶段要汇报的几个数字。在 release 下跑才有意义：
/// `cargo test --release -p runode-terminal vt::tests::snapshot_costs -- --ignored --nocapture`
#[test]
#[ignore = "测量耗时，手动跑"]
#[allow(clippy::print_stderr, reason = "手动跑的测量，结果直接打出来看")]
fn snapshot_costs() {
    let mut rng = Rng::new(7);

    // 回滚历史按默认上限填满时、200 列的快照，分两种输出各测一次：像日志那样只有几种颜色的，
    // 和 `mixed_output` 那样颜色很杂的。200 列时先到的是字节上限，留不到 1 万行。
    let log: Vec<u8> = (0..SCROLLBACK_LINES + 200)
        .flat_map(|i| {
            let level = ["\x1b[32mINFO\x1b[0m", "\x1b[33mWARN\x1b[0m", "\x1b[1;31mERROR\x1b[0m"][i % 3];
            let text = "request served in 12ms 中文 path=/api/v1/items ".repeat(4);
            format!("2026-10-06T12:00:00Z {level} {i:6} {text}\r\n").into_bytes()
        })
        .collect();
    for (name, output) in [("log-like", log), ("mixed", mixed_output(&mut rng, SCROLLBACK_LINES + 200, 200))] {
        let mut terminal = new_terminal(size(200, 50)).unwrap();
        terminal.vt_write(&output);
        let scrollbar = terminal.scrollbar().unwrap();
        let (encode, bytes) = timed(5, || encode_snapshot(&terminal).unwrap());
        let (decode, _) = timed(5, || decode_snapshot(&bytes).unwrap());
        let replay_bytes = format_replay(&terminal).unwrap();
        eprintln!(
            "snapshot {name} 200x50, {} rows in total ({} KiB fed): encode {:.1} ms, decode {:.1} ms, {} KiB; vt replay {} KiB",
            scrollbar.total,
            output.len() / 1024,
            ms(encode),
            ms(decode),
            bytes.len() / 1024,
            replay_bytes.len() / 1024,
        );
    }

    // 喂 100 MiB 混合输出，每次 4 KiB，记录没写完的序列与否。
    let chunk = {
        let mut chunk = mixed_output(&mut rng, 4000, 120);
        chunk.extend(random_bytes(&mut rng, 4096));
        chunk.extend_from_slice(b"\x1b\\\x1b[0m\x1bc");
        chunk
    };
    let rounds = (100 << 20) / chunk.len();
    for tracking in [false, true, false, true, false, true] {
        let mut terminal = new_terminal(size(120, 40)).unwrap();
        if !tracking {
            terminal.set_continuation_max_bytes(0).unwrap();
        }
        assert_eq!(terminal.continuation_max_bytes().unwrap() > 0, tracking);
        let start = Instant::now();
        for _ in 0..rounds {
            for piece in chunk.chunks(4096) {
                terminal.vt_write(piece);
            }
        }
        let elapsed = start.elapsed();
        let mib = (rounds * chunk.len()) as f64 / f64::from(1 << 20);
        eprintln!("feed {mib:.0} MiB, tracking {tracking}: {:.0} ms, {:.0} MiB/s", ms(elapsed), mib / elapsed.as_secs_f64());
    }
}

/// 回滚历史填满时实际留下多少行、占多少内存、快照多大。各跑一个进程才量得准 RSS：
/// `cargo test --release -p runode-terminal vt::tests::scrollback_capacity_80_cols -- --ignored --exact --nocapture`
#[test]
#[ignore = "测量内存，手动跑"]
fn scrollback_capacity_80_cols() {
    scrollback_capacity(80);
}

/// 同 `scrollback_capacity_80_cols`。
#[test]
#[ignore = "测量内存，手动跑"]
fn scrollback_capacity_200_cols() {
    scrollback_capacity(200);
}

#[allow(clippy::print_stderr, reason = "手动跑的测量，结果直接打出来看")]
fn scrollback_capacity(cols: u16) {
    let max_rss = || {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: getrusage 只往传进去的结构里写。
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
        // macOS 上 ru_maxrss 的单位是字节。
        unsafe { usage.assume_init() }.ru_maxrss as f64 / f64::from(1 << 20)
    };
    let before = max_rss();
    let mut terminal = new_terminal(size(cols, 50)).unwrap();
    let fill = |terminal: &mut Terminal<'_, '_>, styled: bool| {
        for i in 0..40_000 {
            let line = if styled {
                format!("\x1b[38;5;{}m2026-10-06T12:00:00Z INFO {i:6}\x1b[0m request served in 12ms path=/api/v1/items\r\n", i % 256)
            } else {
                format!("2026-10-06T12:00:00Z INFO {i:6} request served in 12ms path=/api/v1/items\r\n")
            };
            terminal.vt_write(line.as_bytes());
        }
    };
    fill(&mut terminal, false);
    let after = max_rss();
    let resident = terminal.memory_usage().unwrap().primary_resident_bytes as f64 / f64::from(1 << 20);
    let snapshot = encode_snapshot(&terminal).unwrap().len() as f64 / f64::from(1 << 20);
    eprintln!(
        "{cols} cols plain: {} history rows + 50 on screen, resident {resident:.1} MiB, max RSS {before:.1} -> {after:.1} MiB (+{:.1}), snapshot {snapshot:.1} MiB",
        terminal.scrollback_rows().unwrap(),
        after - before,
    );
    let mut styled = new_terminal(size(cols, 50)).unwrap();
    fill(&mut styled, true);
    let resident = styled.memory_usage().unwrap().primary_resident_bytes as f64 / f64::from(1 << 20);
    eprintln!("{cols} cols 256 styles: {} history rows, resident {resident:.1} MiB", styled.scrollback_rows().unwrap());
}

fn timed<T>(runs: u32, mut f: impl FnMut() -> T) -> (Duration, T) {
    let mut result = f();
    let start = Instant::now();
    for _ in 0..runs {
        result = f();
    }
    (start.elapsed() / runs, result)
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.
}

