//! macOS 上经 launchd 拉起宿主：每次在 `gui/<uid>` 域里提交一个一次性的任务，宿主就有自己的
//! coalition（macOS 给进程分的组，launchd 每个任务一个，子进程继承）。
//!
//! 用 `posix_spawn` 从 app 拉起时，宿主和它开的所有 shell 都留在 app 的 coalition 里。app 退出后再打开
//! （升级重启也是），新的 app 有新的 coalition，旧的那个就没了主人；之后从终端里打开的 GUI 程序（比如
//! 开发中的 Electron）退出时，系统把这个 coalition 里还在跑的宿主和所有 shell 当成那个程序留在后台的
//! 进程，程序坞把它挂成「在后台运行」，用户点「停止在后台运行」或强制退出时一起杀掉（SIGKILL，宿主
//! 来不及留下一行日志）。宿主自己是任务的主进程时，从它的终端打开的程序退出时不带走任何进程。
//!
//! 任务仍记在提交它的 app 名下（launchd 按提交者的 coalition 记，经别的任务转交也一样），app 退出时
//! loginwindow 照样按 Runode 允不允许在后台运行处理宿主，这是系统给 app 后台工作的规矩，不绕开。
//!
//! launchd 不把描述符交给任务：任务先跑一段 `/bin/sh`，把临时目录里的 FIFO 打开到 `STATUS_FD` 上，
//! 写一行自己的 pid，再换成宿主（`exec`，pid 不变）。不要状态管道时写完 pid 就关掉它。
//!
//! 宿主退出后任务还挂在 launchd 里（不在跑），下次提交前清掉（`sweep`）。

use std::{
    ffi::{CString, OsStr},
    fmt::Write as _,
    fs::{File, OpenOptions},
    io::{self, Read as _},
    os::{
        fd::{AsRawFd as _, OwnedFd},
        unix::{
            ffi::{OsStrExt as _, OsStringExt as _},
            fs::OpenOptionsExt as _,
        },
    },
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

/// 任务 Label 的前缀，后面接拉起它的进程号和序号。重启脚本（`restart-macos.sh`）提交的任务也用它，
/// 好让这边一并清掉。
const LABEL_PREFIX: &str = "dev.runode.host.";

/// 等任务里的 shell 报上 pid 最多这么久。
const START_TIMEOUT: Duration = Duration::from_secs(10);

/// 包装：把 FIFO（`$0`）开到 fd 3（`STATUS_FD`）上，写一行 pid，再换成 `"$@"`。
const KEEP_STATUS: &str = r#"exec 3>"$0" && echo $$ >&3 && exec "$@""#;
/// 同 `KEEP_STATUS`，写完 pid 就关掉 fd 3。
const CLOSE_STATUS: &str = r#"exec 3>"$0" && echo $$ >&3 && exec 3>&- && exec "$@""#;

/// 这个进程提交过几个任务，拼进 Label。
static SUBMITTED: AtomicU64 = AtomicU64::new(0);

/// `submit` 为什么没成。
pub(super) enum Error {
    /// 这里用不了 launchd（没有图形会话、路径不是 UTF-8 这类），调用方改用 `posix_spawn`。
    Unavailable(io::Error),
    /// 提交了但没起来，或者程序本身不能执行。
    Failed(io::Error),
}

/// 提交任务跑 `exe args...`，等它报上 pid。返回 pid 和状态的读端：`keep_status` 时宿主拿着
/// `STATUS_FD` 上的写端，它写的读得到，它退出后读到结尾；不然读端已经到了结尾。
pub(super) fn submit(exe: &Path, args: &[&str], keep_status: bool) -> Result<(u32, File), Error> {
    // 程序不能执行时 launchd 照样接下任务，只是 exec 失败；先查，免得等到超时。
    access_executable(exe).map_err(Error::Failed)?;
    let exe = exe.to_str().ok_or_else(|| unavailable("the program path is not UTF-8"))?;
    let dir = TempDir::new().map_err(Error::Unavailable)?;
    let fifo = dir.0.join("status");
    let fifo_name = fifo.to_str().ok_or_else(|| unavailable("the temporary directory is not UTF-8"))?;
    make_fifo(&fifo).map_err(Error::Unavailable)?;
    let reader = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&fifo)
        .map_err(Error::Unavailable)?;
    // 任务里的 shell 打开写端之前读端没有写端，读到的是结尾；先自己拿着一个写端，读到 pid 后再放。
    let guard = OpenOptions::new().write(true).custom_flags(libc::O_CLOEXEC).open(&fifo).map_err(Error::Unavailable)?;

    // SAFETY: 只读这个进程的用户号。
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let label = format!("{LABEL_PREFIX}{}.{}", std::process::id(), SUBMITTED.fetch_add(1, Ordering::Relaxed));
    let wrapper = if keep_status { KEEP_STATUS } else { CLOSE_STATUS };
    let mut program = vec!["/bin/sh", "-c", wrapper, fifo_name, exe];
    program.extend_from_slice(args);
    let plist_file = dir.0.join("job.plist");
    std::fs::write(&plist_file, plist(&label, &program)).map_err(Error::Unavailable)?;
    let plist_file = plist_file.to_str().ok_or_else(|| unavailable("the temporary directory is not UTF-8"))?;

    sweep();
    launchctl(&["bootstrap", &domain, plist_file]).map_err(Error::Unavailable)?;
    let pid = match read_pid(&reader, Instant::now() + START_TIMEOUT) {
        Ok(pid) => pid,
        Err(err) => {
            let _ = launchctl(&["bootout", &format!("{domain}/{label}")]);
            return Err(Error::Failed(err));
        }
    };
    drop(guard);
    if !keep_status {
        return Ok((pid, reader));
    }
    // macOS 上 FIFO 的写端都关了以后 poll 既不报可读也不报挂断，`poll` 着等结果的调用方等不到结尾；阻塞着
    // 读读得到结尾。所以起一个线程阻塞着把 FIFO 里的转进一根普通管道，交出去的是管道的读端。
    // SAFETY: 只改这个描述符的状态标志。
    unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_SETFL, 0) };
    let (status, mut writer) = io::pipe().map_err(Error::Failed)?;
    let mut reader = reader;
    thread::Builder::new()
        .name("host-status".into())
        .spawn(move || {
            let _ = io::copy(&mut reader, &mut writer);
        })
        .map_err(Error::Failed)?;
    Ok((pid, File::from(OwnedFd::from(status))))
}

fn unavailable(message: &str) -> Error {
    Error::Unavailable(io::Error::other(message))
}

/// `path` 能不能执行。
fn access_executable(path: &Path) -> io::Result<()> {
    let path = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: 路径以 NUL 结尾，调用期间活着。
    if unsafe { libc::access(path.as_ptr(), libc::X_OK) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn make_fifo(path: &Path) -> io::Result<()> {
    let path = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: 同 `access_executable`。
    if unsafe { libc::mkfifo(path.as_ptr(), 0o600) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// 读 FIFO 上的第一行（任务里 shell 的 pid），最多等到 `deadline`。一个字节一个字节地读，不多读宿主
/// 接着写的状态。
fn read_pid(reader: &File, deadline: Instant) -> io::Result<u32> {
    let mut line = Vec::new();
    loop {
        let mut byte = [0u8];
        match (&mut &*reader).read(&mut byte) {
            Ok(0) => return Err(io::Error::other("the job exited before reporting its pid")),
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => line.push(byte[0]),
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "the job did not start in time"));
                }
                let mut poll = libc::pollfd { fd: reader.as_raw_fd(), events: libc::POLLIN, revents: 0 };
                let millis = libc::c_int::try_from(left.as_millis().max(1)).unwrap_or(libc::c_int::MAX);
                // SAFETY: 只有一项，指向本地变量；描述符来自 `reader`，调用期间开着。
                unsafe { libc::poll(&mut poll, 1, millis) };
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    let line = String::from_utf8_lossy(&line);
    line.trim().parse().map_err(|_| io::Error::other(format!("the job reported {line:?} as its pid")))
}

/// 清掉之前提交、宿主已经退出的任务。在跑的不碰：`launchctl list` 里 pid 一栏是 `-` 的才是不在跑的。刚提交、
/// 进程还没起来的任务 pid 一栏也是 `-`，所以提交者（Label 里头一个数）还活着的也不碰，免得清掉别人正在等的任务；
/// 它们等提交者退出后再清。
fn sweep() {
    let Ok(output) = Command::new("/bin/launchctl").arg("list").output() else { return };
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut columns = line.split('\t');
        let (Some("-"), Some(_), Some(label)) = (columns.next(), columns.next(), columns.next()) else { continue };
        let Some(rest) = label.strip_prefix(LABEL_PREFIX) else { continue };
        let submitter = rest.split('.').find_map(|part| part.parse::<libc::pid_t>().ok());
        // SAFETY: 只问进程在不在，不发信号。
        if submitter.is_some_and(|pid| unsafe { libc::kill(pid, 0) } == 0) {
            continue;
        }
        let _ = launchctl(&["remove", label]);
    }
}

fn launchctl(args: &[&str]) -> io::Result<()> {
    let output = Command::new("/bin/launchctl").args(args).output()?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(io::Error::other(format!("launchctl {} failed ({}): {}", args[0], output.status, stderr.trim())))
}

/// 任务的 plist：跑一次（不 `KeepAlive`），按前台程序的优先级跑（`Interactive`，不然 launchd 当后台任务
/// 限它的 CPU 和 I/O），宿主退出时不动它的进程组。环境变量照抄这个进程的，launchd 自己的只有几项；
/// 不是 UTF-8 或者带着 plist 写不了的控制字符的跳过。
fn plist(label: &str, program: &[&str]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n",
    );
    let _ = writeln!(out, "\t<key>Label</key>\n\t<string>{}</string>", xml_escape(label));
    out.push_str("\t<key>ProgramArguments</key>\n\t<array>\n");
    for arg in program {
        let _ = writeln!(out, "\t\t<string>{}</string>", xml_escape(arg));
    }
    out.push_str("\t</array>\n\t<key>EnvironmentVariables</key>\n\t<dict>\n");
    for (name, value) in std::env::vars_os() {
        let (Some(name), Some(value)) = (plist_text(&name), plist_text(&value)) else { continue };
        let _ = writeln!(out, "\t\t<key>{}</key>\n\t\t<string>{}</string>", xml_escape(name), xml_escape(value));
    }
    out.push_str(
        "\t</dict>\n\t<key>RunAtLoad</key>\n\t<true/>\n\t<key>ProcessType</key>\n\t<string>Interactive</string>\n\
         \t<key>AbandonProcessGroup</key>\n\t<true/>\n</dict>\n</plist>\n",
    );
    out
}

/// 能原样写进 plist 的文字：UTF-8，除了制表、换行、回车没有控制字符（XML 1.0 不允许）。
fn plist_text(text: &OsStr) -> Option<&str> {
    text.to_str().filter(|text| !text.chars().any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r')))
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// 只有自己进得去的临时目录（`mkdtemp`），丢掉时连同里面的东西删掉。FIFO 放在里面，别人换不成指向别处的
/// 符号链接；读到 pid 以后两边都打开了，删掉不碍事。
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> io::Result<Self> {
        let template = std::env::temp_dir().join("runode-host.XXXXXX");
        let mut template = CString::new(template.into_os_string().into_vec())?.into_bytes_with_nul();
        // SAFETY: 模板以 NUL 结尾，`mkdtemp` 原地改写末尾的 X。
        if unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) }.is_null() {
            return Err(io::Error::last_os_error());
        }
        template.pop();
        Ok(Self(PathBuf::from(OsStr::from_bytes(&template))))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plist_runs_once_in_the_foreground_and_escapes_its_text() {
        let plist = plist("dev.runode.host.1.0", &["/bin/sh", "-c", "a && b", "/tmp/<x>"]);
        assert!(plist.contains("<string>a &amp;&amp; b</string>"), "{plist}");
        assert!(plist.contains("<string>/tmp/&lt;x&gt;</string>"), "{plist}");
        assert!(plist.contains("<key>ProcessType</key>\n\t<string>Interactive</string>"), "{plist}");
        assert!(!plist.contains("KeepAlive"), "{plist}");
    }

    #[test]
    fn control_characters_stay_out_of_the_plist() {
        assert_eq!(plist_text(OsStr::new("a\tb\nc")), Some("a\tb\nc"));
        assert_eq!(plist_text(OsStr::new("a\u{1b}[0m")), None);
        assert_eq!(plist_text(OsStr::from_bytes(b"\xff")), None);
    }
}
