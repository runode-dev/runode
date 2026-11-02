//! 只打印结果地跑一次 agent：提示词放进参数还是经标准输入给，限时跑完、取输出。

use std::time::{Duration, Instant};
#[cfg(unix)]
use std::{fs, path::Path, thread};

use runode_agent::oneshot::{self, RunError, run, with_prompt};

/// 测试里等程序跑完的时限，正常跑完都远用不到。
const TIMEOUT: Duration = Duration::from_secs(180);

#[test]
fn puts_the_prompt_on_argv_or_stdin() {
    let (args, stdin) = oneshot::CLAUDE.command_line(vec!["--model".into(), "haiku".into()], "P");
    assert_eq!(args.last().map(String::as_str), Some("haiku"));
    assert_eq!(stdin.as_deref(), Some("P"));
    let (args, stdin) = oneshot::ANTIGRAVITY.command_line(vec![], "-P");
    assert_eq!(args, ["--sandbox", "--print=-P"]);
    assert_eq!(stdin, None);
    let (args, stdin) = with_prompt(vec!["--ask".into(), "x{prompt}".into()], "P");
    assert_eq!((args, stdin), (vec!["--ask".to_owned(), "xP".to_owned()], None));
    let (args, _) = oneshot::MUSE.command_line(vec!["--model".into(), "m".into()], "P");
    assert_eq!(&args[args.len() - 4..], ["--model", "m", "--", "P"]);
}

/// 提示词经标准输入给，比管道的缓冲大也不卡；失败时报标准错误。
#[cfg(unix)]
#[test]
fn runs_a_command_with_the_prompt_on_stdin() {
    let dir = std::env::temp_dir();
    let prompt = "x".repeat(200_000);
    let out = run("sh", &["-c".into(), "wc -c".into()], Some(prompt), &dir, None, &[], TIMEOUT).unwrap();
    assert_eq!(out.trim(), "200000");
    let err = run("sh", &["-c".into(), "echo boom >&2; exit 3".into()], None, &dir, None, &[], TIMEOUT).unwrap_err();
    assert!(matches!(&err, RunError::Failed(detail) if detail == "boom"), "{err:?}");
    let missing = run("runode-no-such-agent", &[], None, &dir, None, &[], TIMEOUT).unwrap_err();
    assert!(matches!(missing, RunError::Missing), "{missing:?}");
    let env = [("RUNODE_TEST_VAR".to_owned(), "a b".to_owned())];
    let out = run("sh", &["-c".into(), "echo \"$RUNODE_TEST_VAR\"".into()], None, &dir, None, &env, TIMEOUT).unwrap();
    assert_eq!(out, "a b\n");
}

/// Windows 上同样：提示词经标准输入给，比管道的缓冲大也不卡；失败时报标准错误；到时限连子进程一起
/// 结束。
#[cfg(windows)]
#[test]
fn runs_a_command_with_the_prompt_on_stdin_on_windows() {
    let dir = std::env::temp_dir();
    let prompt = format!("{}\n", "x".repeat(99)).repeat(2000);
    let out = run("findstr", &["^".into()], Some(prompt), &dir, None, &[], TIMEOUT).unwrap();
    assert_eq!(out.lines().count(), 2000);
    let err =
        run("cmd", &["/c".into(), "(echo boom)1>&2 & exit 3".into()], None, &dir, None, &[], TIMEOUT).unwrap_err();
    assert!(matches!(&err, RunError::Failed(detail) if detail == "boom"), "{err:?}");
    let started = Instant::now();
    let ping = ["/c".into(), "ping -n 30 127.0.0.1".into()];
    let err = run("cmd", &ping, None, &dir, None, &[], Duration::from_secs(1)).unwrap_err();
    assert!(matches!(err, RunError::Timeout), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[cfg(unix)]
#[test]
fn asks_the_login_shell_for_its_path() {
    let path = oneshot::login_shell_path("sh", &std::env::temp_dir()).unwrap();
    assert!(std::env::split_paths(&path).any(|dir| dir == Path::new("/bin")));
}

/// 到时限连 agent 拉起的子进程一起杀掉；agent 自己退了、后台子进程还拿着输出管道时，到时限也不再等。
#[cfg(unix)]
#[test]
fn kills_the_whole_process_group_at_the_deadline() {
    let dir = std::env::temp_dir();
    let timeout = Duration::from_secs(1);
    let started = Instant::now();
    let out = run("sh", &["-c".into(), "sleep 30 & echo hi".into()], None, &dir, None, &[], timeout).unwrap();
    assert_eq!(out, "hi\n");
    let err = run("sh", &["-c".into(), "sleep 30 & wait".into()], None, &dir, None, &[], timeout).unwrap_err();
    assert!(matches!(err, RunError::Timeout), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(10));
}

/// 拿着输出管道的后台进程自己另起了进程组、杀整组杀不到它时，到时限稍等一会儿就报超时，不一直等。
#[cfg(unix)]
#[test]
fn gives_up_on_pipes_held_outside_the_process_group() {
    let dir = std::env::temp_dir();
    let started = Instant::now();
    // `set -m` 让后台的 `sleep` 自成一个进程组。指名用 bash：`sh` 是 dash 时（Debian、Ubuntu）没有终端
    // 就开不了作业控制，后台进程留在组里被杀掉，测不到要测的情形。
    let script = "set -m; sleep 5 & echo hi";
    let err = run("bash", &["-c".into(), script.into()], None, &dir, None, &[], Duration::from_secs(1)).unwrap_err();
    assert!(matches!(err, RunError::Timeout), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(4));
}

/// 那样的后代一直拿着输出管道时，`run` 返回时把管道也关上了：没有读线程替它收着，它之后再写
/// 只会写不进去（EPIPE），不会每重试一次就漏一份。
#[cfg(unix)]
#[test]
fn closes_pipes_held_outside_the_process_group() {
    let dir = std::env::temp_dir();
    let marker = dir.join(format!("runode-agent-oneshot-pipes-{}", std::process::id()));
    fs::remove_file(&marker).ok();
    // 后台的子 shell 自成一个进程组、不理 SIGPIPE，等 `run` 返回以后往标准输出写一行，把写不写得
    // 进去记到 `$1` 里。和上面一样指名用 bash 开作业控制。
    let script = r#"set -m; (trap '' PIPE; sleep 5; if echo late 2>/dev/null; then r=open; else r=closed; fi
        echo $r > "$1") & echo hi"#;
    let args = ["-c".into(), script.into(), "sh".into(), marker.display().to_string()];
    let started = Instant::now();
    let err = run("bash", &args, None, &dir, None, &[], Duration::from_secs(1)).unwrap_err();
    assert!(matches!(err, RunError::Timeout), "{err:?}");
    // 要在子 shell 写之前返回，下面看到的才是返回以后的样子。
    assert!(started.elapsed() < Duration::from_secs(4));
    let result = loop {
        if let Ok(text) = fs::read_to_string(&marker)
            && text.ends_with('\n')
        {
            break text;
        }
        assert!(started.elapsed() < Duration::from_secs(20), "the background shell never reported");
        thread::sleep(Duration::from_millis(50));
    };
    fs::remove_file(&marker).ok();
    // 写不进去的那行 bash 的 `echo` 留在自己的缓冲里，会跟着下一个 `echo` 一起写进文件，只看最后一行。
    assert_eq!(result.lines().last(), Some("closed"), "{result}");
}
