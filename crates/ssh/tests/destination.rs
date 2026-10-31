//! 用户填的连接目标：别名、user@host:port、ssh:// 写法、IPv6，不收会被当成选项的。

use runode_ssh::Destination;

fn dest(target: &str, port: Option<u16>) -> Option<Destination> {
    Some(Destination { target: target.into(), port })
}

#[test]
fn parses_destinations() {
    assert_eq!(Destination::parse(" devbox "), dest("devbox", None));
    assert_eq!(Destination::parse("me@host:2222"), dest("me@host", Some(2222)));
    assert_eq!(Destination::parse("ssh://me@host:22/"), dest("me@host", Some(22)));
    assert_eq!(Destination::parse("[::1]:2200"), dest("::1", Some(2200)));
    assert_eq!(Destination::parse("fe80::1"), dest("fe80::1", None));
    assert_eq!(Destination::parse("-oProxyCommand=x"), None);
    assert_eq!(Destination::parse("me@-x"), None);
    assert_eq!(Destination::parse("a b"), None);
    assert_eq!(Destination::parse("host:port"), None);
    assert_eq!(Destination::parse(""), None);
    let with_port = Destination::parse("me@host:2222").unwrap();
    assert_eq!(with_port.args(), ["-p", "2222", "--", "me@host"]);
    assert_eq!(with_port.to_string(), "me@host:2222");
    assert_eq!(Destination::parse("[::1]:22").unwrap().to_string(), "[::1]:22");
    let v6 = Destination::parse("me@[2001:db8::1]:22").unwrap();
    assert_eq!(Destination::parse(&v6.to_string()), Some(v6));
}
