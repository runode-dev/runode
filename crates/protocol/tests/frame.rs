//! 帧的编解码：往返、帧头字节序、截断、超长、未知种类和分次读到的帧。

use std::io::{self, Read};

use runode_protocol::frame::{FrameError, FrameKind, HEADER_LEN, MAX_PAYLOAD, read_frame, write_frame};

fn encode(kind: FrameKind, channel: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    write_frame(&mut out, kind, channel, payload).unwrap();
    out
}

#[test]
fn frames_round_trip() {
    let mut stream = Vec::new();
    for (kind, channel, payload) in [
        (FrameKind::Output, 7, &b"\x1b[31mred"[..]),
        (FrameKind::Input, 7, b""),
        (FrameKind::Control, 0, br#"{"type":"list_sessions"}"#),
        (FrameKind::Snapshot, u32::MAX, &[0u8; 300][..]),
    ] {
        stream.extend(encode(kind, channel, payload));
    }
    let mut reader = &stream[..];
    let mut frames = Vec::new();
    while let Some(frame) = read_frame(&mut reader).unwrap() {
        frames.push((frame.kind, frame.channel, frame.payload.len()));
    }
    assert_eq!(
        frames,
        [
            (FrameKind::Output, 7, 8),
            (FrameKind::Input, 7, 0),
            (FrameKind::Control, 0, 24),
            (FrameKind::Snapshot, u32::MAX, 300)
        ]
    );
}

#[test]
fn the_header_is_little_endian() {
    assert_eq!(encode(FrameKind::Snapshot, 0x0102_0304, b"ab"), [2, 0, 0, 0, 3, 4, 3, 2, 1, b'a', b'b']);
}

#[test]
fn truncated_frames_are_errors() {
    let frame = encode(FrameKind::Output, 1, b"hello");
    // 帧头读了一半、载荷读了一半。
    for len in [1, HEADER_LEN - 1, HEADER_LEN, frame.len() - 1] {
        assert!(matches!(read_frame(&mut &frame[..len]), Err(FrameError::Truncated)), "{len}");
    }
    assert!(read_frame(&mut &frame[..0]).unwrap().is_none());
}

#[test]
fn oversized_frames_are_rejected() {
    // 读的时候不照着声明的长度分配内存。
    let mut header = (MAX_PAYLOAD + 1).to_le_bytes().to_vec();
    header.extend([0, 0, 0, 0, 0]);
    assert!(matches!(read_frame(&mut &header[..]), Err(FrameError::TooLong(len)) if len == u64::from(MAX_PAYLOAD) + 1));
    let mut header = u32::MAX.to_le_bytes().to_vec();
    header.extend([2, 0, 0, 0, 0]);
    assert!(matches!(read_frame(&mut &header[..]), Err(FrameError::TooLong(_))));
    // 写的时候什么都不写。
    let mut out = Vec::new();
    let payload = vec![0u8; MAX_PAYLOAD as usize + 1];
    assert!(matches!(write_frame(&mut out, FrameKind::Snapshot, 1, &payload), Err(FrameError::TooLong(_))));
    assert!(out.is_empty());
    // 正好到上限的可以。
    write_frame(&mut out, FrameKind::Snapshot, 1, &payload[1..]).unwrap();
    assert_eq!(read_frame(&mut &out[..]).unwrap().unwrap().payload.len(), MAX_PAYLOAD as usize);
}

#[test]
fn unknown_kinds_are_rejected() {
    let mut frame = encode(FrameKind::Output, 1, b"x");
    frame[4] = 9;
    assert!(matches!(read_frame(&mut &frame[..]), Err(FrameError::UnknownKind(9))));
}

/// 一次只给一个字节的读取方，像慢的 socket。
struct Trickle<'a>(&'a [u8]);

impl Read for Trickle<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some((&first, rest)) = self.0.split_first() else {
            return Ok(0);
        };
        if buf.is_empty() {
            return Ok(0);
        }
        buf[0] = first;
        self.0 = rest;
        Ok(1)
    }
}

#[test]
fn short_reads_are_reassembled() {
    let stream = [encode(FrameKind::Output, 3, "中文".as_bytes()), encode(FrameKind::Input, 4, b"ls\r")].concat();
    let mut reader = Trickle(&stream);
    assert_eq!(read_frame(&mut reader).unwrap().unwrap().payload, "中文".as_bytes());
    assert_eq!(read_frame(&mut reader).unwrap().unwrap().channel, 4);
    assert!(read_frame(&mut reader).unwrap().is_none());
}
