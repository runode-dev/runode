//! 拉起单独一个进程的宿主（`runode --host`）：和拉起它的 app 脱开，app 退出后照样跑着。升级时
//! 拉起的接手旧宿主的新宿主（`runode --host --take-over`）见 `launch_successor`。macOS 上经 launchd
//! 拉起，宿主不留在 app 的 coalition 里，见 `launchd`；用不了 launchd 时和别的 Unix 一样直接
//! `posix_spawn`（`detach`）。Windows 上不接 app 的控制台、自成一个进程组地拉起。

#[cfg(target_os = "macos")]
mod launchd;

use std::{ffi::CStr, io, path::Path};
#[cfg(unix)]
use std::{
    ffi::{CString, c_char},
    fs::File,
    os::{
        fd::{AsRawFd as _, FromRawFd as _, OwnedFd},
        unix::ffi::OsStrExt as _,
    },
    ptr, thread,
};

/// `sys/spawn.h` 的 `POSIX_SPAWN_SETSID`：子进程自己开一个新的会话（`setsid`），不在拉起它的
/// app 的会话和进程组里，app 所在的终端关掉、app 的进程组收到信号都不波及它。`libc` 里没有。
#[cfg(target_os = "macos")]
const POSIX_SPAWN_SETSID: libc::c_int = 0x0400;
#[cfg(all(unix, not(target_os = "macos")))]
const POSIX_SPAWN_SETSID: libc::c_int = 0x80;

/// 除了显式接上的，子进程不继承任何描述符。只有 macOS 有这个标志；Linux 上另由 `close_from`
/// 关描述符。
#[cfg(target_os = "macos")]
const CLOEXEC_DEFAULT: libc::c_int = libc::POSIX_SPAWN_CLOEXEC_DEFAULT;
#[cfg(all(unix, not(target_os = "macos")))]
const CLOEXEC_DEFAULT: libc::c_int = 0;

// libSystem 导出、没有公开头文件的接口：子进程不再把拉起它的 app 当作「责任进程」，自己担着。
// macOS 按责任进程判断本地网络这类隐私权限；宿主比 app 活得久，挂在 app 名下的话，app 一退出，
// 宿主和它拉起的 shell 都查不到授权，连局域网报 `No route to host`。宿主是同一个 .app 里的可执行
// 文件，自己担着时照样按这个 app 的授权算。
#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn responsibility_spawnattrs_setdisclaim(attr: *mut libc::posix_spawnattr_t, disclaim: bool) -> libc::c_int;
}

/// 进程自己的环境变量表。
#[cfg(target_os = "macos")]
fn environ() -> *const *mut c_char {
    // SAFETY: `_NSGetEnviron` 总是返回指向进程环境表指针的有效指针。
    unsafe { (*libc::_NSGetEnviron()).cast_const() }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn environ() -> *const *mut c_char {
    unsafe extern "C" {
        static environ: *const *mut c_char;
    }
    // SAFETY: libc 导出的进程环境表，只读它的值。
    unsafe { environ }
}

/// 用 `exe --host` 拉起宿主进程，返回它的 pid，不等它连上 socket。
///
/// 子进程和拉起它的 app 脱开：不在 app 的会话和进程组里，不继承 app 的描述符，见 `detach` 和 `launchd`。
///
/// 连上它（每隔几毫秒试一次 socket，撞上它正因空闲退出时重来）是调用方的事。
pub fn launch(exe: &Path) -> io::Result<u32> {
    launch_with(exe, c"--host")
}

/// 用 `exe --screen` 拉起远程桌面的采集进程，返回它的 pid，不等它连上 socket。和宿主一样与拉起它的
/// 进程脱开、自己担着责任进程，屏幕录制和辅助功能的授权按这个 app 算。
pub fn launch_screen(exe: &Path) -> io::Result<u32> {
    launch_with(exe, c"--screen")
}

#[cfg(unix)]
fn launch_with(exe: &Path, arg: &CStr) -> io::Result<u32> {
    #[cfg(target_os = "macos")]
    match launchd::submit(exe, &[arg.to_str().map_err(io::Error::other)?], false) {
        Ok((pid, _)) => return Ok(pid),
        Err(launchd::Error::Failed(err)) => return Err(err),
        Err(launchd::Error::Unavailable(err)) => {
            tracing::warn!("cannot start {exe:?} {arg:?} through launchd, it stays in our coalition: {err}");
        }
    }
    detach(exe, &[arg], None)
}

/// 标准输入输出接空设备，不接 app 的控制台（`DETACHED_PROCESS`），app 的控制台收到 Ctrl+C 不波及它；
/// app 在作业对象里（比如从终端里起的）时尽量脱出去（`CREATE_BREAKAWAY_FROM_JOB`），作业不让脱就留在
/// 里面。不用 `CREATE_NEW_PROCESS_GROUP`：它让宿主和宿主起的 shell 都忽略 Ctrl+C。
#[cfg(windows)]
fn launch_with(exe: &Path, arg: &CStr) -> io::Result<u32> {
    use std::{
        os::windows::process::CommandExt as _,
        process::{Command, Stdio},
    };
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    let arg = arg.to_str().map_err(io::Error::other)?;
    let spawn = |flags| {
        Command::new(exe)
            .arg(arg)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(flags)
            .spawn()
    };
    let child = match spawn(DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB) {
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied => spawn(DETACHED_PROCESS)?,
        spawned => spawned?,
    };
    Ok(child.id())
}

/// 新宿主在这个描述符上报告接手的结果，见 `launch_successor`。
#[cfg(unix)]
pub const STATUS_FD: libc::c_int = 3;

/// `launch_successor` 拉起的新宿主。
#[cfg(unix)]
#[derive(Debug)]
pub struct Successor {
    pub pid: u32,
    /// 状态管道的读端：新宿主接手成功或者失败后往里写一行，写完关掉；它没写就退出时读到结尾。
    pub status: File,
}

/// 升级时用 `exe --host --take-over` 拉起新宿主，让它接手 socket 上旧宿主的会话，不等它接完。
///
/// 和 `launch` 一样与 app 脱开，只多继承下面这一个描述符；另外开一根管道（macOS 上经 launchd 拉起时是 FIFO），写端接到新进程的 `STATUS_FD` 上，读端
/// 交给调用方读结果。这边不留写端，新宿主一退出调用方就读到结尾。
#[cfg(unix)]
pub fn launch_successor(exe: &Path) -> io::Result<Successor> {
    #[cfg(target_os = "macos")]
    match launchd::submit(exe, &["--host", "--take-over"], true) {
        Ok((pid, status)) => return Ok(Successor { pid, status }),
        Err(launchd::Error::Failed(err)) => return Err(err),
        Err(launchd::Error::Unavailable(err)) => {
            tracing::warn!("cannot start the new host through launchd, it stays in our coalition: {err}");
        }
    }
    // 两端都设了 close-on-exec：别处拉起的进程不会带走写端，让这边等不到结尾。
    let (reader, writer) = io::pipe()?;
    let mut writer = OwnedFd::from(writer);
    if writer.as_raw_fd() <= STATUS_FD {
        // 挪到比 `STATUS_FD` 大的号上：dup2 到自己身上不清 close-on-exec；调用方关着标准输入输出时写端可能
        // 落在 0 到 2 上，`detach` 先把这三个换成 /dev/null，再接到 `STATUS_FD` 上的就是 /dev/null 了。
        // SAFETY: 描述符来自上面的管道，在这次调用期间开着；`F_DUPFD_CLOEXEC` 返回新的描述符，归这里。
        let moved = unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_DUPFD_CLOEXEC, STATUS_FD + 1) };
        if moved < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `moved` 是刚复制出来、没有别人拿着的描述符。
        writer = unsafe { OwnedFd::from_raw_fd(moved) };
    }
    let pid = detach(exe, &[c"--host", c"--take-over"], Some(&writer))?;
    drop(writer);
    Ok(Successor { pid, status: File::from(OwnedFd::from(reader)) })
}

/// 让这个宿主进程自己担着责任进程：带上 `responsibility_spawnattrs_setdisclaim` 原地重新执行自己一次
/// （`POSIX_SPAWN_SETEXEC`，pid、参数、环境和没设 close-on-exec 的描述符都不变），重新执行后的那次调用
/// 直接返回。`launch` 拉起的本来就自己担着；在终端里手动跑的挂在那个终端的责任进程（在 runode 的终端里
/// 就是宿主）名下，那个进程一退出，这个宿主开的终端就都连不上局域网。成功时不返回。
///
/// # Safety
///
/// 要改环境变量：调用时进程里不能有别的线程。
pub unsafe fn disclaim_responsibility() -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        // 重新执行过的标记，免得一直重来；读到就去掉，不传给宿主开的 shell。
        const MARK: &str = "RUNODE_HOST_DISCLAIMED";
        if std::env::var_os(MARK).is_some() {
            // SAFETY: 调用方保证还没有别的线程。
            unsafe { std::env::remove_var(MARK) };
            return Ok(());
        }
        let program = CString::new(std::env::current_exe()?.into_os_string().as_bytes())?;
        let args = std::env::args_os().map(|arg| CString::new(arg.as_bytes())).collect::<Result<Vec<_>, _>>()?;
        let argv: Vec<*mut c_char> = args.iter().map(|arg| arg.as_ptr().cast_mut()).chain([ptr::null_mut()]).collect();
        let mut attr = SpawnAttr::new()?;
        // SAFETY: `attr` 已经初始化。
        unsafe {
            check(libc::posix_spawnattr_setflags(attr.as_mut_ptr(), libc::POSIX_SPAWN_SETEXEC as libc::c_short))?;
            check(responsibility_spawnattrs_setdisclaim(attr.as_mut_ptr(), true))?;
        }
        // SAFETY: 调用方保证还没有别的线程。
        unsafe { std::env::set_var(MARK, "1") };
        let mut pid: libc::pid_t = 0;
        // SAFETY: 同 `detach`；成功时这个进程换成新的映像，不再回到这里。
        let failed = check(unsafe {
            libc::posix_spawn(&mut pid, program.as_ptr(), ptr::null(), attr.as_ptr(), argv.as_ptr(), environ())
        });
        // SAFETY: 同上。
        unsafe { std::env::remove_var(MARK) };
        failed
    }
    #[cfg(not(target_os = "macos"))]
    Ok(())
}

/// 在新的会话里用 `exe extra_args...` 拉起进程，返回 pid：标准输入输出接 `/dev/null`，给了
/// `status` 时把它接到 `STATUS_FD` 上，此外不继承任何描述符；信号处理恢复默认、信号屏蔽清空，
/// 环境变量照抄，macOS 上自己担着责任进程。退出后由这里起的一个线程收尸。
#[cfg(unix)]
fn detach(exe: &Path, extra_args: &[&CStr], status: Option<&OwnedFd>) -> io::Result<u32> {
    let program = CString::new(exe.as_os_str().as_bytes())?;
    let argv: Vec<*mut c_char> = std::iter::once(program.as_c_str())
        .chain(extra_args.iter().copied())
        .map(|arg| arg.as_ptr().cast_mut())
        .chain([ptr::null_mut()])
        .collect();
    let mut actions = FileActions::new()?;
    for (fd, flags) in [(0, libc::O_RDONLY), (1, libc::O_WRONLY), (2, libc::O_WRONLY)] {
        // SAFETY: `actions` 已经初始化；路径是以 NUL 结尾的常量。
        check(unsafe {
            libc::posix_spawn_file_actions_addopen(actions.as_mut_ptr(), fd, c"/dev/null".as_ptr(), flags, 0)
        })?;
    }
    if let Some(status) = status {
        // SAFETY: `actions` 已经初始化；`status` 在 posix_spawn 返回前一直开着。
        check(unsafe { libc::posix_spawn_file_actions_adddup2(actions.as_mut_ptr(), status.as_raw_fd(), STATUS_FD) })?;
    }
    #[cfg(target_os = "linux")]
    close_from(&mut actions, if status.is_some() { STATUS_FD + 1 } else { STATUS_FD })?;
    let mut attr = SpawnAttr::new()?;
    let flags = POSIX_SPAWN_SETSID | CLOEXEC_DEFAULT | libc::POSIX_SPAWN_SETSIGDEF | libc::POSIX_SPAWN_SETSIGMASK;
    // SAFETY: `attr` 已经初始化；两个信号集是本地变量，初始化后才交出去。
    unsafe {
        check(libc::posix_spawnattr_setflags(attr.as_mut_ptr(), flags as libc::c_short))?;
        let mut all: libc::sigset_t = std::mem::zeroed();
        libc::sigfillset(&mut all);
        check(libc::posix_spawnattr_setsigdefault(attr.as_mut_ptr(), &all))?;
        let mut none: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut none);
        check(libc::posix_spawnattr_setsigmask(attr.as_mut_ptr(), &none))?;
        #[cfg(target_os = "macos")]
        check(responsibility_spawnattrs_setdisclaim(attr.as_mut_ptr(), true))?;
    }
    let mut pid: libc::pid_t = 0;
    // SAFETY: 路径和参数都是以 NUL 结尾的字符串，参数表以空指针结尾，在调用期间都活着；环境表
    // 是进程自己的 `environ`，posix_spawn 只读它。
    check(unsafe {
        libc::posix_spawn(&mut pid, program.as_ptr(), actions.as_ptr(), attr.as_ptr(), argv.as_ptr(), environ())
    })?;
    let reaped = thread::Builder::new().name("host-reaper".into()).spawn(move || reap(pid));
    if let Err(err) = reaped {
        tracing::warn!("cannot reap the host process {pid} when it exits: {err}");
    }
    Ok(pid.unsigned_abs())
}

/// 等子进程退出、收尸。
#[cfg(unix)]
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
#[cfg(unix)]
fn check(code: libc::c_int) -> io::Result<()> {
    if code == 0 { Ok(()) } else { Err(io::Error::from_raw_os_error(code)) }
}

/// 初始化过的 `posix_spawn_file_actions_t`，丢掉时销毁。
#[cfg(unix)]
struct FileActions(libc::posix_spawn_file_actions_t);

#[cfg(unix)]
impl FileActions {
    fn new() -> io::Result<Self> {
        // SAFETY: 不论是指针（macOS）还是结构体（Linux），全零都是合法的待初始化值。
        let mut actions: libc::posix_spawn_file_actions_t = unsafe { std::mem::zeroed() };
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

/// 子进程里关掉 `from` 起的所有描述符，接在 `actions` 已有的动作后面：app 从别的程序继承来的、C 库没设
/// close-on-exec 打开的描述符不跟着宿主活下去，比如拉起 app 的进程等着结尾的管道写端。用 glibc 2.34 起有的
/// `posix_spawn_file_actions_addclosefrom_np`，运行时找；找不到（更老的 glibc、musl）时不关，只靠各处打开描述符
/// 时设的 close-on-exec。
#[cfg(target_os = "linux")]
fn close_from(actions: &mut FileActions, from: libc::c_int) -> io::Result<()> {
    type AddCloseFrom = unsafe extern "C" fn(*mut libc::posix_spawn_file_actions_t, libc::c_int) -> libc::c_int;
    // SAFETY: 名字是以 NUL 结尾的常量；`RTLD_DEFAULT` 在已经载入的库里找。
    let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"posix_spawn_file_actions_addclosefrom_np".as_ptr()) };
    if symbol.is_null() {
        return Ok(());
    }
    // SAFETY: glibc 里这个名字的函数就是这个签名。
    let add = unsafe { std::mem::transmute::<*mut libc::c_void, AddCloseFrom>(symbol) };
    // SAFETY: `actions` 已经初始化。
    check(unsafe { add(actions.as_mut_ptr(), from) })
}

#[cfg(unix)]
impl Drop for FileActions {
    fn drop(&mut self) {
        // SAFETY: 由 `new` 初始化，只销毁这一次。
        unsafe { libc::posix_spawn_file_actions_destroy(&mut self.0) };
    }
}

/// 初始化过的 `posix_spawnattr_t`，丢掉时销毁。
#[cfg(unix)]
struct SpawnAttr(libc::posix_spawnattr_t);

#[cfg(unix)]
impl SpawnAttr {
    fn new() -> io::Result<Self> {
        // SAFETY: 同 `FileActions::new`。
        let mut attr: libc::posix_spawnattr_t = unsafe { std::mem::zeroed() };
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

#[cfg(unix)]
impl Drop for SpawnAttr {
    fn drop(&mut self) {
        // SAFETY: 由 `new` 初始化，只销毁这一次。
        unsafe { libc::posix_spawnattr_destroy(&mut self.0) };
    }
}
