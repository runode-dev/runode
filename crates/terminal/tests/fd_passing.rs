//! 在 Unix socket 上连同描述符一起收发消息。

use std::{
    io::{self, Read as _, Write as _},
    os::{
        fd::{AsFd as _, AsRawFd as _, OwnedFd},
        unix::net::UnixStream,
    },
    thread,
};

use runode_terminal::fd_passing::{MAX_FDS, recv_with_fds, send_with_fds};

/// 收到的描述符是新的一份，指向同一个管道，而且带着 `FD_CLOEXEC`。
#[test]
fn descriptors_arrive_with_the_bytes() {
    let (a, b) = UnixStream::pair().unwrap();
    let (pipe_rx, pipe_tx) = io::pipe().unwrap();
    send_with_fds(&a, b"hello", &[pipe_rx.as_fd(), pipe_tx.as_fd()]).unwrap();
    let (data, fds) = recv_with_fds(&b).unwrap();
    assert_eq!(data, b"hello");
    let [rx, tx]: [OwnedFd; 2] = fds.try_into().unwrap();
    assert_ne!(rx.as_raw_fd(), pipe_rx.as_raw_fd());
    for fd in [&rx, &tx] {
        // SAFETY: 描述符开着，F_GETFD 只读标志。
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
    }
    drop((pipe_rx, pipe_tx));
    let mut tx = std::fs::File::from(tx);
    tx.write_all(b"through").unwrap();
    drop(tx);
    let mut got = String::new();
    std::fs::File::from(rx).read_to_string(&mut got).unwrap();
    assert_eq!(got, "through");
}

/// 几条消息连着发，各自的字节和描述符不串。空消息和不带描述符的消息也行。
#[test]
fn messages_keep_their_own_descriptors() {
    let (a, b) = UnixStream::pair().unwrap();
    let (pipe_rx, _pipe_tx) = io::pipe().unwrap();
    send_with_fds(&a, b"", &[pipe_rx.as_fd()]).unwrap();
    send_with_fds(&a, b"none", &[]).unwrap();
    send_with_fds(&a, b"three", &[pipe_rx.as_fd(), pipe_rx.as_fd(), pipe_rx.as_fd()]).unwrap();
    let counts: Vec<(Vec<u8>, usize)> = (0..3)
        .map(|_| {
            let (data, fds) = recv_with_fds(&b).unwrap();
            (data, fds.len())
        })
        .collect();
    assert_eq!(counts, [(b"".to_vec(), 1), (b"none".to_vec(), 0), (b"three".to_vec(), 3)]);
}

/// 比 socket 缓冲大得多的消息分几次发完，描述符照样跟着到。
#[test]
fn a_large_message_is_sent_in_pieces() {
    let (a, b) = UnixStream::pair().unwrap();
    let data: Vec<u8> = (0..4 << 20).map(|i: u32| (i % 251) as u8).collect();
    let expected = data.clone();
    let sender = thread::spawn(move || {
        let (pipe_rx, _pipe_tx) = io::pipe().unwrap();
        send_with_fds(&a, &data, &[pipe_rx.as_fd()]).unwrap();
    });
    let (got, fds) = recv_with_fds(&b).unwrap();
    sender.join().unwrap();
    assert_eq!(got.len(), expected.len());
    assert!(got == expected);
    assert_eq!(fds.len(), 1);
}

#[test]
fn too_many_descriptors_are_refused() {
    let (a, _b) = UnixStream::pair().unwrap();
    let (pipe_rx, _pipe_tx) = io::pipe().unwrap();
    let fds = vec![pipe_rx.as_fd(); MAX_FDS + 1];
    assert_eq!(send_with_fds(&a, b"", &fds).unwrap_err().kind(), io::ErrorKind::InvalidInput);
    let fds = vec![pipe_rx.as_fd(); MAX_FDS];
    send_with_fds(&a, b"", &fds).unwrap();
}

/// 对面在一条消息中间关了连接。
#[test]
fn a_closed_peer_is_unexpected_eof() {
    let (a, b) = UnixStream::pair().unwrap();
    drop(a);
    assert_eq!(recv_with_fds(&b).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);

    let (mut a, b) = UnixStream::pair().unwrap();
    // 头里说有 10 字节，只发了 2 字节就关了。
    a.write_all(&[10, 0, 0, 0, 0, 0, 0, 0, 1, 2]).unwrap();
    drop(a);
    assert_eq!(recv_with_fds(&b).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
}

/// 头里说的描述符个数和实际收到的对不上。
#[test]
fn a_missing_descriptor_is_an_error() {
    let (mut a, b) = UnixStream::pair().unwrap();
    a.write_all(&[0, 0, 0, 0, 1, 0, 0, 0]).unwrap();
    assert_eq!(recv_with_fds(&b).unwrap_err().kind(), io::ErrorKind::InvalidData);
}
