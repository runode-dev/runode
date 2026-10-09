//! 指针下的链接：程序用 OSC 8 标的超链接、文字里的网址，以及按 shell 当前目录解析、软换行或程序自己硬换行折开的路径。

mod common;

use std::path::Path;

use common::{at, idle_session};
use runode_shared_types::session::SessionMeta;
use runode_terminal::session::{LinkSpan, LinkTarget};

#[test]
fn an_osc8_hyperlink_covers_the_cells_it_marks() {
    let mut session = idle_session();
    session.feed(b"go \x1b]8;;https://example.com/x\x1b\\here\x1b]8;;\x1b\\ now");
    let link = session.link_at(at(4.5, 0.5)).unwrap();
    assert_eq!(link.target, LinkTarget::Url("https://example.com/x".into()));
    assert_eq!(link.spans, [LinkSpan { y: 0, x: 3..7 }]);
    assert_eq!(session.link_at(at(1.5, 0.5)), None);
}

#[test]
fn a_url_in_the_text_is_a_link() {
    let mut session = idle_session();
    session.feed(b"at http://a.io/b.");
    let link = session.link_at(at(8.5, 0.5)).unwrap();
    assert_eq!(link.target, LinkTarget::Url("http://a.io/b".into()));
    assert_eq!(link.spans, [LinkSpan { y: 0, x: 3..16 }]);
}

/// 会话 20 列宽，路径从第一行折到第二行；指针点在哪一行都认得出整条，相对路径按 shell 当前目录解析。
#[test]
fn a_wrapped_relative_path_resolves_against_the_cwd() {
    let mut session = idle_session();
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    session.apply_meta(SessionMeta {
        foreground_is_shell: true,
        cwd: Some(crate_dir.into()),
        ..SessionMeta::default()
    });
    session.feed(b"Edited tests/link.rs:12 ok");
    let expected = LinkTarget::Path(crate_dir.join("tests/link.rs"));
    for pointer in [at(10.5, 0.5), at(1.5, 1.5)] {
        let link = session.link_at(pointer).unwrap();
        assert_eq!(link.target, expected);
        assert_eq!(link.spans, [LinkSpan { y: 0, x: 7..20 }, LinkSpan { y: 1, x: 0..3 }]);
    }
    // 不存在的路径不算链接。
    session.feed(b"\r\nsrc/nope.rs");
    assert_eq!(session.link_at(at(3.5, 2.5)), None);
}

/// Claude Code 自己折行：第一行写满 20 列后硬换行，第二行缩进两格接着写。点在哪一行都认得出整条路径，
/// 缩进不算在链接里；本来就到行尾为止的路径不会被下一行接长。
#[test]
fn a_path_hard_wrapped_with_an_indent_is_joined() {
    let mut session = idle_session();
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    session.apply_meta(SessionMeta {
        foreground_is_shell: true,
        cwd: Some(crate_dir.into()),
        ..SessionMeta::default()
    });
    session.feed(b"* Read(tests/common/\r\n  mod.rs)");
    let expected = LinkTarget::Path(crate_dir.join("tests/common/mod.rs"));
    for pointer in [at(10.5, 0.5), at(3.5, 1.5)] {
        let link = session.link_at(pointer).unwrap();
        assert_eq!(link.target, expected);
        assert_eq!(link.spans, [LinkSpan { y: 0, x: 7..20 }, LinkSpan { y: 1, x: 2..8 }]);
    }
    assert_eq!(session.link_at(at(0.5, 1.5)), None);
    session.feed(b"\x1b[H\x1b[2J   see tests/link.rs\r\n  then");
    let link = session.link_at(at(10.5, 0.5)).unwrap();
    assert_eq!(link.target, LinkTarget::Path(crate_dir.join("tests/link.rs")));
    assert_eq!(link.spans, [LinkSpan { y: 0, x: 7..20 }]);
}
