//! 远程桌面开不起来的错误归类：观看端照归到的类给人看说明，归不了的看原文。

use std::io;

use runode_screen::{StopCause, caused_error, stop_cause};

#[test]
fn caused_errors_keep_their_cause_kind_and_text() {
    let err = caused_error(StopCause::Declined, io::ErrorKind::TimedOut, "nobody allowed it in time");
    assert_eq!(stop_cause(&err), Some(StopCause::Declined));
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    assert_eq!(err.to_string(), "nobody allowed it in time");
}

/// 归过类的错误按它的类，哪怕种类是 `Unsupported`。
#[test]
fn the_cause_wins_over_the_kind() {
    let err = caused_error(StopCause::OtherUser, io::ErrorKind::Unsupported, "someone else");
    assert_eq!(stop_cause(&err), Some(StopCause::OtherUser));
}

#[test]
fn unsupported_means_no_encoder() {
    let err = io::Error::new(io::ErrorKind::Unsupported, "no HEVC encoder");
    assert_eq!(stop_cause(&err), Some(StopCause::NoEncoder));
}

#[test]
fn other_errors_have_no_cause() {
    assert_eq!(stop_cause(&io::Error::other("the display was disconnected")), None);
    assert_eq!(stop_cause(&io::Error::new(io::ErrorKind::PermissionDenied, "denied")), None);
}
