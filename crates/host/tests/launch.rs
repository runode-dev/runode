//! 拉起单独的宿主进程：子进程在自己的会话里，不继承拉起它的进程开着的描述符。这里拉起的不是
//! 真宿主，是一个不看参数的脚本。

mod common;

use std::{
    os::fd::AsRawFd as _,
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
    let responsible = responsible_pid(pid);
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

/// macOS 按哪个进程的授权判断 `pid` 的隐私权限（本地网络等）。
#[cfg(target_os = "macos")]
fn responsible_pid(pid: libc::pid_t) -> libc::pid_t {
    unsafe extern "C" {
        fn responsibility_get_pid_responsible_for_pid(pid: libc::pid_t) -> libc::pid_t;
    }
    // SAFETY: 只按 pid 查，不碰内存。
    unsafe { responsibility_get_pid_responsible_for_pid(pid) }
}

#[test]
fn launching_a_missing_program_fails() {
    assert!(runode_host::launch(std::path::Path::new("/nonexistent/runode")).is_err());
}
