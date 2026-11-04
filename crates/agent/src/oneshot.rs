//! 让 agent 的命令行不进交互、只打印结果地跑一次：各家的调用方式（`Cli`），和在后台限时跑完、取它的
//! 输出（`run`）。这些调用都关掉工具或者只读，不留会话记录。模型不在这里选，由调用方加在参数里，比如
//! `--model sonnet`。

use std::{
    ffi::{OsStr, OsString},
    io::{self, ErrorKind, Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::{
    os::{
        fd::{AsRawFd, RawFd},
        raw::c_int,
    },
    process::{ChildStderr, ChildStdin, ChildStdout},
};

#[cfg(windows)]
use std::sync::{Arc, Condvar, Mutex, PoisonError};

/// 参数里换成提示词的占位；参数里没有它时提示词经标准输入给（提示词可能很大，命令行放不下）。
pub const PROMPT: &str = "{prompt}";

/// 问登录 shell 的 PATH 最多等这么久：配置里有等终端输入的东西时不至于卡住。
const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(10);
/// 到时限杀掉整组以后，再等输出管道关上这么久。
pub(crate) const KILL_GRACE: Duration = Duration::from_secs(1);
/// 等 agent 退出时最多隔这么久看一次。
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// agent 打印出来的是什么。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Output {
    /// 就是结果。
    Text,
    /// OpenCode 的 `--format json`：一行一个事件，结果在 `text` 事件里。
    OpenCodeEvents,
}

/// 一家 agent 只打印结果的命令行。
pub struct Cli {
    pub binary: &'static str,
    /// 调用方加的参数前面的参数。
    pub args: &'static [&'static str],
    /// 调用方加的参数后面的参数：放提示词和必须排在最后的位置参数。
    pub tail: &'static [&'static str],
    pub output: Output,
}

pub const CLAUDE: Cli = Cli {
    binary: "claude",
    // 提示词里可能夹带别人写的指令：`--tools ""` 关掉所有内置工具，`--strict-mcp-config` 不接用户配的
    // MCP 服务，它就读写不了文件、跑不了命令。
    args: &[
        "-p",
        "--output-format",
        "text",
        "--permission-mode",
        "plan",
        "--tools",
        "",
        "--strict-mcp-config",
        "--no-session-persistence",
    ],
    tail: &[],
    output: Output::Text,
};

pub const CODEX: Cli = Cli {
    binary: "codex",
    // 只读沙箱里照样能跑命令、读本机任意文件：关掉 `shell_tool` 和 `unified_exec`，它就没有执行命令的工具。
    args: &[
        "exec",
        "--ephemeral",
        "--skip-git-repo-check",
        "-s",
        "read-only",
        "--disable",
        "shell_tool",
        "--disable",
        "unified_exec",
    ],
    tail: &[],
    output: Output::Text,
};

pub const PI: Cli = Cli {
    binary: "pi",
    args: &["--print", "--no-session", "--no-tools", "--no-skills", "--no-context-files", "--mode", "text"],
    tail: &[],
    output: Output::Text,
};

pub const OMP: Cli = Cli {
    binary: "omp",
    args: &["--print", "--no-session", "--no-tools", "--no-extensions", "--no-skills", "--no-rules", "--mode", "text"],
    tail: &[],
    output: Output::Text,
};

pub const OPENCODE: Cli = Cli {
    binary: "opencode",
    args: &["run", "--agent", "plan", "--format", "json"],
    tail: &[],
    output: Output::OpenCodeEvents,
};

pub const OPENCODE2: Cli = Cli {
    binary: "opencode2",
    args: &["run", "--agent", "plan", "--format", "json"],
    tail: &[],
    output: Output::OpenCodeEvents,
};

pub const CURSOR: Cli = Cli {
    binary: "cursor-agent",
    args: &["--print", "--mode", "ask", "--trust", "--output-format", "text"],
    tail: &[PROMPT],
    output: Output::Text,
};

pub const MUSE: Cli = Cli {
    binary: "muse",
    args: &[
        "exec",
        "--no-session-log",
        "--approval-mode",
        "never",
        "--disable-sandbox",
        "--disable-shell",
        "--disable-write",
        "--disable-web-tools",
    ],
    tail: &["--", PROMPT],
    output: Output::Text,
};

pub const DSH: Cli = Cli {
    binary: "dsh",
    // `-` 是 dsh 明说的「从标准输入读」，不写它时就算标准输入是管道也报缺任务。
    args: &["--profile", "headless"],
    tail: &["-"],
    output: Output::Text,
};

pub const COPILOT: Cli = Cli {
    binary: "copilot",
    args: &["--silent", "--stream", "off", "--no-custom-instructions"],
    tail: &["--prompt", PROMPT],
    output: Output::Text,
};

pub const ANTIGRAVITY: Cli = Cli {
    binary: "agy",
    args: &["--sandbox"],
    // 写成 `--print=…`：提示词以 `-` 开头时也归 `--print`，不被当成别的选项。
    tail: &["--print={prompt}"],
    output: Output::Text,
};

impl Cli {
    /// 完整的参数（`args`、`user_args`、`tail` 依次接上，里面的 `PROMPT` 换成 `prompt`）和要经标准输入给的
    /// 提示词：参数里放了提示词时后者为空。
    pub fn command_line(&self, user_args: Vec<String>, prompt: &str) -> (Vec<String>, Option<String>) {
        let mut args: Vec<String> = self.args.iter().map(|&arg| arg.to_owned()).collect();
        args.extend(user_args);
        args.extend(self.tail.iter().map(|&arg| arg.to_owned()));
        with_prompt(args, prompt)
    }
}

/// 把参数里的 `PROMPT` 换成 `prompt`，再给出要经标准输入给的提示词：参数里放了提示词时为空。
pub fn with_prompt(args: Vec<String>, prompt: &str) -> (Vec<String>, Option<String>) {
    let on_argv = args.iter().any(|arg| arg.contains(PROMPT));
    let args = args.into_iter().map(|arg| arg.replace(PROMPT, prompt)).collect();
    (args, (!on_argv).then(|| prompt.to_owned()))
}

/// `run` 没拿到输出的原因，由调用方说给用户听。
#[derive(Debug)]
pub enum RunError {
    /// 找不到程序：没装，或者不在 PATH 里。
    Missing,
    /// 到时限没跑完，或者它退了、别的进程还一直拿着输出管道。
    Timeout,
    /// 退出码不是 0：去掉首尾空白的标准错误，它为空时是标准输出，都为空时是退出状态。
    Failed(String),
    /// 拉不起来、接不上管道、等不到退出这类别的错。
    Io(io::Error),
}

/// 用户的登录 shell `shell` 配好以后的 PATH，问不到时为空。交互加登录地起，`.zshrc` 这类只在交互时
/// 读的配置里加的目录也算上；配置里打印的别的东西在 `PATH=` 那行之前，取最后一行。Windows 上的 shell
/// 不从配置里改 PATH，不问。
pub fn login_shell_path(shell: &str, dir: &Path) -> Option<OsString> {
    if cfg!(windows) {
        return None;
    }
    let args = ["-i", "-l", "-c", "/usr/bin/env"].map(str::to_owned);
    let env = run(shell, &args, None, dir, None, &[], LOGIN_SHELL_TIMEOUT).ok()?;
    env.lines().rev().find_map(|line| line.strip_prefix("PATH=")).map(OsString::from)
}

/// 在 `dir` 里跑 `program`，`stdin` 不为空时从标准输入交给它，返回标准输出；过了 `timeout` 连同它拉起的
/// 子孙一起杀掉。`program` 由调用方找好：Windows 上 `Command` 自己只给不带目录的名字补 `.exe`。`path` 不为空
/// 时设成它的 `PATH`；`env` 加在 `PATH` 之后，里面写了 `PATH` 时用它的。
pub fn run(
    program: impl AsRef<OsStr>,
    args: &[String],
    stdin: Option<String>,
    dir: &Path,
    path: Option<OsString>,
    env: &[(String, String)],
    timeout: Duration,
) -> Result<String, RunError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(dir)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // 包装脚本拉起的真 agent、后台的 helper 到时限时一起杀掉。
    crate::process::isolate(&mut command);
    if let Some(path) = path {
        command.env("PATH", path);
    }
    command.envs(env.iter().map(|(name, value)| (name, value)));
    let mut child = command.spawn().map_err(|err| match err.kind() {
        ErrorKind::NotFound => RunError::Missing,
        _ => RunError::Io(err),
    })?;
    let group = child.id();
    let kill_group = || crate::process::kill_tree(group);
    // 提示词和输出都可能比管道的缓冲大，一边等 agent 退出一边写、读，不然两边都停下来等对方。
    let mut pipes = match Pipes::new(&mut child, stdin) {
        Ok(pipes) => pipes,
        Err(err) => {
            kill_group();
            child.wait().ok();
            return Err(RunError::Io(err));
        }
    };
    let deadline = Instant::now() + timeout;
    // 组长退了也先不收尸，杀组用的组号要一直是它的，直到下面不再杀组。
    loop {
        if crate::process::has_exited(&mut child).map_err(RunError::Io)? {
            break;
        }
        let now = Instant::now();
        if now > deadline {
            kill_group();
            child.wait().ok();
            return Err(RunError::Timeout);
        }
        pipes.pump((deadline - now).min(POLL_INTERVAL));
    }
    // agent 退了，组里还有进程拿着输出管道时读不到头：等到时限，再杀掉整组读完剩下的。拿着管道的
    // 进程自己另开了进程组或会话、杀不到它时，再等一会儿还读不到头就算超时，不一直等下去；返回时
    // `pipes` 跟着关掉，那个进程再写只会写不进去。
    let mut until = deadline;
    let mut killed = false;
    while !pipes.outputs_closed() {
        let now = Instant::now();
        if now >= until {
            if killed {
                child.wait().ok();
                return Err(RunError::Timeout);
            }
            kill_group();
            killed = true;
            until = now + KILL_GRACE;
            continue;
        }
        pipes.pump(until - now);
    }
    let status = child.wait().map_err(RunError::Io)?;
    let (stdout, stderr) = pipes.into_outputs();
    let (stdout, stderr) =
        (String::from_utf8_lossy(&stdout).into_owned(), String::from_utf8_lossy(&stderr).into_owned());
    if !status.success() {
        let detail = [stderr.trim(), stdout.trim()].into_iter().find(|text| !text.is_empty());
        return Err(RunError::Failed(detail.map_or_else(|| status.to_string(), str::to_owned)));
    }
    Ok(stdout)
}

/// `run` 拉起的 agent 的三条管道，都设成非阻塞，在当前线程里用 `poll` 一起写、读，不另起线程：
/// 拿着管道的后代杀不到时，这边到时限丢掉 `Pipes` 就关上了，没有线程卡在读写上，fd 也不漏。
#[cfg(unix)]
struct Pipes {
    stdin: Option<ChildStdin>,
    /// 要写进标准输入的提示词，和写到了哪儿。
    input: Vec<u8>,
    written: usize,
    /// 读到头或读出错时为空。
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    stdout_bytes: Vec<u8>,
    stderr_bytes: Vec<u8>,
}

#[cfg(unix)]
impl Pipes {
    fn new(child: &mut Child, input: Option<String>) -> io::Result<Self> {
        let input = input.map(String::into_bytes).unwrap_or_default();
        // 没有要写的就直接关上标准输入，agent 读到头。
        let stdin = child.stdin.take().filter(|_| !input.is_empty());
        let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
        let fds = [
            stdin.as_ref().map(AsRawFd::as_raw_fd),
            stdout.as_ref().map(AsRawFd::as_raw_fd),
            stderr.as_ref().map(AsRawFd::as_raw_fd),
        ];
        for fd in fds.into_iter().flatten() {
            set_nonblocking(fd)?;
        }
        Ok(Self { stdin, input, written: 0, stdout, stderr, stdout_bytes: Vec::new(), stderr_bytes: Vec::new() })
    }

    /// 读到的标准输出和标准错误。
    fn into_outputs(self) -> (Vec<u8>, Vec<u8>) {
        (self.stdout_bytes, self.stderr_bytes)
    }

    /// 标准输出和标准错误都读到头了。
    fn outputs_closed(&self) -> bool {
        self.stdout.is_none() && self.stderr.is_none()
    }

    /// 最多等 `wait`，等到哪条管道能写、能读（或对面关了）就写、读一次。
    fn pump(&mut self, wait: Duration) {
        let fds = [
            self.stdin.as_ref().map(|pipe| (pipe.as_raw_fd(), libc::POLLOUT)),
            self.stdout.as_ref().map(|pipe| (pipe.as_raw_fd(), libc::POLLIN)),
            self.stderr.as_ref().map(|pipe| (pipe.as_raw_fd(), libc::POLLIN)),
        ];
        let mut fds: Vec<libc::pollfd> =
            fds.into_iter().flatten().map(|(fd, events)| libc::pollfd { fd, events, revents: 0 }).collect();
        if fds.is_empty() {
            thread::sleep(wait);
            return;
        }
        // 向上取整到毫秒，剩下不到一毫秒时也睡一下，不空转。
        let millis = c_int::try_from(wait.as_micros().div_ceil(1000)).unwrap_or(c_int::MAX);
        // SAFETY: `fds` 是有效的 `pollfd` 数组，长度照实传；里面的 fd 都由 `self` 拿着，调用期间开着。
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, millis) };
        // 超时或被信号打断：由调用方看时限，再来一轮。
        if ready <= 0 {
            return;
        }
        // 不细分是哪条就绪：fd 都是非阻塞的，没就绪的那几条只会得到 `WouldBlock`。
        self.feed();
        read_some(&mut self.stdout, &mut self.stdout_bytes);
        read_some(&mut self.stderr, &mut self.stderr_bytes);
    }

    /// 往标准输入写一次；写完了或 agent 不收了（管道读端关了）就关上，和不收时不报错一样。
    fn feed(&mut self) {
        let Some(pipe) = &mut self.stdin else {
            return;
        };
        match pipe.write(&self.input[self.written..]) {
            Ok(written) => self.written += written,
            Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => return,
            Err(_) => self.written = self.input.len(),
        }
        if self.written == self.input.len() {
            self.stdin = None;
        }
    }
}

/// 从 `pipe` 读一次，读到的接在 `bytes` 后面；读到头或出错时关上，读到的照样留着。
#[cfg(unix)]
fn read_some(pipe: &mut Option<impl Read>, bytes: &mut Vec<u8>) {
    let Some(reader) = pipe else {
        return;
    };
    let mut buf = [0; 16 * 1024];
    match reader.read(&mut buf) {
        Ok(0) => *pipe = None,
        Ok(read) => bytes.extend_from_slice(&buf[..read]),
        Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
        Err(_) => *pipe = None,
    }
}

#[cfg(unix)]
fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: `fd` 是调用方拿着、开着的管道，F_GETFL 只读它的状态标志。
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    // SAFETY: 同上，F_SETFL 只改这个 fd 自己的状态标志。
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Windows 的匿名管道不能 `poll`：写标准输入、读两条输出各起一个线程，`pump` 只等它们。
/// shortcut: 杀不到的子孙一直拿着输出管道时，读的线程卡在读上，直到那个进程退出才结束；碰上了再改用
/// 重叠 I/O。
#[cfg(windows)]
struct Pipes {
    read: Arc<(Mutex<Collected>, Condvar)>,
}

/// 读到的标准输出、标准错误，和还没读到头的有几条。
#[cfg(windows)]
#[derive(Default)]
struct Collected {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    open: usize,
}

#[cfg(windows)]
impl Pipes {
    fn new(child: &mut Child, input: Option<String>) -> io::Result<Self> {
        let input = input.map(String::into_bytes).unwrap_or_default();
        // 没有要写的就直接关上标准输入，agent 读到头。
        if let Some(mut stdin) = child.stdin.take().filter(|_| !input.is_empty()) {
            // agent 不收时写会失败，和不收时不报错一样。
            thread::Builder::new().name("agent-oneshot-stdin".into()).spawn(move || drop(stdin.write_all(&input)))?;
        }
        let read = Arc::new((Mutex::new(Collected { open: 2, ..Collected::default() }), Condvar::new()));
        let reader = |pipe: Option<Box<dyn Read + Send>>, err: bool| {
            let read = read.clone();
            move || {
                if let Some(mut pipe) = pipe {
                    let mut buf = [0; 16 * 1024];
                    loop {
                        match pipe.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                let mut out = read.0.lock().unwrap_or_else(PoisonError::into_inner);
                                if err { &mut out.stderr } else { &mut out.stdout }.extend_from_slice(&buf[..n]);
                            }
                            Err(err) if err.kind() == ErrorKind::Interrupted => {}
                            Err(_) => break,
                        }
                    }
                }
                read.0.lock().unwrap_or_else(PoisonError::into_inner).open -= 1;
                read.1.notify_all();
            }
        };
        let stdout = child.stdout.take().map(|pipe| -> Box<dyn Read + Send> { Box::new(pipe) });
        let stderr = child.stderr.take().map(|pipe| -> Box<dyn Read + Send> { Box::new(pipe) });
        thread::Builder::new().name("agent-oneshot-stdout".into()).spawn(reader(stdout, false))?;
        thread::Builder::new().name("agent-oneshot-stderr".into()).spawn(reader(stderr, true))?;
        Ok(Self { read })
    }

    /// 读到的标准输出和标准错误。
    fn into_outputs(self) -> (Vec<u8>, Vec<u8>) {
        let mut out = self.read.0.lock().unwrap_or_else(PoisonError::into_inner);
        (std::mem::take(&mut out.stdout), std::mem::take(&mut out.stderr))
    }

    /// 标准输出和标准错误都读到头了。
    fn outputs_closed(&self) -> bool {
        self.read.0.lock().unwrap_or_else(PoisonError::into_inner).open == 0
    }

    /// 最多等 `wait`，有一条输出读到头就提前回来。
    fn pump(&mut self, wait: Duration) {
        let out = self.read.0.lock().unwrap_or_else(PoisonError::into_inner);
        let open = out.open;
        drop(self.read.1.wait_timeout_while(out, wait, |out| out.open == open));
    }
}
