//! 快照格式的 golden 测试：一份固定的 VT 状态编出的快照，和 `tests/golden/snapshot-v{N}.bin`
//! 逐字节比，N 是 `snapshot_format()`。宿主升级时只要两边格式号相同，新宿主就直接解旧宿主编的
//! 快照，所以改了快照的编码必须改格式号（libghostty 快照信封里的 `version`）。
//!
//! - 字节变了、格式号没变：`the_encoding_matches_the_golden_file` 失败。编码真的变了时去改格式号，
//!   再按下面生成新格式号的 golden；旧格式号的文件留着不动。
//! - 只是 runode 自己的 VT 设置变了（编进快照的回滚上限、主题这类选项的值），格式号对应的 golden
//!   照样解得开（`the_golden_file_still_decodes` 通过）时，不必改格式号，重新生成这个号的 golden。
//!
//! 生成或重新生成：`RUNODE_UPDATE_GOLDEN=1 cargo test -p runode-terminal --test snapshot_format`，
//! 写的是当前格式号的文件，提交时连同它一起提交。

use std::path::PathBuf;

use runode_shared_types::{
    color::{Rgb, TerminalColor},
    grid::GridSize,
    settings::{CursorStyle, OptionAsAlt, TermSettings},
};
use runode_terminal::{
    host_session::{HostSession, snapshot_format},
    pty::Pty,
};

const SIZE: GridSize = GridSize { cols: 20, rows: 5, cell_width_px: 8, cell_height_px: 16 };

/// 写全每一项、不取默认值的设置：默认配色以后变了，golden 不跟着变。
fn settings() -> TermSettings {
    TermSettings {
        background: Rgb(0x10, 0x11, 0x12),
        foreground: Rgb(0xe0, 0xe1, 0xe2),
        palette: (0..16).map(|i| (i, Rgb(i * 10, 255 - i * 10, i))).collect(),
        cursor_style: CursorStyle::Bar,
        cursor_blink: Some(false),
        cursor_color: Some(TerminalColor::Rgb(Rgb(1, 2, 3))),
        cursor_text: None,
        selection_background: None,
        selection_foreground: None,
        search_background: TerminalColor::Rgb(Rgb(4, 5, 6)),
        search_foreground: TerminalColor::Rgb(Rgb(7, 8, 9)),
        search_selected_background: TerminalColor::Rgb(Rgb(10, 11, 12)),
        search_selected_foreground: TerminalColor::Rgb(Rgb(13, 14, 15)),
        option_as_alt: OptionAsAlt::False,
        scrollback_limit: 4 * 1024 * 1024,
    }
}

/// 尽量多带些状态：回滚历史、软折行、宽字符、颜色和样式、超链接、程序改的调色板、标题、
/// 存下的光标、滚动区域、模式、等着折行的光标，最后停在一条没写完的序列中间（续接）。
const CONTENT: &[&[u8]] = &[
    b"line 1\r\nline 2\r\nline 3\r\nline 4\r\nline 5\r\nline 6\r\n",
    b"0123456789abcdefghijKLMN\r\n",
    "\x1b[1;31mred\x1b[0m \x1b[4;38;5;123m下划线\x1b[0m \x1b[48;2;1;2;3m宽字\x1b[0m\r\n".as_bytes(),
    b"\x1b]8;id=x;https://example.com\x1b\\link\x1b]8;;\x1b\\ ",
    b"\x1b]4;3;rgb:12/34/56\x07\x1b]2;golden title\x07",
    b"\x1b[2;4r\x1b[?2004h\x1b[?1h\x1b[>1u\x1b7\x1b[2;5H",
    b"\x1b[1;1H01234567890123456789",
    b"\x1b]2;unfinished",
];

fn golden_path() -> PathBuf {
    let format = snapshot_format().expect("the build has a snapshot format");
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/golden/snapshot-v{format}.bin"))
}

fn fixed_snapshot() -> Vec<u8> {
    let pty = Pty::open(SIZE, Box::new(|_| true)).unwrap();
    let mut session = HostSession::new(SIZE, pty, None, &settings()).unwrap();
    for chunk in CONTENT {
        session.feed(chunk);
    }
    session.snapshot().expect("the fixed state encodes")
}

#[test]
fn the_encoding_matches_the_golden_file() {
    let snapshot = fixed_snapshot();
    assert_eq!(snapshot, fixed_snapshot(), "encoding the same state twice must give the same bytes");
    let path = golden_path();
    if std::env::var_os("RUNODE_UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &snapshot).unwrap();
        return;
    }
    let golden = std::fs::read(&path).unwrap_or_else(|err| {
        panic!(
            "no golden snapshot for format {:?} at {} ({err}); after changing the format, generate it with \
             RUNODE_UPDATE_GOLDEN=1 (see the comment at the top of this test)",
            snapshot_format(),
            path.display()
        )
    });
    assert!(
        snapshot == golden,
        "the snapshot encoding changed but the snapshot format is still {:?}: change the format in libghostty, or, \
         if only runode's own VT settings changed and the_golden_file_still_decodes passes, regenerate the golden \
         file (see the comment at the top of this test)",
        snapshot_format()
    );
}

/// 这个构建解得开同一格式号的 golden，解出来的屏幕、标题和停在半条序列上的状态都对。
#[test]
fn the_golden_file_still_decodes() {
    let path = golden_path();
    let Ok(golden) = std::fs::read(&path) else {
        panic!("no golden snapshot at {}", path.display());
    };
    let pty = Pty::open(SIZE, Box::new(|_| true)).unwrap();
    let mut session = HostSession::from_snapshot(&golden, pty, &settings()).unwrap();
    assert_eq!(session.size(), SIZE);
    assert_eq!(session.meta().title.as_deref(), Some("golden title"));
    let screen = session.screen_text(Some(100)).unwrap();
    for text in ["line 1", "0123456789abcdefghij", "KLMN", "red 下划线 宽字", "link", "01234567890123456789"] {
        assert!(screen.contains(text), "{text:?} missing from {screen:?}");
    }
    // 续接还在：标题接着写完。
    assert!(!session.at_ground());
    session.feed(b" now\x07");
    assert_eq!(session.meta().title.as_deref(), Some("unfinished now"));
    // 再编一遍和解之前的一样（解码不丢状态），这一条只在 golden 是这个构建编的时候成立。
    let fresh = HostSession::from_snapshot(&golden, Pty::open(SIZE, Box::new(|_| true)).unwrap(), &settings()).unwrap();
    if fixed_snapshot() == golden {
        assert_eq!(fresh.snapshot().unwrap(), golden);
    }
}
