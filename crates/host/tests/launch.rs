//! 拉起单独的宿主进程：子进程在自己的会话里，不继承拉起它的进程开着的描述符。这里拉起的不是
//! 真宿主，是一个不看参数的脚本。

mod common;

use std::{
    io::Read as _,
    os::fd::AsRawFd as _,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// 测试进程里开着、没设 `FD_CLOEXEC` 的描述符号；子进程里不该有它。
const LEAKY_FD: i32 = 57;

#[test]
fn the_host_runs_in_its_own_session_without_our_descriptors() {
    let dir = common::temp_dir("launch");
    let listing = dir.join("fds.txt");
    let script = common::script(
        &dir,
        "host.sh",
        &format!("ls /dev/fd > {}.tmp\nmv {0}.tmp {0}\nexec /bin/sleep 30", listing.display()),
    );
    let file = std::fs::File::open(&script).unwrap();
    // SAFETY: 两个描述符都有效；`dup2` 出来的没有 `FD_CLOEXEC`，用完关掉。
    assert_eq!(unsafe { libc::dup2(file.as_raw_fd(), LEAKY_FD) }, LEAKY_FD);

    let pid = runode_host::launch(std::path::Path::new(&script)).unwrap();
    // SAFETY: 关掉上面 `dup2` 出来的那份。
    unsafe { libc::close(LEAKY_FD) };
    let pid = libc::pid_t::try_from(pid).unwrap();

    let deadline = Instant::now() + common::WAIT;
    while !listing.exists() {
        assert!(Instant::now() < deadline, "the launched program did not run");
        std::thread::sleep(Duration::from_millis(20));
    }
    // SAFETY: 只读进程的会话号和进程组号。
    let (sid, pgid, ours) = unsafe { (libc::getsid(pid), libc::getpgid(pid), libc::getsid(0)) };
    let fds = std::fs::read_to_string(&listing).unwrap();
    #[cfg(target_os = "macos")]
    let responsible = common::responsible_pid(pid);
    // SAFETY: 结束拉起的子进程；它由 `launch` 起的线程收尸。
    unsafe { libc::kill(pid, libc::SIGTERM) };

    assert_eq!((sid, pgid), (pid, pid), "the host leads its own session");
    assert_ne!(sid, ours);
    #[cfg(target_os = "macos")]
    assert_eq!(responsible, pid, "the host is its own responsible process, not ours");
    let fds: Vec<&str> = fds.split_whitespace().collect();
    assert!(!fds.contains(&LEAKY_FD.to_string().as_str()), "inherited {fds:?}");
    assert!(fds.contains(&"0") && fds.contains(&"2"), "{fds:?}");
}

#[test]
fn launching_a_missing_program_fails() {
    assert!(runode_host::launch(std::path::Path::new("/nonexistent/runode")).is_err());
}

/// 拉起的新宿主在 `STATUS_FD` 上写的那一行，拉起它的一方从状态管道读得到。
#[test]
fn the_status_pipe_reaches_the_successor() {
    let marker = std::env::var_os(CLOSED_STDIO);
    if marker.is_some() {
        // Rust 程序启动时把关着的标准输入输出接到 /dev/null，所以要在跑起来以后自己关。
        // SAFETY: 这个子进程只跑这一个测试，之后没人再用这三个描述符。
        unsafe {
            for fd in 0..=2 {
                libc::close(fd);
            }
        }
    }
    let dir = common::temp_dir("status");
    let script = common::script(&dir, "successor.sh", "echo ok >&3");
    let mut successor = runode_host::launch_successor(std::path::Path::new(&script)).unwrap();
    let mut status = String::new();
    successor.status.read_to_string(&mut status).unwrap();
    assert_eq!(status.trim(), "ok");
    // 告诉父进程这个测试真的跑完了。
    if let Some(marker) = marker {
        std::fs::write(marker, "ran").unwrap();
    }
}

/// 子进程里设了它时，`the_status_pipe_reaches_the_successor` 先关掉标准输入输出再拉起新宿主，跑完往它给的路径
/// 写一个文件。
const CLOSED_STDIO: &str = "RUNODE_TEST_CLOSED_STDIO";

/// 拉起新宿主的进程关着标准输入输出时，状态管道的写端会落在 0 到 2 上，照样要接到新宿主的
/// `STATUS_FD`：在单独的子进程里关掉这三个、再跑一遍 `the_status_pipe_reaches_the_successor`。
#[test]
fn the_status_pipe_survives_closed_standard_streams() {
    let marker = common::temp_dir("closed-stdio").join("ran");
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "the_status_pipe_reaches_the_successor", "--test-threads=1"])
        .env(CLOSED_STDIO, &marker)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "the run with closed standard streams failed: {status}");
    assert!(marker.exists(), "the run with closed standard streams did not run the test");
}
