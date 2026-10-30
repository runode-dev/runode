//! 桥接的开场白：跳过登录脚本打的杂行，读完开场白后剩下的是协议字节。

use std::io::{BufRead as _, BufReader, Read as _};

use runode_ssh::bridge::{Failure, Opening, Start, read_start};

#[test]
fn skips_noise_before_the_opening_line() {
    let opening = Opening { build: "0.1.16.abc".into(), pid: 42, via: "took".into(), session_bound: true };
    let wire = format!("Welcome to Ubuntu\n\n{}\u{0}\u{1}protocol", opening.line());
    let mut input = BufReader::new(wire.as_bytes());
    assert_eq!(read_start(&mut input).unwrap(), Start::Bridge(opening));
    let mut rest = Vec::new();
    input.read_to_end(&mut rest).unwrap();
    assert_eq!(rest, b"\0\x01protocol");
}

#[test]
fn reports_failures_and_probes() {
    let failure = Failure { code: "incompatible".into(), message: "old app\nconnected".into() };
    let line = failure.line();
    let mut input = BufReader::new(line.as_bytes());
    let Start::Failed(read) = read_start(&mut input).unwrap() else { panic!("not a failure") };
    assert_eq!((read.code.as_str(), read.message.as_str()), ("incompatible", "old app connected"));

    let mut probe = BufReader::new("RUNODE-PROBE os=Darwin\nRUNODE-PROBE arch=arm64\n".as_bytes());
    let Start::Probe(Some(platform)) = read_start(&mut probe).unwrap() else { panic!("not a probe") };
    assert_eq!(platform.arch, "arm64");
    assert!(probe.fill_buf().unwrap().is_empty());
    assert_eq!(read_start(&mut BufReader::new("".as_bytes())).unwrap(), Start::Probe(None));
}

#[test]
fn classifies_ssh_errors() {
    use runode_ssh::Failure;
    assert_eq!(Failure::of("me@h: Permission denied (publickey,password)."), Failure::Auth);
    assert_eq!(Failure::of("Host key verification failed."), Failure::HostKey);
    assert_eq!(Failure::of("ssh: Could not resolve hostname x: nodename nor servname provided"), Failure::Resolve);
    assert_eq!(Failure::of("ssh: connect to host h port 22: Operation timed out"), Failure::Timeout);
    assert_eq!(Failure::of("ssh: connect to host h port 22: Connection refused"), Failure::Unreachable);
    assert_eq!(Failure::of("weird\nlast words\n"), Failure::Other("last words".into()));
    assert!(!Failure::Auth.retryable() && Failure::Timeout.retryable());
}
