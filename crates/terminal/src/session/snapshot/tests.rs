//! 差分测试：一份 VT 从头喂到尾；另一份在随机切点「编码 → 解码 → 接着喂剩下的字节」，
//! 最后两份必须一样。比的是 `Format::Vt` 的输出、每行的折行和语义标记、每个单元格的
//! 超链接和保护属性、`Frame`、各个模式、标题、目录、调色板和动态颜色、Kitty 键盘协议的标志、
//! 光标和滚动条。再往两份里喂同样的「探针」（恢复 DECSC 存的光标、走制表位、弹 Kitty 键盘
//! 协议的栈、退出备用屏幕、改尺寸等），把快照里带着、但当下看不出来的状态也翻出来比。
//!
//! 唯一允许不同的是回滚历史最老的那几行留没留着：回滚按 page 整块丢弃，page 怎么划分不在
//! 快照里，解出来的 VT 之后丢的行数可能多也可能少，见
//! `scrollback_pruning_diverges_after_a_snapshot`。所以最后只比两边都还留着的那些行。
//! 刚解完、还没接着喂的时候不一样：解出来的历史必须一行不少，见 `assert_history_survives`。

use libghostty_vt::{
    Terminal,
    fmt::Formatter,
    selection::Selection,
    terminal::{Mode, Point, PointCoordinate},
};
use runode_shared_types::{color::Rgb, settings::TermSettings};

use super::super::{render::Renderer, testing::row_text};
use crate::{
    pty::Pty,
    session::Session,
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
    let vt = Formatter::new(terminal, vt::replay_options().with_selection(&selection)).unwrap().format_alloc(None).unwrap().to_vec();
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
        ("screen", format!("{:?} at prompt {}", terminal.active_screen().unwrap(), terminal.is_cursor_at_prompt().unwrap())),
        // 视口离底部多远、多高；总行数随回滚历史留了多少而变。
        ("viewport", format!("{} {}", scrollbar.total - scrollbar.offset, scrollbar.len)),
        ("size", format!("{}x{} {}x{}px", cols, terminal.rows().unwrap(), terminal.width_px().unwrap(), terminal.height_px().unwrap())),
    ]
}

/// 比较两份 VT。回滚历史只比两边都留着的行，再去掉最上面那条不完整的逻辑行：它的开头
/// 可能已经随 page 丢掉了，改尺寸重排后折行的位置也就不同。`detailed` 为假时不逐个单元格比，
/// 也不比回滚历史，探针之间用它，省时间。
fn assert_same(context: &str, expected: &Terminal<'static, 'static>, actual: &Terminal<'static, 'static>, detailed: bool) {
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
    for ((name, expected), (_, actual)) in state(expected, history, detailed).into_iter().zip(state(actual, history, detailed)) {
        if expected != actual {
            let at = expected.bytes().zip(actual.bytes()).position(|(a, b)| a != b).unwrap_or(expected.len().min(actual.len()));
            let around = |s: &str| {
                let start = s.floor_char_boundary(at.saturating_sub(200));
                let end = s.ceil_char_boundary((at + 200).min(s.len()));
                format!("{:?}", &s[start..end])
            };
            panic!("{context}: {name} differs at byte {at}\nexpected {}\nactual   {}", around(&expected), around(&actual));
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
}

/// 各份数据；`scale` 放大随机生成的数据和切点的个数，见 `snapshots_survive_a_long_soak`。
fn samples(seed: u64, scale: usize) -> Vec<Sample> {
    let mut rng = Rng::new(seed);
    let sample = |name, cols, rows, data: &[u8]| Sample { name, cols, rows, data: data.to_vec(), cuts: 6 * scale };
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
        Sample { name: "long scrollback", cols: LONG_SCROLLBACK_COLS, rows: 6, data: long_scrollback(), cuts: scale },
        Sample { name: "mixed output", cols: 60, rows: 12, data: mixed_output(&mut rng, 100 * scale, 60), cuts: scale },
        Sample { name: "random bytes", cols: 30, rows: 8, data: random_bytes(&mut rng, 8 * 1024 * scale), cuts: 3 * scale },
    ]
}

/// 一个 page 能放的行数随列数变：200 列时不到 500 行，20 列时四千多行。用宽的终端，少喂
/// 一些就能让回滚历史丢掉好几个 page。
const LONG_SCROLLBACK_COLS: u16 = 200;

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

/// 回滚历史的上限按 app 现在的配置：行数上限加 libghostty 默认的字节上限。字节上限下解码
/// 会丢历史（见 `decoding_drops_history_under_the_default_byte_limit`），所以这里不查
/// `assert_history_survives`。
#[test]
fn snapshots_resume_exactly_where_the_terminal_was() {
    differential(1, 1, Limits::Default);
}

/// 去掉字节上限，只按行数上限：解出来的回滚历史必须一行不少。
#[test]
fn snapshots_keep_all_history_without_a_byte_limit() {
    differential(1, 1, Limits::LinesOnly);
}

/// 换更多种子、切更多刀、用更大的数据。调试构建太慢，在 release 下手动跑：
/// `cargo test --release -p runode-terminal snapshots_survive_a_long_soak -- --ignored`
#[test]
#[ignore = "要跑几分钟，手动跑"]
fn snapshots_survive_a_long_soak() {
    for seed in 2..22 {
        differential(seed, 6, Limits::Default);
        differential(seed, 6, Limits::LinesOnly);
    }
}

/// 差分测试里回滚历史的上限。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Limits {
    /// `vt::new_terminal` 设的，即 app 现在用的。
    Default,
    /// 去掉字节上限。
    LinesOnly,
}

fn new_terminal(cols: u16, rows: u16, limits: Limits) -> Terminal<'static, 'static> {
    let mut terminal = vt::new_terminal(size(cols, rows)).unwrap();
    if limits == Limits::LinesOnly {
        terminal.set_scrollback_max_bytes(None).unwrap();
    }
    terminal
}

/// 刚解出来的 VT 和编快照的那份一样：回滚历史一行不少，文字（含回滚历史）一样。
fn assert_history_survives(context: &str, source: &Terminal<'static, 'static>, decoded: &Terminal<'static, 'static>) {
    let rows = |t: &Terminal<'static, 'static>| t.scrollback_rows().unwrap();
    assert_eq!(rows(decoded), rows(source), "{context}: history rows after decoding");
    let text = |t: &Terminal<'static, 'static>| vt::screen_lines(t, 0, t.total_rows().unwrap() - 1).unwrap();
    assert_eq!(text(decoded), text(source), "{context}: text after decoding");
}

/// 编一份快照再解出来，`limits` 为 `LinesOnly` 时查 `assert_history_survives`；暂时编不出
/// 或者会丢 pending wrap 时为 `None`。
fn checked_round_trip(context: &str, source: &Terminal<'static, 'static>, limits: Limits) -> Option<Terminal<'static, 'static>> {
    if loses_pending_wrap(source) {
        return None;
    }
    let decoded = round_trip(source)?;
    if limits == Limits::LinesOnly {
        assert_history_survives(context, source, &decoded);
    }
    Some(decoded)
}

fn differential(seed: u64, scale: usize, limits: Limits) {
    let mut rng = Rng::new(seed);
    for sample in samples(seed + 41, scale) {
        let cuts = cut_points(&sample.data, &mut rng, sample.cuts);
        let mut skipped = 0;
        for (i, &cut) in cuts.iter().enumerate() {
            let context = format!("seed {seed}, {} cut at {cut}/{}", sample.name, sample.data.len());
            let mut expected = new_terminal(sample.cols, sample.rows, limits);
            feed_chunked(&mut expected, &sample.data, &mut rng, 512);

            let mut actual = new_terminal(sample.cols, sample.rows, limits);
            feed_chunked(&mut actual, &sample.data[..cut], &mut rng, 512);
            // 暂时编不出快照（`SnapshotError::Unfinished`）时，宿主会等下一批输出再编，这里换个切点。
            let Some(mut actual) = checked_round_trip(&context, &actual, limits) else {
                skipped += 1;
                continue;
            };
            let mut rest = &sample.data[cut..];
            // 每隔一个切点再在解出来的 VT 上编一次：宿主交接时就是这样。
            if i % 2 == 1 && !rest.is_empty() {
                let second = rng.below(rest.len());
                feed_chunked(&mut actual, &rest[..second], &mut rng, 512);
                if let Some(again) = checked_round_trip(&context, &actual, limits) {
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

/// 快照漏带的状态：libghostty 默认的回滚字节上限下，解出来的 VT 的回滚历史比编快照的那份
/// 少一大截（80 列、喂 700 行时 651 行只剩几十行），多半是解码时按字节上限又剪了一遍。没有
/// 字节上限时一行不少，见 `snapshots_keep_all_history_without_a_byte_limit`。哪天这里不成立
/// 了，删掉这个测试，让 `Limits::Default` 也查 `assert_history_survives`。
#[test]
fn decoding_drops_history_under_the_default_byte_limit() {
    let output: Vec<u8> = (0..700).flat_map(|i| format!("\x1b[38;5;{}mline {i}\x1b[0m\r\n", i % 256).into_bytes()).collect();
    for limits in [Limits::Default, Limits::LinesOnly] {
        let mut source = new_terminal(80, 50, limits);
        source.vt_write(&output);
        let decoded = round_trip(&source).unwrap();
        let (before, after) = (source.scrollback_rows().unwrap(), decoded.scrollback_rows().unwrap());
        if limits == Limits::Default {
            assert!(after < before / 2, "{after} of {before} history rows survived");
        } else {
            assert_history_survives("lines only", &source, &decoded);
        }
    }
}

/// 光标停在右边距（不是最后一列）上等着折行：快照丢掉这个状态，见
/// `pending_wrap_at_a_right_margin_is_lost`。差分测试不在这时候编快照。
fn loses_pending_wrap(terminal: &Terminal<'static, 'static>) -> bool {
    terminal.is_cursor_pending_wrap().unwrap() && terminal.cursor_x().unwrap() + 1 != terminal.cols().unwrap()
}

/// 快照漏带的状态：用 DECSLRM 设了左右边距、光标写到右边距上等着折行时，解出来的 VT 不再
/// 等着折行，下一个字写在边距的最后一格上，而不是折到下一行。光标在最后一列等着折行时没有
/// 这个问题。哪天这里不成立了，说明 libghostty 修好了，删掉这个测试和 `loses_pending_wrap`。
#[test]
fn pending_wrap_at_a_right_margin_is_lost() {
    let mut original = vt::new_terminal(size(30, 4)).unwrap();
    original.vt_write(b"\x1b[?69h\x1b[3;20s\x1b[1;20Hi");
    assert!(original.is_cursor_pending_wrap().unwrap());
    let mut decoded = round_trip(&original).unwrap();
    assert!(!decoded.is_cursor_pending_wrap().unwrap());
    original.vt_write(b"XYZ");
    decoded.vt_write(b"XYZ");
    assert_eq!(vt::screen_lines(&original, 0, 1).unwrap(), ["                   i", "  XYZ"]);
    assert_eq!(vt::screen_lines(&decoded, 0, 1).unwrap(), ["                   X", "  YZ"]);
}

/// 一个没有 shell 的会话：`from_snapshot` 要一个 PTY，测试里只喂字节。
fn unstarted_pty(cols: u16, rows: u16) -> Pty {
    Pty::open(size(cols, rows)).unwrap().0
}

fn session_from(source: &Session) -> Session {
    let size = source.size.get();
    Session::from_snapshot(&source.snapshot().unwrap(), unstarted_pty(size.cols, size.rows)).unwrap()
}

fn rows(session: &mut Session) -> Vec<String> {
    let frame = session.frame().clone();
    (0..frame.rows).map(|y| row_text(&frame, y)).collect()
}

#[test]
fn a_session_resumes_from_a_snapshot() {
    let (cols, rows_count) = (80, 24);
    let mut original = Session::with_pty(size(cols, rows_count), unstarted_pty(cols, rows_count)).unwrap();
    original.apply_config(&TermSettings::default());
    let cut = RECORDED_ZSH_VIM_LESS.len() / 2;
    original.feed(&RECORDED_ZSH_VIM_LESS[..cut]);
    // 快照里带着默认颜色和光标样式，这里不再 `apply_config`：它会改 VT 的状态，见
    // `apply_config_turns_cursor_blinking_back_on`。
    let mut resumed = session_from(&original);
    assert_eq!(resumed.size.get(), size(cols, rows_count));
    assert_eq!(resumed.title, original.title);
    for session in [&mut original, &mut resumed] {
        session.feed(&RECORDED_ZSH_VIM_LESS[cut..]);
    }
    assert_same("session", &original.terminal, &resumed.terminal, true);
    assert_eq!(rows(&mut resumed), rows(&mut original));
    assert_eq!(resumed.title, original.title);
    assert_eq!(format!("{:?}", resumed.frame().cursor), format!("{:?}", original.frame().cursor));
}

/// `apply_config` 不只改默认值：光标样式是默认的时候，重设默认闪烁会把程序用 `CSI ? 12 l`
/// 关掉的闪烁（DEC 模式 12）又打开。所以同一份配置在两份 VT 上应用的时机不同，两份就分叉了；
/// 从快照建会话后再 `apply_config`，界面这份就和宿主那份不一样。
#[test]
fn apply_config_turns_cursor_blinking_back_on() {
    let mut session = Session::with_pty(size(20, 4), unstarted_pty(20, 4)).unwrap();
    session.apply_config(&TermSettings::default());
    session.feed(b"\x1b[?12l");
    assert!(!session.terminal.mode(Mode::CURSOR_BLINKING).unwrap());
    session.apply_config(&TermSettings::default());
    assert!(session.terminal.mode(Mode::CURSOR_BLINKING).unwrap());
    // 程序用 DECSCUSR 设过样式时不受影响。
    session.feed(b"\x1b[?12l\x1b[2 q");
    session.apply_config(&TermSettings::default());
    assert!(!session.terminal.mode(Mode::CURSOR_BLINKING).unwrap());
}

/// 回滚历史按 page 整块丢弃，page 的划分不在快照里：解出来的 VT 之后丢掉的行数和原来那份
/// 不一样，两份留着的历史行数就不同了，最近的那些行和屏幕上的内容仍然一样。所以宿主和界面
/// 之间不能用回滚历史里的绝对行号指同一行。
#[test]
fn scrollback_pruning_diverges_after_a_snapshot() {
    let data = long_scrollback();
    let cut = data.len() / 3;
    let mut expected = vt::new_terminal(size(LONG_SCROLLBACK_COLS, 6)).unwrap();
    expected.vt_write(&data);
    let mut actual = vt::new_terminal(size(LONG_SCROLLBACK_COLS, 6)).unwrap();
    actual.vt_write(&data[..cut]);
    let mut actual = round_trip(&actual).unwrap();
    actual.vt_write(&data[cut..]);
    assert_ne!(expected.scrollback_rows().unwrap(), actual.scrollback_rows().unwrap());
    assert_same("pruned", &expected, &actual, true);
}

#[test]
fn a_title_from_the_snapshot_goes_through_agent_detection() {
    let mut original = Session::with_pty(size(40, 6), unstarted_pty(40, 6)).unwrap();
    original.feed("\x1b]0;✳ 修 bug\x07".as_bytes());
    let resumed = session_from(&original);
    assert_eq!(resumed.title.as_deref(), Some("修 bug"));
    assert_eq!(resumed.agent, original.agent);
}

#[test]
fn colors_set_by_programs_survive_apply_config() {
    let mut original = Session::with_pty(size(20, 4), unstarted_pty(20, 4)).unwrap();
    original.apply_config(&TermSettings::default());
    original.feed(b"\x1b]4;1;rgb:12/34/56\x07\x1b]11;rgb:01/02/03\x07");
    let mut resumed = session_from(&original);
    let theme = |seed: u8| TermSettings {
        background: Rgb(seed, seed, seed),
        palette: (0..16).map(|i| (i, Rgb(seed, i, 0))).collect(),
        ..TermSettings::default()
    };
    for seed in [200, 100] {
        resumed.apply_config(&theme(seed));
        let colors = resumed.ansi_colors();
        // 程序用 OSC 4 改过的第 1 项和 OSC 11 改的背景照旧；没改过的跟着主题走。
        assert_eq!(colors[1], Rgb(0x12, 0x34, 0x56));
        assert_eq!(colors[2], Rgb(seed, 2, 0));
        assert_eq!(resumed.frame().background, Rgb(1, 2, 3));
    }
    // 程序把颜色重置回去后，用的是当前主题的颜色。
    resumed.feed(b"\x1b]104\x07\x1b]111\x07");
    assert_eq!(resumed.ansi_colors()[1], Rgb(100, 1, 0));
    assert_eq!(resumed.frame().background, Rgb(100, 100, 100));
}





