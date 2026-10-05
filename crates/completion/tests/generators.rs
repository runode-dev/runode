//! 动态补全的生成器：解析时 panic 就断开，词里有 shell 语法时不跑，标准输入是空的、慢命令
//! 超时，丢掉 `Job` 就取消。

use std::time::{Duration, Instant};

use runode_completion::{
    GeneratorResults,
    generators::{Environment, TIMEOUT, command, spawn},
};
use warp_command_signatures::Generator;

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
