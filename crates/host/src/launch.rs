//! 拉起单独一个进程的宿主（`runode --host`）：和拉起它的 app 脱开，app 退出后照样跑着。

use std::{
    ffi::{CString, c_char},
    io,
    os::unix::ffi::OsStrExt as _,
    path::Path,
    ptr, thread,
};

/// `sys/spawn.h` 的 `POSIX_SPAWN_SETSID`：子进程自己开一个新的会话（`setsid`），不在拉起它的
/// app 的会话和进程组里，app 所在的终端关掉、app 的进程组收到信号都不波及它。`libc` 里没有。
const POSIX_SPAWN_SETSID: libc::c_int = 0x0400;

/// 用 `exe --host` 拉起宿主进程，返回它的 pid，不等它连上 socket。
///
/// 子进程在新的会话里（`setsid`），标准输入输出接 `/dev/null`，除此之外不继承任何描述符
/// （`POSIX_SPAWN_CLOEXEC_DEFAULT`：app 开着的 socket、PTY 这些都不带过去），信号处理恢复默认、
/// 信号屏蔽清空，环境变量照抄。退出后由这里起的一个线程收尸，不留僵尸进程。
///
/// 连上它（每隔几毫秒试一次 socket，撞上它正因空闲退出时重来）是调用方的事。
pub fn launch(exe: &Path) -> io::Result<u32> {
    let program = CString::new(exe.as_os_str().as_bytes())?;
    let host_flag = c"--host";
    let argv: [*mut c_char; 3] = [program.as_ptr().cast_mut(), host_flag.as_ptr().cast_mut(), ptr::null_mut()];
    let mut actions = FileActions::new()?;
    for (fd, flags) in [(0, libc::O_RDONLY), (1, libc::O_WRONLY), (2, libc::O_WRONLY)] {
        // SAFETY: `actions` 已经初始化；路径是以 NUL 结尾的常量。
        check(unsafe {
            libc::posix_spawn_file_actions_addopen(actions.as_mut_ptr(), fd, c"/dev/null".as_ptr(), flags, 0)
        })?;
    }
    let mut attr = SpawnAttr::new()?;
    let flags = POSIX_SPAWN_SETSID
        | libc::POSIX_SPAWN_CLOEXEC_DEFAULT
        | libc::POSIX_SPAWN_SETSIGDEF
        | libc::POSIX_SPAWN_SETSIGMASK;
    // SAFETY: `attr` 已经初始化；两个信号集是本地变量，初始化后才交出去。
    unsafe {
        check(libc::posix_spawnattr_setflags(attr.as_mut_ptr(), flags as libc::c_short))?;
        let mut all: libc::sigset_t = std::mem::zeroed();
        libc::sigfillset(&mut all);
        check(libc::posix_spawnattr_setsigdefault(attr.as_mut_ptr(), &all))?;
        let mut none: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut none);
        check(libc::posix_spawnattr_setsigmask(attr.as_mut_ptr(), &none))?;
    }
    let mut pid: libc::pid_t = 0;
    // SAFETY: 路径和参数都是以 NUL 结尾的字符串，参数表以空指针结尾，在调用期间都活着；环境表
    // 是进程自己的 `environ`，posix_spawn 只读它。
    check(unsafe {
        libc::posix_spawn(
            &mut pid,
            program.as_ptr(),
            actions.as_ptr(),
            attr.as_ptr(),
            argv.as_ptr(),
            (*libc::_NSGetEnviron()).cast_const(),
        )
    })?;
    let reaped = thread::Builder::new().name("host-reaper".into()).spawn(move || reap(pid));
    if let Err(err) = reaped {
        tracing::warn!("cannot reap the host process {pid} when it exits: {err}");
    }
    Ok(pid.unsigned_abs())
}

/// 等子进程退出、收尸。
fn reap(pid: libc::pid_t) {
    let mut status = 0;
    loop {
        // SAFETY: `status` 指向本地变量。
        let result = unsafe { libc::waitpid(pid, &mut status, 0) };
        if result >= 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            break;
        }
    }
}

/// posix_spawn 系列返回的是错误码本身，不设 errno。
fn check(code: libc::c_int) -> io::Result<()> {
    if code == 0 { Ok(()) } else { Err(io::Error::from_raw_os_error(code)) }
}

/// 初始化过的 `posix_spawn_file_actions_t`，丢掉时销毁。
struct FileActions(libc::posix_spawn_file_actions_t);

impl FileActions {
    fn new() -> io::Result<Self> {
        let mut actions: libc::posix_spawn_file_actions_t = ptr::null_mut();
        // SAFETY: 输出参数指向本地变量。
        check(unsafe { libc::posix_spawn_file_actions_init(&mut actions) })?;
        Ok(Self(actions))
    }

    fn as_ptr(&self) -> *const libc::posix_spawn_file_actions_t {
        &raw const self.0
    }

    fn as_mut_ptr(&mut self) -> *mut libc::posix_spawn_file_actions_t {
        &raw mut self.0
    }
}

impl Drop for FileActions {
    fn drop(&mut self) {
        // SAFETY: 由 `new` 初始化，只销毁这一次。
        unsafe { libc::posix_spawn_file_actions_destroy(&mut self.0) };
    }
}

/// 初始化过的 `posix_spawnattr_t`，丢掉时销毁。
struct SpawnAttr(libc::posix_spawnattr_t);

impl SpawnAttr {
    fn new() -> io::Result<Self> {
        let mut attr: libc::posix_spawnattr_t = ptr::null_mut();
        // SAFETY: 输出参数指向本地变量。
        check(unsafe { libc::posix_spawnattr_init(&mut attr) })?;
        Ok(Self(attr))
    }

    fn as_ptr(&self) -> *const libc::posix_spawnattr_t {
        &raw const self.0
    }

    fn as_mut_ptr(&mut self) -> *mut libc::posix_spawnattr_t {
        &raw mut self.0
    }
}

impl Drop for SpawnAttr {
    fn drop(&mut self) {
        // SAFETY: 由 `new` 初始化，只销毁这一次。
        unsafe { libc::posix_spawnattr_destroy(&mut self.0) };
    }
}
