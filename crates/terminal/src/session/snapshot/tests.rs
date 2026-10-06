//! 差分测试：一份 VT 从头喂到尾；另一份在随机切点「编码 → 解码 → 接着喂剩下的字节」，
//! 最后两份必须一样。比的是 `Format::Vt` 的输出、每行的折行和语义标记、每个单元格的
//! 超链接和保护属性、`Frame`、各个模式、标题、目录、调色板和动态颜色、Kitty 键盘协议的标志、
//! 光标和滚动条。再往两份里喂同样的「探针」（恢复 DECSC 存的光标、走制表位、弹 Kitty 键盘
//! 协议的栈、退出备用屏幕、改尺寸等），把快照里带着、但当下看不出来的状态也翻出来比。
//!
//! 唯一允许不同的是回滚历史最老的那几行留没留着：回滚按 page 整块丢弃，page 怎么划分不在
//! 快照里，解出来的 VT 之后丢的行数可能多也可能少，见
//! `scrollback_pruning_diverges_after_a_snapshot`。所以最后只比两边都还留着的那些行。
//! 刚解完、还没接着喂的时候不一样：解出来的历史必须一行不少，见 `assert_history_survives`
//! 和 `decoding_keeps_a_full_scrollback`。

use libghostty_vt::{
    Terminal,
    fmt::Formatter,
    selection::Selection,
    terminal::{Mode, Point, PointCoordinate},
};
use runode_shared_types::settings::TermSettings;

use super::super::render::Renderer;
use crate::{
    host_session::HostSession,
    session::Session,
    testing::{row_text, unstarted_pty},
    vt::{
        self,
        tests::{RECORDED_ZSH_VIM_LESS, Rng, feed_chunked, mixed_output, random_bytes, size},
    },
};

/// 比较时读的全部模式。
const MODES: [Mode; 44] = [
    Mode::KAM,
    Mode::INSERT,
    Mode::SRM,
    Mode::LINEFEED,
    Mode::DECCKM,
    Mode::_132_COLUMN,
    Mode::SLOW_SCROLL,
    Mode::REVERSE_COLORS,
    Mode::ORIGIN,
    Mode::WRAPAROUND,
    Mode::AUTOREPEAT,
    Mode::X10_MOUSE,
    Mode::CURSOR_BLINKING,
    Mode::CURSOR_VISIBLE,
    Mode::ENABLE_MODE3,
    Mode::REVERSE_WRAP,
    Mode::ALT_SCREEN_LEGACY,
    Mode::KEYPAD_KEYS,
    Mode::BACKARROW_KEY_MODE,
    Mode::LEFT_RIGHT_MARGIN,
    Mode::NORMAL_MOUSE,
    Mode::BUTTON_MOUSE,
    Mode::ANY_MOUSE,
    Mode::FOCUS_EVENT,
    Mode::UTF8_MOUSE,
    Mode::SGR_MOUSE,
    Mode::ALT_SCROLL,
    Mode::URXVT_MOUSE,
    Mode::SGR_PIXELS_MOUSE,
    Mode::NUMLOCK_KEYPAD,
    Mode::ALT_ESC_PREFIX,
    Mode::ALT_SENDS_ESC,
    Mode::REVERSE_WRAP_EXT,
    Mode::ALT_SCREEN,
    Mode::SAVE_CURSOR,
    Mode::ALT_SCREEN_SAVE,
    Mode::BRACKETED_PASTE,
    Mode::SYNC_OUTPUT,
    Mode::GRAPHEME_CLUSTER,
    Mode::COLOR_SCHEME_REPORT,
    Mode::VISIBILITY_REPORT,
    Mode::IN_BAND_RESIZE,
    Mode::PASTE_EVENTS,
    Mode::new(1048, libghostty_vt::terminal::ModeKind::Dec),
];

/// 逐个单元格比的回滚历史最多几行，再多的只比文字，免得测试太慢。
const DETAILED_HISTORY: usize = 20;

/// VT 状态里要比的各项，按名字排好；不一样时报出是哪一项。回滚历史只看最近的 `history` 行。
///
/// 单元格的内容按解析后的样式比，不比 `Format::Vt` 输出的正文：样式表按 page 分开存，同样的
/// 样式落在不同 page 的两段上时，格式化会在中间多发一遍 SGR，字节不同、画出来一样。
/// `Format::Vt` 只格式化最后一行，用来比它带出的滚动区域、制表位、当前样式、字符集等。
fn state(terminal: &Terminal<'static, '_>, history: usize, detailed: bool) -> Vec<(&'static str, String)> {
    let mut renderer = Renderer::new().unwrap();
    renderer.refresh(terminal).unwrap();
    let total = terminal.total_rows().unwrap();
    let active = usize::from(terminal.rows().unwrap());
    let first = total - active - history;
    let cols = terminal.cols().unwrap();
    let at = |x, y: usize| Point::Screen(PointCoordinate { x, y: y as u32 });
    let mut rows = String::new();
    let detailed_rows = if detailed { total - active - history.min(DETAILED_HISTORY)..total } else { 0..0 };
    for y in detailed_rows {
        let row = terminal.grid_ref(at(0, y)).unwrap().row().unwrap();
        rows.push_str(&format!(
            "{}:{}{}{:?}",
            y - first,
            u8::from(row.is_wrapped().unwrap()),
            u8::from(row.is_wrap_continuation().unwrap()),
            row.semantic_prompt().unwrap()
        ));
        for x in 0..cols {
            let grid = terminal.grid_ref(at(x, y)).unwrap();
            let cell = grid.cell().unwrap();
            let content = cell.semantic_content().unwrap();
            if cell.is_protected().unwrap() {
                rows.push_str(&format!(" {x}:protected"));
            }
            if cell.has_styling().unwrap() {
                rows.push_str(&format!(" {x}:{:?}", grid.style().unwrap()));
            }
            if cell.has_hyperlink().unwrap() {
                let mut uri = [0u8; 256];
                let len = grid.hyperlink_uri(&mut uri).unwrap();
                rows.push_str(&format!(" {x}:{}", String::from_utf8_lossy(&uri[..len])));
            }
            rows.push_str(&format!(" {content:?}"));
        }
        rows.push('\n');
    }
    let selection = Selection::new(
        terminal.grid_ref(at(0, total - 1)).unwrap(),
        terminal.grid_ref(at(cols - 1, total - 1)).unwrap(),
        false,
    );
    let vt = Formatter::new(terminal, vt::replay_options().with_selection(&selection))
        .unwrap()
        .format_alloc(None)
        .unwrap()
        .to_vec();
    let scrollbar = terminal.scrollbar().unwrap();
    vec![
        ("vt", String::from_utf8_lossy(&vt).into_owned()),
        ("text", vt::screen_lines(terminal, first, total - 1).unwrap().join("\n")),
        ("rows", rows),
        ("frame", format!("{:?}", renderer.frame)),
        ("modes", MODES.iter().map(|&mode| u8::from(terminal.mode(mode).unwrap()).to_string()).collect()),
        ("title", terminal.title().unwrap().to_owned()),
        ("pwd", terminal.pwd().unwrap().to_owned()),
        ("palette", format!("{:?}", terminal.color_palette().unwrap().0)),
        ("default palette", format!("{:?}", terminal.default_color_palette().unwrap().0)),
        (
            "dynamic colors",
            format!(
                "{:?}",
                [
                    terminal.fg_color().unwrap(),
                    terminal.bg_color().unwrap(),
                    terminal.cursor_color().unwrap(),
                    terminal.default_fg_color().unwrap(),
                    terminal.default_bg_color().unwrap(),
                    terminal.default_cursor_color().unwrap(),
                ]
            ),
        ),
        ("kitty keyboard", format!("{:?}", terminal.kitty_keyboard_flags().unwrap())),
        (
            "cursor",
            format!(
                "{},{} pending {} visible {} pen {:?}",
                terminal.cursor_x().unwrap(),
                terminal.cursor_y().unwrap(),
                terminal.is_cursor_pending_wrap().unwrap(),
                terminal.is_cursor_visible().unwrap(),
                terminal.cursor_style().unwrap()
            ),
        ),
        ("mouse", format!("{:?} {}", terminal.mouse_shape().unwrap(), terminal.is_mouse_tracking().unwrap())),
        (
            "screen",
            format!("{:?} at prompt {}", terminal.active_screen().unwrap(), terminal.is_cursor_at_prompt().unwrap()),
        ),
        // 视口离底部多远、多高；总行数随回滚历史留了多少而变。
        ("viewport", format!("{} {}", scrollbar.total - scrollbar.offset, scrollbar.len)),
        (
            "size",
            format!(
                "{}x{} {}x{}px",
                cols,
                terminal.rows().unwrap(),
                terminal.width_px().unwrap(),
                terminal.height_px().unwrap()
            ),
        ),
    ]
}

/// 比较两份 VT。回滚历史只比两边都留着的行，再去掉最上面那条不完整的逻辑行：它的开头
/// 可能已经随 page 丢掉了，改尺寸重排后折行的位置也就不同。`detailed` 为假时不逐个单元格比，
/// 也不比回滚历史，探针之间用它，省时间。
fn assert_same(
    context: &str,
    expected: &Terminal<'static, 'static>,
    actual: &Terminal<'static, 'static>,
    detailed: bool,
) {
    let mut history = expected.scrollback_rows().unwrap().min(actual.scrollback_rows().unwrap());
    let continuation = |terminal: &Terminal<'static, 'static>, from_bottom: usize| {
        let y = terminal.total_rows().unwrap() - from_bottom;
        let at = Point::Screen(PointCoordinate { x: 0, y: y as u32 });
        terminal.grid_ref(at).unwrap().row().unwrap().is_wrap_continuation().unwrap()
    };
    let rows = usize::from(expected.rows().unwrap());
    while history > 0 && [expected, actual].into_iter().any(|t| continuation(t, rows + history)) {
        history -= 1;
    }
    if history > 0 {
        // 最上面这行是一条逻辑行的开头，但两边这条逻辑行都可能只剩后半截。
        history -= 1;
        while history > 0 && [expected, actual].into_iter().any(|t| continuation(t, rows + history)) {
            history -= 1;
        }
    }
    if !detailed {
        history = 0;
    }
    for ((name, expected), (_, actual)) in
        state(expected, history, detailed).into_iter().zip(state(actual, history, detailed))
    {
        if expected != actual {
            let at = expected
                .bytes()
                .zip(actual.bytes())
                .position(|(a, b)| a != b)
                .unwrap_or(expected.len().min(actual.len()));
            let around = |s: &str| {
                let start = s.floor_char_boundary(at.saturating_sub(200));
                let end = s.ceil_char_boundary((at + 200).min(s.len()));
                format!("{:?}", &s[start..end])
            };
            panic!(
                "{context}: {name} differs at byte {at}\nexpected {}\nactual   {}",
                around(&expected),
                around(&actual)
            );
        }
    }
}

/// 探针：两份 VT 喂同样的字节或者改同样的尺寸后还得一样，把当下看不出来的状态翻出来。
fn probe(context: &str, expected: &mut Terminal<'static, 'static>, actual: &mut Terminal<'static, 'static>) {
    assert_same(context, expected, actual, true);
    const PROBES: [&[u8]; 7] = [
        // DECRC 恢复存的光标、字符集和样式；G1 也写几个字。
        b"\x1b8Zlqk\x0elqk\x0f",
        // 制表位。
        b"\r\t\t\tT\x1b[2IU\x1b[ZV",
        // Kitty 键盘协议的栈。
        b"\x1b[<u\x1b[<u",
        // 滚动区域和左右边距。
        b"\x1b[999B\n\n\x1b[2S\x1b[3T\x1b[99Cx",
        // 回到主屏幕。
        b"\x1b[?1049l\x1b[?1047l\x1b[?47l",
        // 程序设的超链接、样式和保护模式接着用。
        b"after\r\n",
        // 清掉滚动区域，整屏往上滚，把回滚历史顶出去。
        b"\x1b[r\x1b[?69l\x1b[999B\n\n\n\n\n",
    ];
    for (i, bytes) in PROBES.iter().enumerate() {
        expected.vt_write(bytes);
        actual.vt_write(bytes);
        assert_same(&format!("{context}, probe {i}"), expected, actual, i == PROBES.len() - 1);
    }
    let (cols, rows) = (expected.cols().unwrap(), expected.rows().unwrap());
    for (cols, rows) in [(cols + 7, rows.saturating_sub(1).max(2)), (cols.saturating_sub(5).max(4), rows + 3)] {
        for terminal in [&mut *expected, &mut *actual] {
            terminal.resize(cols, rows, 8, 16).unwrap();
        }
        assert_same(&format!("{context}, resized to {cols}x{rows}"), expected, actual, true);
    }
}

/// 差分测试的一份数据：名字、尺寸和字节。
struct Sample {
    name: &'static str,
    cols: u16,
    rows: u16,
    data: Vec<u8>,
    /// 随机切点的个数，另外还有落在序列中间的切点和两头，见 `cut_points`。调试构建的
    /// libghostty 每次写入都做完整性检查，几十 KB 就要喂好几秒，大的数据少切几刀。
    cuts: usize,
    /// 回滚历史的字节上限；`None` 用默认的。
    scrollback_bytes: Option<usize>,
}

/// 各份数据；`scale` 放大随机生成的数据和切点的个数，见 `snapshots_survive_a_long_soak`。
fn samples(seed: u64, scale: usize) -> Vec<Sample> {
    let mut rng = Rng::new(seed);
    let sample = |name, cols, rows, data: &[u8]| Sample {
        name,
        cols,
        rows,
        data: data.to_vec(),
        cuts: 6 * scale,
        scrollback_bytes: None,
    };
    let mut long_osc = b"\x1b]6973;token;functions=".to_vec();
    long_osc.extend(b"fn_name ".repeat(4096));
    long_osc.extend_from_slice(b"\x07prompt$ \x1b]2;");
    long_osc.extend(b"t".repeat(3000));
    long_osc.extend_from_slice(b"\x1b\\done");
    vec![
        sample("recorded zsh, vim and less", 80, 24, RECORDED_ZSH_VIM_LESS),
        sample(
            "alternate screen",
            30,
            8,
            b"shell$ vim\r\n\x1b[?1049h\x1b[22;0;0t\x1b[1;7r\x1b[?12h\x1b[?25l\x1b[H\x1b[2J\x1b[2;1H~\x1b[3;1H~\x1b[8;1H\x1b[7m-- INSERT --\x1b[m\
              \x1b[1;1Hedited text\x1b[5;3r\x1b[5;1H\x1bD\x1bD\x1bMx\x1b[?25h\x1b[?1049l\x1b[23;0;0tshell$ less\r\n\
              \x1b[?1049h\x1b[?1h\x1b=\x1b[H\x1b[2Jpage one\r\n\x1b[7m(END)\x1b[27m\x1b[?1047l\x1b[?1047hstill alt",
        ),
        sample(
            "kitty keyboard",
            20,
            4,
            b"\x1b[>1u\x1b[>3uone\x1b[=5;2u\x1b[?u\x1b[<u\x1b[>31utwo\x1b[?1049h\x1b[>8ualt\x1b[?1049l\x1b[=4;3u\x1b[>4;2m",
        ),
        sample(
            "colors",
            20,
            4,
            b"\x1b]4;1;rgb:ff/00/00\x1b\\\x1b]4;2;#00ff00;200;rgb:01/02/03\x07\x1b]104;2\x07\x1b]10;rgb:aa/bb/cc\x07\
              \x1b]11;#102030\x07\x1b]12;rgb:ff/ff/00\x07\x1b]110\x07\x1b]10;rgb:11/22/33\x07\x1b]4;1;?\x07\x1b]11;?\x07\
              \x1b[31mred\x1b[38;5;200m256\x1b[48;2;1;2;3mrgb\x1b[0m",
        ),
        sample(
            "titles and directories",
            20,
            4,
            "\x1b]0;第一个\x07\x1b]1;icon\x07\x1b]2;second\x1b\\\x1b[22;0t\x1b]2;pushed\x07\x1b[23;0t\x1b]7;file://host/tmp/a%20b\x07\
              \x1b]2;\x07\x1b]2;✳ 修 bug\x07"
                .as_bytes(),
        ),
        sample(
            "modes",
            20,
            6,
            b"\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h\x1b[?1015h\x1b[?1016h\x1b[?1004h\x1b[?1h\x1b=\
              \x1b[?6h\x1b[2;5r\x1b[Hin region\x1b[?7lno wrap no wrap no wrap\x1b[?7h\x1b[4hins\x1b[4l\x1b[?5h\x1b[?25l\
              \x1b[5 q\x1b[?12h\x1b[?2027h\x1b[?2048h\x1b[?2031h\x1b[?1007h\x1b[?1036l\x1b[?1039h\x1b[?45h\x1b[?67h\x1b[20h\
              \x1b[?2026hheld\x1b[s",
        ),
        sample(
            "semantic prompts",
            30,
            6,
            b"\x1b]133;A;cl=m\x07~ $ \x1b]133;B\x07ls\r\n\x1b]133;C;cmdline=ls\x07a b c\r\n\x1b]133;D;0\x07\
              \x1b]133;A\x07~ $ \x1b]133;B\x07false\r\n\x1b]133;C\x07\x1b]133;D;1;aid=x\x07\x1b]133;P;k=s\x07> \x1b]133;B\x07\
              cont\x1b]133;A\x07~ $ \x1b]133;B\x07typing",
        ),
        sample("long sequences", 20, 4, &long_osc),
        sample(
            "wide characters and soft wraps",
            10,
            5,
            "0123456789abcdef\r\n123456789中文字\r\n12345678中\r\ne\u{301}ok 👨‍👩‍👧 \x1b[?2027h👨‍👩‍👧x\u{fe0f}\r\n0123456789\x1b[?7l0123\x1b[?7h"
                .as_bytes(),
        ),
        sample(
            "charsets, margins, tabs and protection",
            30,
            8,
            b"\x1b(0lqk\x1b(B\x1b)0\x0eabc\x0f\x1b*0\x1bn\x1b}xyz\x1bo\x1b|\x1b[1;31m\x1b7\x1b[0m\x1b[3;3H\x1b#8\x1b[5;5H\
              \x1bH\x1b[8G\x1bH\x1b[3g\x1b[?69h\x1b[3;20s\x1b[2;6r\x1b[1\"qprotected\x1b[0\"q\x1bVspa\x1bWepa\x1b[K\x1b[?6h\
              \x1b[Hin margins\x1b]8;id=a;https://example.com\x1b\\linked\x1b]8;;\x1b\\\x1b]8;;https://b\x07open",
        ),
        sample(
            "styles",
            40,
            4,
            b"\x1b[1;2;3;4;5;7;8;9;53mall\x1b[0m\x1b[4:3;58;2;10;20;30mcurly\x1b[59;4:0m\x1b[21mdouble\x1b[22;23;24;25;27;28;29m\
              \x1b[38:2::1:2:3mcolon\x1b[39;49m\x1b[100;97mbright\x1b[m\x1b[3$}\x1b[0$}",
        ),
        // 喂到要丢掉最老的 page 为止，见 `long_scrollback`。
        Sample {
            name: "long scrollback",
            cols: LONG_SCROLLBACK_COLS,
            rows: 6,
            data: long_scrollback(),
            cuts: scale,
            scrollback_bytes: LONG_SCROLLBACK_BYTES,
        },
        Sample {
            name: "mixed output",
            cols: 60,
            rows: 12,
            data: mixed_output(&mut rng, 100 * scale, 60),
            cuts: scale,
            scrollback_bytes: None,
        },
        Sample {
            name: "random bytes",
            cols: 30,
            rows: 8,
            data: random_bytes(&mut rng, 8 * 1024 * scale),
            cuts: 3 * scale,
            scrollback_bytes: None,
        },
    ]
}

/// 一个 page 能放的行数随列数变：200 列时不到 500 行，20 列时四千多行。用宽的终端、小一些的
/// 字节上限（默认的 10 MiB 在 200 列要喂好几千行才满，调试构建太慢），少喂一些就能让回滚
/// 历史丢掉好几个 page。字节上限不能太小，见 `decoding_drops_history_under_a_tight_byte_limit`。
const LONG_SCROLLBACK_COLS: u16 = 200;
const LONG_SCROLLBACK_BYTES: Option<usize> = Some(2_000_000);

fn long_scrollback() -> Vec<u8> {
    (0..1000).flat_map(|i| format!("line {i}\r\n").into_bytes()).collect()
}

/// 切点：`random` 个随机的位置，加上紧跟在 ESC 后面、落在序列中间的几个位置，和两头。
fn cut_points(data: &[u8], rng: &mut Rng, random: usize) -> Vec<usize> {
    let mut cuts: Vec<usize> = (0..random).map(|_| rng.below(data.len() + 1)).collect();
    let escapes: Vec<usize> = data.iter().enumerate().filter(|&(_, &b)| b == 0x1b).map(|(i, _)| i + 1).collect();
    for _ in 0..random.div_ceil(2) {
        if !escapes.is_empty() {
            cuts.push(*rng.pick(&escapes));
            // 再往后一两个字节，落在 CSI 的参数或 OSC 的内容里。
            cuts.push((*rng.pick(&escapes) + 1 + rng.below(3)).min(data.len()));
        }
    }
    cuts.extend([0, data.len()]);
    cuts.sort_unstable();
    cuts.dedup();
    cuts
}

/// 从 `terminal` 编一份快照再解出来；VT 停在一条太长的序列中间、暂时编不出时为 `None`。
fn round_trip(terminal: &Terminal<'static, '_>) -> Option<Terminal<'static, 'static>> {
    match vt::encode_snapshot(terminal) {
        Ok(bytes) => Some(vt::decode_snapshot(&bytes).unwrap()),
        Err(vt::SnapshotError::Unfinished) => None,
        Err(err) => panic!("{err}"),
    }
}

#[test]
fn snapshots_resume_exactly_where_the_terminal_was() {
    differential(1, 1);
}

/// 换更多种子、切更多刀、用更大的数据。调试构建太慢，在 release 下手动跑：
/// `cargo test --release -p runode-terminal snapshots_survive_a_long_soak -- --ignored`
#[test]
#[ignore = "要跑几分钟，手动跑"]
fn snapshots_survive_a_long_soak() {
    for seed in 2..22 {
        differential(seed, 6);
    }
}

/// 刚解出来的 VT 和编快照的那份一样：回滚历史一行不少，文字（含回滚历史）一样。
fn assert_history_survives(context: &str, source: &Terminal<'static, 'static>, decoded: &Terminal<'static, 'static>) {
    let rows = |t: &Terminal<'static, 'static>| t.scrollback_rows().unwrap();
    assert_eq!(rows(decoded), rows(source), "{context}: history rows after decoding");
    let text = |t: &Terminal<'static, 'static>| vt::screen_lines(t, 0, t.total_rows().unwrap() - 1).unwrap();
    assert_eq!(text(decoded), text(source), "{context}: text after decoding");
}

/// 编一份快照再解出来，查 `assert_history_survives`；暂时编不出时为 `None`。
fn checked_round_trip(context: &str, source: &Terminal<'static, 'static>) -> Option<Terminal<'static, 'static>> {
    let decoded = round_trip(source)?;
    assert_history_survives(context, source, &decoded);
    Some(decoded)
}

fn differential(seed: u64, scale: usize) {
    let mut rng = Rng::new(seed);
    for sample in samples(seed + 41, scale) {
        let cuts = cut_points(&sample.data, &mut rng, sample.cuts);
        let mut skipped = 0;
        for (i, &cut) in cuts.iter().enumerate() {
            let context = format!("seed {seed}, {} cut at {cut}/{}", sample.name, sample.data.len());
            let new_terminal = || {
                let mut terminal = vt::new_terminal(size(sample.cols, sample.rows)).unwrap();
                if sample.scrollback_bytes.is_some() {
                    let options = vt::CommonOptions { scrollback_bytes: sample.scrollback_bytes };
                    vt::configure_common(&mut terminal, options).unwrap();
                }
                terminal
            };
            let mut expected = new_terminal();
            feed_chunked(&mut expected, &sample.data, &mut rng, 512);

            let mut actual = new_terminal();
            feed_chunked(&mut actual, &sample.data[..cut], &mut rng, 512);
            // 暂时编不出快照（`SnapshotError::Unfinished`）时，宿主会等下一批输出再编，这里换个切点。
            let Some(mut actual) = checked_round_trip(&context, &actual) else {
                skipped += 1;
                continue;
            };
            let mut rest = &sample.data[cut..];
            // 每隔一个切点再在解出来的 VT 上编一次：宿主交接时就是这样。
            if i % 2 == 1 && !rest.is_empty() {
                let second = rng.below(rest.len());
                feed_chunked(&mut actual, &rest[..second], &mut rng, 512);
                if let Some(again) = checked_round_trip(&context, &actual) {
                    actual = again;
                }
                rest = &rest[second..];
            }
            feed_chunked(&mut actual, rest, &mut rng, 512);
            probe(&context, &mut expected, &mut actual);
        }
        assert!(skipped * 2 <= cuts.len(), "seed {seed}, {}: skipped {skipped} of {} cuts", sample.name, cuts.len());
    }
}

/// 回滚历史填满了，解码也一行不丢，解出来的 VT 还是同样的上限。默认的 10 MiB 在调试构建里
/// 要喂太久才满，这里用 2 MB 的字节上限；默认上限下没填满的情形由差分测试逐个切点查。
#[test]
fn decoding_keeps_a_full_scrollback() {
    let mut source = vt::new_terminal(size(LONG_SCROLLBACK_COLS, 50)).unwrap();
    vt::configure_common(&mut source, vt::CommonOptions { scrollback_bytes: LONG_SCROLLBACK_BYTES }).unwrap();
    for i in 0..1500 {
        source.vt_write(format!("\x1b[38;5;{}mline {i} with some text after it\x1b[0m\r\n", i % 256).as_bytes());
    }
    let decoded = round_trip(&source).unwrap();
    assert!(source.scrollback_rows().unwrap() < 1450, "the byte limit should have pruned some history");
    assert_history_survives("full", &source, &decoded);
    assert_eq!(decoded.scrollback_max_bytes().unwrap(), LONG_SCROLLBACK_BYTES);
    assert_eq!(decoded.scrollback_max_lines().unwrap(), Some(vt::SCROLLBACK_LINES));
}

/// 新建的 VT 用默认的上限，解码出来的保留编快照那份的上限。
#[test]
fn decoding_keeps_the_scrollback_limits() {
    let mut source = vt::new_terminal(size(20, 4)).unwrap();
    assert_eq!(source.scrollback_max_bytes().unwrap(), Some(runode_shared_types::settings::DEFAULT_SCROLLBACK_LIMIT));
    vt::configure_common(&mut source, vt::CommonOptions { scrollback_bytes: Some(3_000_000) }).unwrap();
    let decoded = round_trip(&source).unwrap();
    assert_eq!(decoded.scrollback_max_bytes().unwrap(), Some(3_000_000));
    assert_eq!(decoded.scrollback_max_lines().unwrap(), Some(vt::SCROLLBACK_LINES));
}

/// 字节上限很紧（不到三个 page，约 1.2 MB；比如 libghostty 自己默认的 10000 字节）时，解码会
/// 丢掉一大截回滚历史：80 列、700 行带样式的输出，651 行历史解出来只剩几十行，多半是解码后
/// 按字节上限又剪了一遍。默认的 10 MiB 下不会这样，见 `decoding_keeps_a_full_scrollback`。
/// 哪天这里不成立了，说明 libghostty 修好了，删掉这个测试。
#[test]
fn decoding_drops_history_under_a_tight_byte_limit() {
    let mut source = vt::new_terminal(size(80, 50)).unwrap();
    source.set_scrollback_max_bytes(Some(10_000)).unwrap();
    for i in 0..700 {
        source.vt_write(format!("\x1b[38;5;{}mline {i}\x1b[0m\r\n", i % 256).as_bytes());
    }
    let decoded = round_trip(&source).unwrap();
    let (before, after) = (source.scrollback_rows().unwrap(), decoded.scrollback_rows().unwrap());
    assert!(after < before / 2, "{after} of {before} history rows survived");
}

/// 光标不在最后一列却等着折行时编的快照，解出来的 VT 也等着折行，下一个字照样折到下一行。
/// 写到右边距（DECSLRM 设的，不是最后一列）上会这样；之后关掉左右边距模式也还等着。DECSC
/// 存的光标不管停在哪一列都原样存着，DECRC 恢复时也原样恢复，即使中间改过边距；1049 进出
/// 备用屏幕时存取主屏幕的光标也一样。
#[test]
fn pending_wrap_off_the_last_column_survives() {
    const MARGINS: &[u8] = b"\x1b[?69h\x1b[3;20s\x1b[1;20Hi";
    // 第二行是接着写的 XYZ，折到当时的左边距上；关掉左右边距模式后左边距回到第一列。
    for (name, head, tail, wrapped) in [
        ("cursor", &b""[..], &b""[..], "  XYZ"),
        ("margin mode reset", b"\x1b[?69l", b"", "XYZ"),
        ("saved cursor", b"\x1b7\x1b[3;3H", b"\x1b8", "  XYZ"),
        ("saved cursor, margins changed", b"\x1b7\x1b[3;25s", b"\x1b8", "  XYZ"),
        ("alternate screen", b"\x1b[?1049h", b"\x1b[?1049l", "  XYZ"),
        ("alternate screen, margin mode reset", b"\x1b[?1049h\x1b[?69l", b"\x1b[?1049l", "XYZ"),
    ] {
        let mut original = vt::new_terminal(size(30, 4)).unwrap();
        original.vt_write(MARGINS);
        assert!(original.is_cursor_pending_wrap().unwrap());
        original.vt_write(head);
        let mut decoded = round_trip(&original).unwrap();
        assert_same(name, &original, &decoded, true);
        for terminal in [&mut original, &mut decoded] {
            terminal.vt_write(tail);
            terminal.vt_write(b"XYZ");
        }
        assert_eq!(vt::screen_lines(&decoded, 0, 1).unwrap(), ["                   i", wrapped], "{name}");
        probe(name, &mut original, &mut decoded);
    }

    // 左边距不在第一列：停在右边距上等着折行，折到下一行的左边距上。
    let mut original = vt::new_terminal(size(30, 4)).unwrap();
    original.vt_write(b"\x1b[?69h\x1b[3;5s\x1b[1;3Habc");
    assert!(original.is_cursor_pending_wrap().unwrap());
    let mut decoded = round_trip(&original).unwrap();
    assert_same("left margin", &original, &decoded, true);
    for terminal in [&mut original, &mut decoded] {
        terminal.vt_write(b"XY");
    }
    assert_eq!(vt::screen_lines(&decoded, 0, 1).unwrap(), ["  abc", "  XY"]);
    probe("left margin", &mut original, &mut decoded);
}

/// 停在各种序列的每一个字节后面编快照：都编得出，解出来的 VT 停在同一个地方（是否在 ground），
/// 不用等回到 ground 就能再编（宿主交接时就是这样），接着喂剩下的字节后和一口气喂完的那份
/// 一样。8 位 C1 字节开头的序列也编得出，见 `vt::tests::a_c1_control_ending_a_string_still_encodes`。
#[test]
fn a_snapshot_cut_inside_any_sequence_resumes() {
    const SEQUENCES: [&[u8]; 26] = [
        // CSI：带冒号的 SGR、私有模式、带中间字节的、中间夹着 C0 控制字符和 CAN 的。
        b"\x1b[1;38:2::10:20:30mrgb",
        b"\x1b[?2004h\x1b[?1049h",
        b"\x1b[5 q\x1b[3;20s",
        b"\x1b[2\x07;4H\x1b[3\x08;1H",
        b"\x1b[31\x18m",
        b"\x1b[?12$p\x1b[>q",
        // ESC：字符集、存光标、DECALN、反向换行。
        b"\x1b(0lqk\x1b(B\x1b7\x1b#8\x1bM",
        // OSC：BEL 和 ST 结尾、超链接、语义标记、中文标题。
        "\x1b]2;中文标题\x07".as_bytes(),
        b"\x1b]8;id=a;https://example.com\x1b\\link\x1b]8;;\x1b\\",
        b"\x1b]133;A;cl=m\x07$ \x1b]133;B\x07",
        b"\x1b]4;1;rgb:12/34/56\x1b\\\x1b]11;?\x07",
        // DCS：DECRQSS、XTGETTCAP。
        b"\x1bP$qm\x1b\\\x1bP+q544e\x1b\\",
        // APC：Kitty 图片协议的查询。
        b"\x1b_Ga=q,i=1;AAAA\x1b\\",
        // SOS、PM。
        b"\x1bXsos\x1b\\\x1b^pm\x1b\\",
        // UTF-8：宽字符、emoji 序列、组合字符，以及开了字形簇模式后的。
        "中文👨‍👩‍👧e\u{301}".as_bytes(),
        "\x1b[?2027h👨‍👩‍👧x\u{fe0f}".as_bytes(),
        // 序列中间断开的 UTF-8。
        "\x1b]2;标\x07".as_bytes(),
        // ESC 后面跟着 8 位 C1 引导字节。
        b"\x1b\x9b31mred",
        b"\x1b\x9d2;c1\x07",
        // 8 位 C1 字节结束 SOS、PM、APC，开始 DCS、OSC、CSI。
        b"\x1bXsos\x90$qm\x1b\\",
        b"\x1b^pm\x9d2;c1\x07",
        b"\x1b_Gx\x9b31mred",
        // 左右边距里写到右边距上等着折行，存光标（DECSC 和 1049）后再恢复，中间改边距、关掉
        // 左右边距模式。
        b"\x1b[?69h\x1b[1;6sabcdef\x1b7\x1b[Hx\x1b8\x1b7\x1b[1;9s\x1b8\x1b[?1049h\x1b[?69lz\x1b[?1049ly",
        // Kitty 键盘协议的栈、XTWINOPS 存取标题。
        b"\x1b[>1u\x1b[>3u\x1b[<u",
        b"\x1b]2;a\x07\x1b[22;0t\x1b]2;b\x07\x1b[23;0t",
        // 软折行处停在序列中间。
        b"0123456789\x1b[1mabc",
    ];
    for sequence in SEQUENCES {
        let mut data = b"before ".to_vec();
        data.extend_from_slice(sequence);
        data.extend_from_slice(b" after\r\n");
        let start = "before ".len();
        for cut in start..=start + sequence.len() {
            let context = format!("{:?} cut at {cut}", String::from_utf8_lossy(sequence));
            let mut expected = vt::new_terminal(size(12, 4)).unwrap();
            expected.vt_write(&data);
            let mut source = vt::new_terminal(size(12, 4)).unwrap();
            source.vt_write(&data[..cut]);
            let decoded = round_trip(&source).unwrap_or_else(|| panic!("{context}: cannot encode"));
            assert_eq!(decoded.is_vt_ground().unwrap(), source.is_vt_ground().unwrap(), "{context}");
            assert_history_survives(&context, &source, &decoded);
            // 解出来的 VT 马上再编一次，不等回到 ground。
            let mut actual = round_trip(&decoded).unwrap_or_else(|| panic!("{context}: cannot encode again"));
            actual.vt_write(&data[cut..]);
            assert!(actual.is_vt_ground().unwrap(), "{context}");
            probe(&context, &mut expected, &mut actual);
        }
    }
}

/// 宿主那边的会话，接在没有 shell 的伪终端上，测试里只喂字节。
fn host(cols: u16, rows: u16) -> HostSession {
    HostSession::new(size(cols, rows), unstarted_pty(cols, rows), None, &TermSettings::default()).unwrap()
}

/// 用宿主那边的快照建界面这边的会话，就像界面连上宿主时那样。
fn session_from(source: &HostSession) -> Session {
    Session::from_snapshot(&source.snapshot().unwrap(), Box::new(|_| {})).unwrap()
}

fn rows(session: &mut Session) -> Vec<String> {
    let frame = session.frame().clone();
    (0..frame.rows).map(|y| row_text(&frame, y)).collect()
}

/// 界面从宿主的快照接着喂之后的输出，和一直喂到底的界面一模一样。
#[test]
fn a_session_resumes_from_a_snapshot() {
    let (cols, rows_count) = (80, 24);
    let mut source = host(cols, rows_count);
    let mut original = Session::new(size(cols, rows_count), &TermSettings::default(), Box::new(|_| {})).unwrap();
    let cut = RECORDED_ZSH_VIM_LESS.len() / 2;
    source.feed(&RECORDED_ZSH_VIM_LESS[..cut]);
    original.feed(&RECORDED_ZSH_VIM_LESS[..cut]);
    // 快照里带着默认颜色和光标样式，这里不再 `apply_theme`：它会改 VT 的状态（比如光标还跟着
    // 默认样式时换掉形状），只在宿主标出的位置套，见 `vt::apply_theme`。
    let mut resumed = session_from(&source);
    assert_eq!(resumed.size(), size(cols, rows_count));
    for session in [&mut original, &mut resumed] {
        session.feed(&RECORDED_ZSH_VIM_LESS[cut..]);
    }
    source.feed(&RECORDED_ZSH_VIM_LESS[cut..]);
    assert_same("session", &original.terminal, &resumed.terminal, true);
    assert_same("host", source.terminal(), &resumed.terminal, true);
    assert_eq!(rows(&mut resumed), rows(&mut original));
    assert_eq!(format!("{:?}", resumed.frame().cursor), format!("{:?}", original.frame().cursor));
}

/// 套主题不冲掉程序用 `CSI ? 12 h/l`（DEC 模式 12）设的光标闪烁，程序没动过闪烁时跟着配置走。
/// 宿主那份和从它的快照解出来的界面那份在同一个位置套同样的几个主题，每套一个都比一遍，
/// 最后再探一遍：两份一样，闪烁也是期望的那样。
#[test]
fn a_theme_applied_after_a_snapshot_keeps_the_programs_cursor_blinking() {
    use runode_shared_types::settings::CursorStyle;
    let steady_bar =
        TermSettings { cursor_style: CursorStyle::Bar, cursor_blink: Some(false), ..TermSettings::default() };
    let blinking_underline =
        TermSettings { cursor_style: CursorStyle::Underline, cursor_blink: Some(true), ..TermSettings::default() };
    let blinking_block = TermSettings { cursor_blink: Some(true), ..TermSettings::default() };
    let themes = [blinking_block, steady_bar.clone(), blinking_underline, steady_bar.clone()];
    // 起始主题、程序输出、依次套完 `themes` 后各自的闪烁。
    let cases: [(&TermSettings, &[u8], [bool; 4]); 7] = [
        // 程序没动过闪烁：跟着配置。
        (&themes[0], b"", [true, false, true, false]),
        // 程序关掉了闪烁：一直关着，直到它和默认值一样（这时分不出是谁设的），之后随配置变。
        (&themes[0], b"\x1b[?12l", [false, false, true, false]),
        // 默认不闪时程序打开了闪烁：开着，直到默认值也是闪的，之后随配置变。
        (&steady_bar, b"\x1b[?12h", [true, false, true, false]),
        // 程序用 DECSCUSR 设过形状：闪烁归程序，配置怎么变都不动。
        (&themes[0], b"\x1b[2 q", [false, false, false, false]),
        (&themes[0], b"\x1b[2 q\x1b[?12h", [true, true, true, true]),
        // 程序回到默认形状后又跟着配置。
        (&themes[0], b"\x1b[2 q\x1b[0 q", [true, false, true, false]),
        // 备用屏幕上一样。
        (&themes[0], b"\x1b[?1049h\x1b[?12l", [false, false, true, false]),
    ];
    for (start, output, expected) in cases {
        let context = format!("{output:?} from {:?}", start.cursor_style);
        let mut original = vt::new_terminal(size(20, 4)).unwrap();
        vt::apply_theme(&mut original, start);
        original.vt_write(output);
        let mut decoded = round_trip(&original).unwrap();
        assert_same(&context, &original, &decoded, true);
        for (i, (theme, blinking)) in themes.iter().zip(expected).enumerate() {
            vt::apply_theme(&mut original, theme);
            vt::apply_theme(&mut decoded, theme);
            let context = format!("{context}, theme {i}");
            assert_same(&context, &original, &decoded, true);
            assert_eq!(original.mode(Mode::CURSOR_BLINKING).unwrap(), blinking, "{context}");
        }
        probe(&context, &mut original, &mut decoded);
    }
}

/// 界面这边的会话从快照建好后套主题，和宿主那份一样，程序打开的闪烁也还开着。
#[test]
fn a_session_from_a_snapshot_applies_the_theme_like_the_host() {
    use runode_shared_types::settings::CursorStyle;
    let mut source = host(20, 4);
    source.feed(b"\x1b[?12h");
    let mut resumed = session_from(&source);
    let theme = TermSettings { cursor_style: CursorStyle::Bar, cursor_blink: Some(false), ..TermSettings::default() };
    assert!(source.apply_theme(&theme));
    resumed.apply_theme(&theme);
    assert!(resumed.terminal.mode(Mode::CURSOR_BLINKING).unwrap());
    assert_same("theme", source.terminal(), &resumed.terminal, true);
    assert_eq!(resumed.frame().cursor.map(|c| c.blinking), Some(true));
}

/// 回滚历史按 page 整块丢弃，page 的划分不在快照里：解出来的 VT 之后丢掉的行数和原来那份
/// 不一样，两份留着的历史行数就不同了，最近的那些行和屏幕上的内容仍然一样。所以宿主和界面
/// 之间不能用回滚历史里的绝对行号指同一行。
#[test]
fn scrollback_pruning_diverges_after_a_snapshot() {
    let data = long_scrollback();
    // 切在哪里会分叉要看 page 怎么划分；这个切点上，原来那份最后留 757 行历史，解出来的留 885 行。
    let cut = data.len() / 10;
    let new_terminal = || {
        let mut terminal = vt::new_terminal(size(LONG_SCROLLBACK_COLS, 6)).unwrap();
        vt::configure_common(&mut terminal, vt::CommonOptions { scrollback_bytes: LONG_SCROLLBACK_BYTES }).unwrap();
        terminal
    };
    let mut expected = new_terminal();
    expected.vt_write(&data);
    let mut actual = new_terminal();
    actual.vt_write(&data[..cut]);
    let mut actual = round_trip(&actual).unwrap();
    actual.vt_write(&data[cut..]);
    assert_ne!(expected.scrollback_rows().unwrap(), actual.scrollback_rows().unwrap());
    assert_same("pruned", &expected, &actual, true);
}
