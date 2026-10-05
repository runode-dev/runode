//! 宿主和界面的会话从快照恢复：快照里的标题照样经过 agent 识别，程序设的颜色在套用新主题
//! 后留着。

mod common;

use common::unstarted_pty;
use runode_shared_types::{color::Rgb, grid::GridSize, settings::TermSettings};
use runode_terminal::{host_session::HostSession, session::Session};

fn size(cols: u16, rows: u16) -> GridSize {
    GridSize { cols, rows, cell_width_px: 8, cell_height_px: 16 }
}

/// 宿主那边的会话，接在没有 shell 的伪终端上，测试里只喂字节。
fn host(cols: u16, rows: u16) -> HostSession {
    HostSession::new(size(cols, rows), unstarted_pty(cols, rows), None, &TermSettings::default()).unwrap()
}

/// 用宿主那边的快照建界面这边的会话，就像界面连上宿主时那样。
fn session_from(source: &HostSession) -> Session {
    Session::from_snapshot(&source.snapshot().unwrap(), Box::new(|_| {})).unwrap()
}

#[test]
fn a_title_from_the_snapshot_goes_through_agent_detection() {
    let mut original = host(40, 6);
    original.feed("\x1b]0;✳ 修 bug\x07".as_bytes());
    let resumed =
        HostSession::from_snapshot(&original.snapshot().unwrap(), unstarted_pty(40, 6), &TermSettings::default()).unwrap();
    assert_eq!(resumed.meta().title.as_deref(), Some("修 bug"));
    assert_eq!(resumed.meta().agent, original.meta().agent);
}

#[test]
fn colors_set_by_programs_survive_apply_theme() {
    let mut original = host(20, 4);
    original.feed(b"\x1b]4;1;rgb:12/34/56\x07\x1b]11;rgb:01/02/03\x07");
    let mut resumed = session_from(&original);
    let theme = |seed: u8| TermSettings {
        background: Rgb(seed, seed, seed),
        palette: (0..16).map(|i| (i, Rgb(seed, i, 0))).collect(),
        ..TermSettings::default()
    };
    for seed in [200, 100] {
        resumed.apply_theme(&theme(seed));
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
