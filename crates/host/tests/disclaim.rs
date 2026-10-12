//! `disclaim_responsibility` 原地重新执行这个测试程序后，它自己担着责任进程：fork 出的子进程算在
//! 它名下，不算在跑测试的终端名下。不用 libtest，自己写 main：改环境变量时不能有别的线程。

mod common;

fn main() {
    let before = std::process::id();
    // SAFETY: 自己写的 main，还没开别的线程。
    unsafe { runode_host::disclaim_responsibility() }.unwrap();
    assert_eq!(std::process::id(), before, "re-executed in place");
    assert!(std::env::var_os("RUNODE_HOST_DISCLAIMED").is_none(), "the marker does not reach the shells");
    #[cfg(target_os = "macos")]
    {
        // SAFETY: 子进程只调 `_exit`。
        let child = unsafe { libc::fork() };
        if child == 0 {
            // SAFETY: 等父进程查完再退出。
            unsafe {
                libc::sleep(5);
                libc::_exit(0);
            }
        }
        let responsible = common::responsible_pid(child);
        // SAFETY: 结束上面 fork 出的子进程并收尸。
        unsafe {
            libc::kill(child, libc::SIGKILL);
            libc::waitpid(child, std::ptr::null_mut(), 0);
        }
        // 责任进程没设过或已经退出时，查到的是进程自己，所以看子进程而不看这个进程。
        assert_eq!(responsible, libc::pid_t::try_from(before).unwrap(), "the child counts as ours");
    }
}
