//! 动态补全：规格里的生成器给出一条 shell 命令，在 shell 当前目录下跑它，把输出解析成候选。
//!
//! 命令在后台线程里用 `/bin/sh -c` 跑，stdin 接空，stderr 丢掉，自成一个进程组；超时或者
//! 调用方不再需要结果（丢掉 `Job`）时连同它拉起的子进程一起杀掉。
//!
//! 有的生成器把输入里的词不加转义就拼进命令，所以词里有 shell 元字符时干脆不跑，免得按一下
//! Tab 就执行了粘贴进来的 `$(...)`。

use std::{
    ffi::OsString,
    io::Read as _,
    os::unix::process::CommandExt as _,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use warp_command_signatures::{Generator, GeneratorProcess, GeneratorResults, Shell};

/// 生成器命令最多跑这么久，超时就杀掉，当作没有结果。
pub const TIMEOUT: Duration = Duration::from_secs(2);
/// 最多读这么多输出，多出来的不要。
const MAX_OUTPUT: u64 = 4 * 1024 * 1024;
/// 等命令结束时查看取消标记的间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// 生成器要跑的命令。`tokens` 是从命令名起到当前词的各个词（当前词只取光标前的部分），
/// `trailing_space` 表示当前词还是空的，`env` 是写在命令前面的变量赋值。
///
/// 要用输入拼命令的生成器，在任何一个词里有 shell 元字符或控制字符、或者变量赋值的值不是
/// 只由安全字符组成时为 `None`，不跑。
pub fn command(generator: &Generator, tokens: &[&str], trailing_space: bool, env: &[String]) -> Option<String> {
    match &generator.process {
        GeneratorProcess::ShellCommand(builder) => Some(builder.build(Shell::Posix).into_owned()),
        GeneratorProcess::CommandFromTokens(build) => {
            if !tokens.iter().all(|token| safe_token(token)) || !env.iter().all(|e| safe_assignment(e)) {
                return None;
            }
            Some(build(tokens, trailing_space, env).build(Shell::Posix).into_owned())
        }
    }
}

/// 拼进命令也执行不了别的东西的词：没有引号、命令替换、变量、分隔、重定向、子 shell、
/// 反斜杠和控制字符。
fn safe_token(token: &str) -> bool {
    !token.chars().any(|c| {
        matches!(c, '\'' | '"' | '`' | '$' | ';' | '&' | '|' | '<' | '>' | '(' | ')' | '\\') || c.is_control()
    })
}

/// `NAME=value`，值只由字母、数字和 `_-./:,+@%=` 组成。
fn safe_assignment(assignment: &str) -> bool {
    let Some((_, value)) = assignment.split_once('=') else {
        return false;
    };
    super::line::is_assignment(assignment)
        && value.chars().all(|c| c.is_ascii_alphanumeric() || "_-./:,+@%=".contains(c))
}

/// 生成器命令在哪里跑。
pub struct Environment {
    /// shell 的当前目录；不存在时根本不跑。
    pub cwd: PathBuf,
    /// shell 集成报告的 PATH；没报告过时为 `None`，沿用 runode 自己的。
    pub path: Option<OsString>,
}

/// 跑着的一条生成器命令；丢掉它就取消，命令还没结束时会被杀掉。
pub struct Job {
    cancelled: Arc<AtomicBool>,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

/// 在后台线程里按 `env` 跑 `command`，结果用 `parse` 解析后从返回的接收端送出。超时或者
/// 跑不起来时收到空结果；被取消、解析时 panic 或者线程起不来时接收端直接断开。
pub fn spawn(
    command: String,
    env: Environment,
    parse: fn(&str) -> GeneratorResults,
) -> (Job, futures::channel::oneshot::Receiver<GeneratorResults>) {
    let cancelled = Arc::new(AtomicBool::new(false));
    let (tx, rx) = futures::channel::oneshot::channel();
    let flag = cancelled.clone();
    let spawned = std::thread::Builder::new().name("completion-generator".into()).spawn(move || {
        let output = run(&command, &env, &flag);
        if flag.load(Ordering::Relaxed) {
            return;
        }
        let results = match output {
            Some(output) => parse(&output),
            None => GeneratorResults::default(),
        };
        let _ = tx.send(results);
    });
    if let Err(err) = spawned {
        tracing::warn!("failed to start a completion generator thread: {err}");
    }
    (Job { cancelled }, rx)
}

/// shell 报告的 PATH 里以 `/` 开头的那些目录。空的一项、`.` 和别的相对路径都相对于生成器命令
/// 运行的目录，也就是用户当前所在、可能是刚下载下来的目录，那里的程序不该被补全悄悄运行，一律
/// 去掉。一个都不剩时为 `None`，沿用 runode 自己的 PATH：空的 PATH 同样表示当前目录。
fn absolute_dirs(path: &std::ffi::OsStr) -> Option<OsString> {
    let dirs: Vec<PathBuf> = std::env::split_paths(path).filter(|dir| dir.is_absolute()).collect();
    if dirs.is_empty() {
        return None;
    }
    std::env::join_paths(dirs).ok()
}

/// 跑命令拿到标准输出；超时、被取消或者跑不起来时为 `None`。
fn run(command: &str, env: &Environment, cancelled: &AtomicBool) -> Option<String> {
    if !env.cwd.is_dir() {
        return None;
    }
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(command).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    // 自成一个进程组，超时时整组杀掉，管道里的其他命令也一起结束。
    cmd.process_group(0);
    cmd.current_dir(&env.cwd);
    if let Some(path) = env.path.as_deref().and_then(absolute_dirs) {
        cmd.env("PATH", path);
    }
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            tracing::debug!("failed to run completion generator `{command}`: {err}");
            return None;
        }
    };
    let pid = child.id() as libc::pid_t;
    let kill = || {
        // SAFETY: 只是给进程组发信号；组号就是刚拉起的子进程的 pid。
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    };

    // 另起一个线程读输出：命令输出很多时，不读就会卡在写满的管道上。
    let mut stdout = child.stdout.take()?;
    let (out_tx, out_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut stdout).take(MAX_OUTPUT).read_to_end(&mut buf);
        let _ = out_tx.send(buf);
    });

    let deadline = Instant::now() + TIMEOUT;
    let output = loop {
        if cancelled.load(Ordering::Relaxed) || Instant::now() >= deadline {
            kill();
            let _ = child.wait();
            return None;
        }
        match out_rx.recv_timeout(POLL_INTERVAL) {
            Ok(output) => break output,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break Vec::new(),
        }
    };
    // 输出读完了，进程多半也结束了；没结束的（比如输出超长）不再等。
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline && !cancelled.load(Ordering::Relaxed) => {
                std::thread::sleep(POLL_INTERVAL);
            }
            _ => {
                kill();
                let _ = child.wait();
                break;
            }
        }
    }
    Some(String::from_utf8_lossy(&output).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(out: &str) -> GeneratorResults {
        GeneratorResults {
            suggestions: out.lines().map(warp_command_signatures::Suggestion::new).collect(),
            is_ordered: true,
        }
    }

    fn wait(rx: futures::channel::oneshot::Receiver<GeneratorResults>) -> Option<GeneratorResults> {
        futures::executor::block_on(rx).ok()
    }

    fn here() -> Environment {
        Environment { cwd: "/".into(), path: None }
    }

    #[test]
    fn runs_in_the_directory_with_the_shell_path() {
        let env = Environment { cwd: "/".into(), path: Some("/bin:/usr/bin:/reported".into()) };
        let (_job, rx) = spawn("pwd; echo \"$PATH\"; echo err >&2".into(), env, lines);
        let names: Vec<String> = wait(rx).unwrap().suggestions.into_iter().map(|s| s.exact_string).collect();
        assert_eq!(names, ["/", "/bin:/usr/bin:/reported"]);
        // 相对路径和空的一项都去掉，只剩相对路径时沿用 runode 自己的 PATH。
        let env = Environment { cwd: "/".into(), path: Some(".:/bin::rel/bin:/usr/bin:".into()) };
        let (_job, rx) = spawn("echo \"$PATH\"".into(), env, lines);
        let names: Vec<String> = wait(rx).unwrap().suggestions.into_iter().map(|s| s.exact_string).collect();
        assert_eq!(names, ["/bin:/usr/bin"]);
        assert_eq!(absolute_dirs(".::bin".as_ref()), None);
        // 目录不存在时不跑，也不退回别的目录。
        let env = Environment { cwd: "/nonexistent-runode-dir".into(), path: None };
        let (_job, rx) = spawn("echo ran".into(), env, lines);
        assert!(wait(rx).unwrap().suggestions.is_empty());
    }

    #[test]
    fn a_panicking_parser_disconnects() {
        fn boom(_: &str) -> GeneratorResults {
            panic!("bad output");
        }
        let (_job, rx) = spawn("echo x".into(), here(), boom);
        assert!(wait(rx).is_none());
    }

    #[test]
    fn tokens_with_shell_syntax_are_not_run() {
        let generator = Generator::command_from_tokens(
            |tokens, _, _| warp_command_signatures::CommandBuilder::single_command(format!("echo {}", tokens.join(" "))),
            lines,
        );
        assert_eq!(command(&generator, &["docker", "ps"], true, &[]).as_deref(), Some("echo docker ps"));
        for bad in ["$(touch x)", "`id`", "a;b", "a|b", "a&b", "a>b", "a\\b", "'a'", "\"a\"", "a\nb", "a\x1bb", "(a)"] {
            assert_eq!(command(&generator, &["docker", bad], false, &[]), None, "{bad:?}");
        }
        assert!(command(&generator, &["x"], true, &["DOCKER_HOST=tcp://h:2375".into()]).is_some());
        assert_eq!(command(&generator, &["x"], true, &["A=$(id)".into()]), None);
        assert_eq!(command(&generator, &["x"], true, &["A=b c".into()]), None);
        assert_eq!(command(&generator, &["x"], true, &["-x=1".into()]), None);
        // 不用输入的生成器照常跑。
        let fixed = Generator::script(warp_command_signatures::CommandBuilder::single_command("ls"), lines);
        assert_eq!(command(&fixed, &["$(id)"], false, &[]).as_deref(), Some("ls"));
    }

    #[test]
    fn stdin_is_empty_and_slow_commands_time_out() {
        let (_job, rx) = spawn("cat; echo done".into(), here(), lines);
        assert_eq!(wait(rx).unwrap().suggestions.len(), 1);
        let started = Instant::now();
        let (_job, rx) = spawn("sleep 10 | cat; echo late".into(), here(), lines);
        assert!(wait(rx).unwrap().suggestions.is_empty());
        assert!(started.elapsed() < TIMEOUT + Duration::from_secs(1));
    }

    #[test]
    fn dropping_the_job_cancels_it() {
        let (job, rx) = spawn("sleep 10".into(), here(), lines);
        let started = Instant::now();
        drop(job);
        assert!(wait(rx).is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
