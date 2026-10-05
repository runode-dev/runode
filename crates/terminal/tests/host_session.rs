//! 宿主那份会话：终端查询由它应答，会话公布的状态变了才交出来。

mod common;

use common::idle_host;

#[test]
fn terminal_queries_are_answered_by_the_host() {
    let mut session = idle_host();
    session.feed(b"\x1b[c\x1b[5n\x1b[6n\x1b[>q");
    assert!(session.replies() >= 4, "{}", session.replies());
}

#[test]
fn meta_is_handed_out_only_when_it_changed() {
    let mut session = idle_host();
    assert!(session.take_meta().is_some());
    assert_eq!(session.take_meta(), None);
    session.feed(b"\x1b]2;hello\x07");
    assert_eq!(session.take_meta().and_then(|meta| meta.title).as_deref(), Some("hello"));
    // 同样的标题再来一遍不算变化。
    session.feed(b"\x1b]2;hello\x07");
    assert_eq!(session.take_meta(), None);
}
