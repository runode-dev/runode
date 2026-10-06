//! 宿主那份会话：终端查询由它应答，会话公布的状态变了才交出来。

mod common;

use std::{
    thread,
    time::{Duration, Instant},
};

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
    // `idle_host` 的「shell」是 `cat`，它就在前台。刚拉起时子进程可能还没 exec 成 `cat`，前台的
    // 进程名还是测试程序自己的，所以重读到它换过来为止。
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        session.refresh_foreground();
        let foreground = session.meta().foreground;
        if foreground.as_deref() == Some("cat") {
            break;
        }
        assert!(Instant::now() < until, "timed out waiting for cat in the foreground, last saw {foreground:?}");
        thread::sleep(Duration::from_millis(20));
    }
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
