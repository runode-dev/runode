//! 拉起单独一个进程的宿主（`runode --host`）：和拉起它的 app 脱开，app 退出后照样跑着。升级时
//! 拉起的接手旧宿主的新宿主（`runode --host --take-over`）见 `launch_successor`。

use std::{
    ffi::{CString, c_char},
    fs::File,
    io,
    os::{
        fd::{AsRawFd as _, OwnedFd},
        unix::ffi::OsStrExt as _,
    },
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
    let pid = spawn_detached(&program, &argv, &actions)?;
    let reaped = thread::Builder::new().name("host-reaper".into()).spawn(move || reap(pid));
    if let Err(err) = reaped {
        tracing::warn!("cannot reap the host process {pid} when it exits: {err}");
    }
    Ok(pid.unsigned_abs())
}

/// 新宿主在这个描述符上报告接手的结果，见 `launch_successor`。
pub const STATUS_FD: libc::c_int = 3;

/// `launch_successor` 拉起的新宿主。
#[derive(Debug)]
pub struct Successor {
    pub pid: u32,
    /// 状态管道的读端：新宿主接手成功或者失败后往里写一行，写完关掉；它没写就退出时读到结尾。
    pub status: File,
}

/// 升级时用 `exe --host --take-over` 拉起新宿主，让它接手 socket 上旧宿主的会话，不等它接完。
///
/// 和 `launch` 一样在新的会话里、标准输入输出接 `/dev/null`、只继承下面这一个描述符、信号恢复
/// 默认、退出后有线程收尸；另外开一根管道，写端接到新进程的 `STATUS_FD` 上，读端交给调用方
/// 读结果。这边的写端拉起后马上关掉，新宿主一退出调用方就读到结尾。
pub fn launch_successor(exe: &Path) -> io::Result<Successor> {
    let program = CString::new(exe.as_os_str().as_bytes())?;
    let argv: [*mut c_char; 4] = [
        program.as_ptr().cast_mut(),
        c"--host".as_ptr().cast_mut(),
        c"--take-over".as_ptr().cast_mut(),
        ptr::null_mut(),
    ];
    // 两端都设了 close-on-exec：别处拉起的进程不会带走写端，让这边等不到结尾。
    let (reader, mut writer) = io::pipe()?;
    if writer.as_raw_fd() == STATUS_FD {
        // dup2 到自己身上不清 close-on-exec；换一个号（3 占着，复制出来的一定不是 3）。
        writer = writer.try_clone()?;
    }
    let mut actions = FileActions::new()?;
    for (fd, flags) in [(0, libc::O_RDONLY), (1, libc::O_WRONLY), (2, libc::O_WRONLY)] {
        // SAFETY: `actions` 已经初始化；路径是以 NUL 结尾的常量。
        check(unsafe {
            libc::posix_spawn_file_actions_addopen(actions.as_mut_ptr(), fd, c"/dev/null".as_ptr(), flags, 0)
        })?;
    }
    // SAFETY: `actions` 已经初始化；`writer` 在 posix_spawn 返回前一直开着。
    check(unsafe { libc::posix_spawn_file_actions_adddup2(actions.as_mut_ptr(), writer.as_raw_fd(), STATUS_FD) })?;
    let pid = spawn_detached(&program, &argv, &actions)?;
    drop(writer);
    let reaped = thread::Builder::new().name("successor-reaper".into()).spawn(move || reap(pid));
    if let Err(err) = reaped {
        tracing::warn!("cannot reap the new host process {pid} when it exits: {err}");
    }
    Ok(Successor { pid: pid.unsigned_abs(), status: File::from(OwnedFd::from(reader)) })
}

/// 在新的会话里拉起 `program`：只继承 `actions` 里设好的描述符，信号处理恢复默认、信号屏蔽清空，
/// 环境变量照抄。返回 pid。
fn spawn_detached(program: &CString, argv: &[*mut c_char], actions: &FileActions) -> io::Result<libc::pid_t> {
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
    debug_assert!(argv.last().is_some_and(|last| last.is_null()));
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
    Ok(pid)
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
