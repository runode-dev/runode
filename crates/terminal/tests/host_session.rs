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

#[test]
fn meta_names_the_foreground_program() {
    let mut session = idle_host();
    session.refresh_foreground();
    // `idle_host` 的「shell」是 `cat`，它就在前台。
    assert_eq!(session.meta().foreground.as_deref(), Some("cat"));
}

#[test]
fn drivers_are_recorded_at_most_once_a_second() {
    use runode_shared_types::session::{DriveAction, Driver};

    let mut session = idle_host();
    session.take_meta();
    let by = Some("ab".repeat(16));
    session.drive(by.clone(), DriveAction::Input, 10_000);
    let driver = Driver { by: by.clone(), action: DriveAction::Input, at_ms: 10_000 };
    assert_eq!(session.take_meta().and_then(|meta| meta.driver), Some(driver));
    // 一秒以内接着打字：时刻不变，状态不用重发。
    session.drive(by.clone(), DriveAction::Input, 10_900);
    assert_eq!(session.take_meta(), None);
    // 换了操作马上记。
    session.drive(by.clone(), DriveAction::Keys, 10_950);
    assert_eq!(session.meta().driver.map(|driver| driver.action), Some(DriveAction::Keys));
    session.drive(by, DriveAction::Keys, 12_000);
    assert_eq!(session.take_meta().and_then(|meta| meta.driver).map(|driver| driver.at_ms), Some(12_000));
    // 用户自己打字后清掉。
    session.clear_driver();
    assert_eq!(session.take_meta().map(|meta| meta.driver), Some(None));
    session.clear_driver();
    assert_eq!(session.take_meta(), None);
}
