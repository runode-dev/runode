//! 两份 VT 不分叉的差分测试：同一串字节分成同样的几段，喂给宿主那份（`HostSession`）和界面
//! 这份（`Session`），中途在同样的位置改尺寸（宿主 `resize`、界面 `apply_resized`）、清屏
//! （宿主 `clear_screen` 返回的字节原样喂给界面）、换主题（两边 `apply_theme` 同一份设置），
//! 每做一步就比一次：屏幕和最近的回滚历史每行的文字、回滚历史的行数、光标、各个模式、标题、
//! 默认颜色和回滚上限。另有抹掉 shell 集成报告内容（`ReportRedactor`）后的输出喂给界面这份时，
//! 两份照样一样。

use std::time::Duration;

use libghostty_vt::{
    Terminal,
    terminal::{Mode, ModeKind},
};
use runode_shared_types::{
    color::Rgb,
    grid::GridSize,
    settings::{CursorStyle, TermSettings},
    shell::IntegrationMode,
};

use super::Session;
use crate::{
    host_session::{HostSession, ReportRedactor},
    pty::Pty,
    testing::unstarted_pty,
    vt::{
        self,
        tests::{RECORDED_ZSH_VIM_LESS, Rng, mixed_output},
    },
};

/// 比较时读的模式：会被字节流和主题改动的那些，含同步输出（2026）和光标闪烁（12）。
const MODES: [Mode; 16] = [
    Mode::INSERT,
    Mode::DECCKM,
    Mode::ORIGIN,
    Mode::WRAPAROUND,
    Mode::CURSOR_BLINKING,
    Mode::CURSOR_VISIBLE,
    Mode::ALT_SCREEN,
    Mode::ALT_SCREEN_SAVE,
    Mode::BRACKETED_PASTE,
    Mode::SYNC_OUTPUT,
    Mode::FOCUS_EVENT,
    Mode::SGR_MOUSE,
    Mode::ANY_MOUSE,
    Mode::IN_BAND_RESIZE,
    Mode::KEYPAD_KEYS,
    Mode::new(1048, ModeKind::Dec),
];

/// 逐行比文字的回滚历史最多几行；更早的只比行数（见 `state` 的 `scrollback`），免得测试太慢。
const COMPARED_ROWS: usize = 200;

/// 一份 VT 的可比状态，按名字列出，不一样时报出是哪一项。
fn state(terminal: &Terminal<'static, 'static>) -> Vec<(&'static str, String)> {
    let total = terminal.total_rows().unwrap();
    let first = total.saturating_sub(usize::from(terminal.rows().unwrap()) + COMPARED_ROWS);
    vec![
        ("size", format!("{}x{}", terminal.cols().unwrap(), terminal.rows().unwrap())),
        ("text", vt::screen_lines(terminal, first, total - 1).unwrap().join("\n")),
        (
            "cursor",
            format!(
                "{},{} pending {} visible {} style {:?}",
                terminal.cursor_x().unwrap(),
                terminal.cursor_y().unwrap(),
                terminal.is_cursor_pending_wrap().unwrap(),
                terminal.is_cursor_visible().unwrap(),
                terminal.cursor_style().unwrap()
            ),
        ),
        ("modes", MODES.iter().map(|&mode| u8::from(terminal.mode(mode).unwrap()).to_string()).collect()),
        ("screen", format!("{:?}", terminal.active_screen().unwrap())),
        ("title", terminal.title().unwrap().to_owned()),
        ("default palette", format!("{:?}", terminal.default_color_palette().unwrap().0)),
        (
            "default colors",
            format!("{:?} {:?}", terminal.default_fg_color().unwrap(), terminal.default_bg_color().unwrap()),
        ),
        (
            "scrollback",
            format!("{:?} {:?}", terminal.scrollback_max_bytes().unwrap(), terminal.scrollback_rows().unwrap()),
        ),
    ]
}

fn assert_same(context: &str, host: &HostSession, view: &Session) {
    for ((name, expected), (_, actual)) in state(host.terminal()).into_iter().zip(state(&view.terminal)) {
        assert!(expected == actual, "{context}: {name} differs\nhost: {expected}\nview: {actual}");
    }
}

/// 测试里默认的主题：回滚上限比实际默认的小，调试构建的 libghostty 清回滚历史（ED 3）时要逐页
/// 校验，历史太长测试就慢。
fn base_theme() -> TermSettings {
    TermSettings { scrollback_limit: 256 * 1024, ..TermSettings::default() }
}

/// 几份主题：回滚上限有大有小（先调低再调回来最容易让两边的回滚历史分叉），光标闪烁和样式、
/// 调色板也各不相同。
fn themes() -> [TermSettings; 3] {
    [
        base_theme(),
        TermSettings {
            scrollback_limit: 64 * 1024,
            cursor_blink: Some(false),
            cursor_style: CursorStyle::Bar,
            background: Rgb(250, 250, 250),
            palette: (0..16).map(|i| (i, Rgb(i * 10, 0, 255 - i * 10))).collect(),
            ..TermSettings::default()
        },
        TermSettings { scrollback_limit: 1 << 20, cursor_style: CursorStyle::Underline, ..TermSettings::default() },
    ]
}

/// 宿主和界面各一份，按同样的尺寸和主题建好；`started` 时宿主接着一个 `cat`（前台是「shell」，
/// 清屏走整屏清掉那一支），否则接着没启动的伪终端（清屏走删掉光标以上那一支）。
fn pair(size: GridSize, started: bool) -> (HostSession, Session) {
    let settings = base_theme();
    let pty = if started {
        Pty::spawn(size, Some("/bin/cat"), None, IntegrationMode::Off, Box::new(|_| true)).unwrap()
    } else {
        unstarted_pty(size.cols, size.rows)
    };
    let host = HostSession::new(size, pty, None, &settings).unwrap();
    let view = Session::new(size, &settings, Box::new(|_| {})).unwrap();
    (host, view)
}

/// 喂一段同样的字节。
fn feed(host: &mut HostSession, view: &mut Session, data: &[u8]) {
    host.feed(data);
    view.feed(data);
}

fn differential(seed: u64, started: bool) {
    let mut rng = Rng::new(seed);
    let mut data = RECORDED_ZSH_VIM_LESS.to_vec();
    data.extend(mixed_output(&mut rng, 400, 60));
    let (mut host, mut view) = pair(GridSize { cols: 60, rows: 12, cell_width_px: 8, cell_height_px: 16 }, started);
    let themes = themes();
    let mut rest = &data[..];
    let mut step = 0;
    while !rest.is_empty() {
        let n = (1 + rng.below(4096)).min(rest.len());
        feed(&mut host, &mut view, &rest[..n]);
        rest = &rest[n..];
        step += 1;
        let context = format!("seed {seed} started {started} step {step}");
        match rng.below(12) {
            0 => {
                let size = GridSize {
                    cols: 10 + rng.below(90) as u16,
                    rows: 3 + rng.below(30) as u16,
                    cell_width_px: 8,
                    cell_height_px: 16,
                };
                if host.resize(size) {
                    view.apply_resized(size);
                }
            }
            1 if host.at_ground() => {
                if let Some(bytes) = host.clear_screen() {
                    view.feed(&bytes);
                }
            }
            2 => {
                let theme = rng.pick(&themes);
                if host.apply_theme(theme) {
                    view.apply_theme(theme);
                }
            }
            _ => continue,
        }
        assert_same(&context, &host, &view);
    }
    assert_same(&format!("seed {seed} started {started} end"), &host, &view);
}

#[test]
fn the_two_terminals_stay_in_step() {
    // 宿主接着 `cat` 和接着没启动的伪终端各一遍，清屏两支都走到。
    differential(1, true);
    differential(2, false);
}

/// 先把回滚上限调低再调回来：两边在同样的位置调，留下的回滚历史一样。界面要是在标记处套了
/// 别的设置（比如自己当时的配置），调低时丢掉的历史两边不一样，之后再也对不上。
#[test]
fn lowering_and_raising_the_scrollback_limit_keeps_both_histories() {
    // 列数多，每行占的内存多，不多的几行就能占满好几个 page，调低上限时才真的丢历史。
    let (mut host, mut view) = pair(GridSize { cols: 200, rows: 6, cell_width_px: 8, cell_height_px: 16 }, false);
    // 不带超链接的纯文字：调试构建的 libghostty 处理超链接时要逐页校验，量大了很慢。
    let data: Vec<u8> =
        (0..3000).flat_map(|i| format!("\x1b[3{}mline {i} of plain text\x1b[0m\r\n", i % 8).into_bytes()).collect();
    let (first, second) = data.split_at(data.len() / 2);
    let roomy = TermSettings { scrollback_limit: 4 << 20, ..TermSettings::default() };
    host.apply_theme(&roomy);
    view.apply_theme(&roomy);
    feed(&mut host, &mut view, first);
    let before = host.terminal().scrollback_rows().unwrap();
    let lowered = TermSettings { scrollback_limit: 16 * 1024, ..TermSettings::default() };
    for theme in [lowered, roomy] {
        assert!(host.apply_theme(&theme));
        view.apply_theme(&theme);
        assert_same("after a theme", &host, &view);
    }
    assert!(
        host.terminal().scrollback_rows().unwrap() < before,
        "lowering the limit should drop history: {before} -> {}",
        host.terminal().scrollback_rows().unwrap()
    );
    feed(&mut host, &mut view, second);
    assert_same("end", &host, &view);
}

/// 程序开了同步输出（2026）不关：界面超时后照常画，但不动 VT 里的模式，和宿主那份一样。
#[test]
fn a_stuck_synchronized_update_does_not_split_the_terminals() {
    let (mut host, mut view) = pair(GridSize { cols: 20, rows: 4, cell_width_px: 8, cell_height_px: 16 }, false);
    feed(&mut host, &mut view, b"before\r\n\x1b[?2026hduring");
    assert!(view.render_held());
    std::thread::sleep(super::SYNC_OUTPUT_TIMEOUT + Duration::from_millis(100));
    // 超时后取帧：放开画面，画出冻结之后的内容。
    let frame = view.frame().clone();
    assert!(!view.render_held());
    let row: String = frame.row(1).iter().map(|cell| cell.text.as_str()).collect();
    assert!(row.starts_with("during"), "{row:?}");
    assert!(view.terminal.mode(Mode::SYNC_OUTPUT).unwrap());
    assert_same("stuck sync", &host, &view);
    feed(&mut host, &mut view, b"\x1b[?2026l after");
    assert_same("released", &host, &view);
}

/// 宿主转给别的进程的输出抹掉了 shell 集成报告的内容（`ReportRedactor`），报告插在别的序列、
/// 多字节字符中间也一样：界面这份 VT 喂抹过的输出，宿主那份喂原样的，两份照样一样。
#[test]
fn redacted_shell_reports_keep_the_terminals_in_step() {
    let mut rng = Rng::new(3);
    let output = mixed_output(&mut rng, 80, 60);
    let mut data = Vec::new();
    for (i, &byte) in output.iter().enumerate() {
        data.push(byte);
        if rng.chance(1) {
            let end: &[u8] = rng.pick(&[&b"\x07"[..], b"\x1b\\", b"\x18", b"\x1a", b"\x1b[31m"]);
            data.extend(format!("\x1b]6973;0123456789abcdef;cwd=/step%20{i}\x01").as_bytes());
            data.extend(end);
        }
    }
    let (mut host, mut view) = pair(GridSize { cols: 60, rows: 12, cell_width_px: 8, cell_height_px: 16 }, false);
    let mut redactor = ReportRedactor::new();
    let mut redacted = Vec::new();
    let mut rest = &data[..];
    while !rest.is_empty() {
        let n = (1 + rng.below(512)).min(rest.len());
        let chunk = &rest[..n];
        rest = &rest[n..];
        let public = redactor.redact(chunk).unwrap_or_else(|| chunk.to_vec());
        host.feed(chunk);
        view.feed(&public);
        redacted.extend(public);
    }
    assert!(!redacted.windows(16).any(|w| w == b"0123456789abcdef"), "a report survived");
    assert_same("redacted", &host, &view);
}
