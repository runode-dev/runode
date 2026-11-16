//! 定时先跑命令用的内置脚本：照宿主的拉起方式跑本机那份，gh 换成按参数打印样例的假程序，核对输出和去重状态。

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use runode_todo::run_scripts::RUN_SCRIPTS;

/// 一个用例的目录：`scripts` 放脚本（状态记在它下面的 `state/t1`），`bin` 放假 gh，`fixtures` 放假 gh 打印的样例。
struct Sandbox {
    root: PathBuf,
}

/// 假 gh：每次把参数追加到 `FAKE_GH_LOG`；按前两个参数（第三个不以 `-` 开头时也算上，第四个是 `--comments` 时再
/// 接 `-comments`）挑 `FAKE_GH_DIR` 里的样例打印，样例不在或设了 `FAKE_GH_FAIL` 时退出 1。
#[cfg(unix)]
const FAKE_GH: (&str, &str) = (
    "gh",
    r#"#!/bin/sh
printf '[%s]' "$@" >>"$FAKE_GH_LOG"
printf '\n' >>"$FAKE_GH_LOG"
[ -z "${FAKE_GH_FAIL:-}" ] || exit 1
name="$1-$2"
case ${3:-} in ''|-*) ;; *) name="$name-$3" ;; esac
if [ "${4:-}" = --comments ]; then name="$name-comments"; fi
[ -f "$FAKE_GH_DIR/$name.txt" ] || exit 1
cat "$FAKE_GH_DIR/$name.txt"
"#,
);

#[cfg(windows)]
const FAKE_GH: (&str, &str) = (
    "gh.cmd",
    "@echo off\r\n\
     >>\"%FAKE_GH_LOG%\" echo %*\r\n\
     if defined FAKE_GH_FAIL exit /b 1\r\n\
     set \"NAME=%~1-%~2\"\r\n\
     set \"THIRD=%~3\"\r\n\
     if not defined THIRD goto print\r\n\
     if \"%THIRD:~0,1%\"==\"-\" goto print\r\n\
     set \"NAME=%NAME%-%THIRD%\"\r\n\
     :print\r\n\
     if \"%~4\"==\"--comments\" set \"NAME=%NAME%-comments\"\r\n\
     if not exist \"%FAKE_GH_DIR%\\%NAME%.txt\" exit /b 1\r\n\
     type \"%FAKE_GH_DIR%\\%NAME%.txt\"\r\n\
     exit /b 0\r\n",
);

impl Sandbox {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("runode-run-scripts-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["scripts", "bin", "fixtures"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        for script in RUN_SCRIPTS {
            let (file, text) = script.for_this_os();
            std::fs::write(root.join("scripts").join(file), text).unwrap();
        }
        let gh = root.join("bin").join(FAKE_GH.0);
        std::fs::write(&gh, FAKE_GH.1).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self { root }
    }

    fn fixture(&self, name: &str, text: &str) {
        std::fs::write(self.root.join("fixtures").join(format!("{name}.txt")), text).unwrap();
    }

    fn remove_fixture(&self, name: &str) {
        std::fs::remove_file(self.root.join("fixtures").join(format!("{name}.txt"))).unwrap();
    }

    fn run(&self, script: &str, args: &[&str]) -> Output {
        self.run_with(script, args, false)
    }

    fn run_with(&self, script: &str, args: &[&str], gh_fails: bool) -> Output {
        let script = RUN_SCRIPTS.iter().find(|item| item.name == script).unwrap();
        let path = self.root.join("scripts").join(script.for_this_os().0);
        let mut command = shell_command(&script_command(&path, args));
        let mut dirs = vec![self.root.join("bin")];
        dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
        command
            .current_dir(&self.root)
            .env("PATH", std::env::join_paths(dirs).unwrap())
            .env("RUNODE_SCHEDULE", "t1")
            .env("FAKE_GH_DIR", self.root.join("fixtures"))
            .env("FAKE_GH_LOG", self.root.join("gh.log"))
            .env_remove("FAKE_GH_FAIL");
        if gh_fails {
            command.env("FAKE_GH_FAIL", "1");
        }
        command.output().unwrap()
    }

    fn state(&self, file: &str) -> String {
        std::fs::read_to_string(self.root.join("scripts").join("state").join("t1").join(file)).unwrap_or_default()
    }

    fn gh_log(&self) -> String {
        std::fs::read_to_string(self.root.join("gh.log")).unwrap_or_default()
    }
}

/// 照宿主跑 `Schedule::run` 的方式拉起 `run`，脚本在这里过了，到宿主那里才一样能跑。
#[cfg(unix)]
fn shell_command(run: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(run);
    command
}

#[cfg(windows)]
fn shell_command(run: &str) -> Command {
    use std::os::windows::process::CommandExt as _;
    let mut command = Command::new("cmd.exe");
    command.args(["/D", "/S", "/C"]).raw_arg(format!("\"{run}\""));
    command
}

/// 定时模板里调脚本的写法。
#[cfg(unix)]
fn script_command(path: &Path, args: &[&str]) -> String {
    let mut run = format!("sh '{}'", path.display());
    for arg in args {
        write!(run, " '{arg}'").unwrap();
    }
    run
}

#[cfg(windows)]
fn script_command(path: &Path, args: &[&str]) -> String {
    let mut run = format!("powershell -NoProfile -ExecutionPolicy Bypass -File \"{}\"", path.display());
    for arg in args {
        write!(run, " \"{arg}\"").unwrap();
    }
    run
}

/// 假 gh 记下的一串参数：Unix 上每个参数包在方括号里，Windows 上是 cmd 交给批处理的原样命令行，带空格的参数带引号。
fn logged(args: &[&str]) -> String {
    if cfg!(windows) {
        args.iter()
            .map(|arg| if arg.contains(' ') { format!("\"{arg}\"") } else { (*arg).to_owned() })
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        args.iter().map(|arg| format!("[{arg}]")).collect()
    }
}

/// 标准输出，按宿主的读法：截断处切开的字符换成 U+FFFD。
fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[track_caller]
fn assert_ok(output: &Output) {
    assert!(output.status.success(), "{:?}: {}", output.status, String::from_utf8_lossy(&output.stderr));
}

/// 跑一次，要求成功，返回标准输出。
#[track_caller]
fn fire(sandbox: &Sandbox, script: &str, args: &[&str]) -> String {
    let output = sandbox.run(script, args);
    assert_ok(&output);
    stdout(&output)
}

fn failed_log(text: &str) -> String {
    format!(
        "build\tRun tests\t\u{feff}2024-05-01T10:00:00.1234567Z {text}\nbuild\tRun tests\t2024-05-01T10:00:01.5Z exit 1\n"
    )
}

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

#[test]
fn watch_ci_is_quiet_when_everything_passes() {
    let sandbox = Sandbox::new("ci-pass");
    sandbox.fixture("run-list", &format!("12\tCI\t{SHA}\tcompleted\tsuccess\n11\tLint\t{SHA}\tcompleted\tneutral\n"));
    assert_eq!(fire(&sandbox, "watch-ci", &["main"]), "");
    assert_eq!(sandbox.state("pending"), "");
}

#[test]
fn watch_ci_fires_once_for_a_new_failure() {
    let sandbox = Sandbox::new("ci-once");
    sandbox.fixture("run-list", &format!("21\t构建 CI\t{SHA}\tcompleted\tfailure\n20\tLint\t{SHA}\tcompleted\tsuccess\n"));
    sandbox.fixture("run-view-21", &failed_log("error: boom"));
    let out = fire(&sandbox, "watch-ci", &["main"]);
    assert!(out.starts_with("These GitHub Actions workflows failed on main."), "{out}");
    assert!(out.contains("## 构建 CI (run 21, commit 012345678, failure)\nerror: boom\nexit 1\n"), "{out}");
    assert!(!out.contains("Lint"), "{out}");
    assert_eq!(sandbox.state("pending"), "构建 CI\n");
    assert_eq!(fire(&sandbox, "watch-ci", &["main"]), "");
    assert_eq!(sandbox.state("pending"), "构建 CI\n");
}

#[test]
fn watch_ci_skips_cancelled_runs_to_the_last_conclusion() {
    let sandbox = Sandbox::new("ci-cancelled");
    sandbox.fixture(
        "run-list",
        &format!("33\tCI\t{SHA}\tcompleted\tcancelled\n32\tCI\t{SHA}\tcompleted\tskipped\n31\tCI\t{SHA}\tcompleted\tfailure\n30\tCI\t{SHA}\tcompleted\tsuccess\n"),
    );
    sandbox.fixture("run-view-31", &failed_log("broken"));
    let out = fire(&sandbox, "watch-ci", &["main"]);
    assert!(out.contains("## CI (run 31, "), "{out}");
    assert!(out.contains("broken"), "{out}");
}

/// 最新的一次还在跑（前面几次被它取消了）时不派更早的失败，pending 照旧；跑完失败了才派。
#[test]
fn watch_ci_waits_for_a_run_in_progress() {
    let sandbox = Sandbox::new("ci-in-progress");
    std::fs::create_dir_all(sandbox.root.join("scripts/state/t1")).unwrap();
    std::fs::write(sandbox.root.join("scripts/state/t1/pending"), "Lint\n").unwrap();
    sandbox.fixture(
        "run-list",
        &format!(
            "93\tCI\t{SHA}\tin_progress\t\n92\tLint\t{SHA}\tqueued\t\n91\tCI\t{SHA}\tcompleted\tcancelled\n\
             90\tCI\t{SHA}\tcompleted\tfailure\n89\tLint\t{SHA}\tcompleted\tsuccess\n"
        ),
    );
    sandbox.fixture("run-view-90", &failed_log("stale"));
    assert_eq!(fire(&sandbox, "watch-ci", &["main"]), "");
    assert_eq!(sandbox.state("pending"), "Lint\n");

    sandbox.fixture(
        "run-list",
        &format!("93\tCI\t{SHA}\tcompleted\tfailure\n91\tCI\t{SHA}\tcompleted\tcancelled\n"),
    );
    sandbox.fixture("run-view-93", &failed_log("fresh"));
    let out = fire(&sandbox, "watch-ci", &["main"]);
    assert!(out.contains("## CI (run 93, ") && out.contains("fresh"), "{out}");
}

/// 等人批准的运行（waiting、action_required）不挡住更早的失败。
#[test]
fn watch_ci_looks_past_runs_waiting_for_approval() {
    let sandbox = Sandbox::new("ci-waiting");
    sandbox.fixture(
        "run-list",
        &format!(
            "53\tCI\t{SHA}\twaiting\t\n52\tCI\t{SHA}\tcompleted\taction_required\n51\tCI\t{SHA}\tcompleted\tfailure\n"
        ),
    );
    sandbox.fixture("run-view-51", &failed_log("still broken"));
    let out = fire(&sandbox, "watch-ci", &["main"]);
    assert!(out.contains("## CI (run 51, ") && out.contains("still broken"), "{out}");
}

/// 日志里 runner 收尾的输出和 action 的标记不占末尾的行数，留下的是报错。
#[test]
fn watch_ci_drops_the_runner_cleanup_from_logs() {
    let sandbox = Sandbox::new("ci-cleanup");
    sandbox.fixture("run-list", &format!("95\tCI\t{SHA}\tcompleted\tfailure\n"));
    let mut log = String::new();
    for (job, text) in [
        ("check", "error: formatting differs"),
        ("check", "##[error]Process completed with exit code 2."),
        ("check", "Post job cleanup."),
        ("check", "[command]/opt/homebrew/bin/git version"),
        ("test", "##[start-action display=Run actions/cache;id=1]"),
        ("test", "test failed: boom"),
        ("test", "##[end-action id=1;outcome=skipped]"),
        ("test", "Post job cleanup."),
        ("test", "Cleaning up orphan processes"),
    ] {
        writeln!(log, "{job}\tUNKNOWN STEP\t2024-05-01T10:00:00.0000000Z {text}").unwrap();
    }
    sandbox.fixture("run-view-95", &log);
    let out = fire(&sandbox, "watch-ci", &["main"]);
    assert!(
        out.ends_with(
            "## CI (run 95, commit 012345678, failure)\nerror: formatting differs\n\
             ##[error]Process completed with exit code 2.\ntest failed: boom\n"
        ),
        "{out}"
    );
}

#[test]
fn watch_ci_fires_again_after_the_workflow_passed() {
    let sandbox = Sandbox::new("ci-refire");
    sandbox.fixture("run-list", &format!("41\tCI\t{SHA}\tcompleted\tfailure\n"));
    sandbox.fixture("run-view-41", &failed_log("first"));
    assert!(fire(&sandbox, "watch-ci", &["main"]).contains("first"));

    sandbox.fixture("run-list", &format!("42\tCI\t{SHA}\tcompleted\tsuccess\n41\tCI\t{SHA}\tcompleted\tfailure\n"));
    assert_eq!(fire(&sandbox, "watch-ci", &["main"]), "");
    assert_eq!(sandbox.state("pending"), "");

    sandbox.fixture("run-list", &format!("43\tCI\t{SHA}\tcompleted\ttimed_out\n42\tCI\t{SHA}\tcompleted\tsuccess\n41\tCI\t{SHA}\tcompleted\tfailure\n"));
    sandbox.fixture("run-view-43", &failed_log("second"));
    let out = fire(&sandbox, "watch-ci", &["main"]);
    assert!(out.contains("## CI (run 43, commit 012345678, timed_out)\nsecond"), "{out}");
}

#[test]
fn watch_ci_fires_at_most_three_workflows_at_a_time() {
    let sandbox = Sandbox::new("ci-three");
    let mut list = String::new();
    for (id, name) in (51..).zip(["A", "B", "C", "D", "E"]) {
        writeln!(list, "{id}\t{name}\t{SHA}\tcompleted\tfailure").unwrap();
        sandbox.fixture(&format!("run-view-{id}"), &failed_log(&format!("log of {name}")));
    }
    sandbox.fixture("run-list", &list);
    let out = fire(&sandbox, "watch-ci", &["main"]);
    assert_eq!(out.matches("\n## ").count(), 3, "{out}");
    assert!(out.contains("log of C") && !out.contains("log of D"), "{out}");
    let mut pending: Vec<_> = sandbox.state("pending").lines().map(str::to_owned).collect();
    pending.sort();
    assert_eq!(pending, ["A", "B", "C"]);

    let out = fire(&sandbox, "watch-ci", &["main"]);
    assert_eq!(out.matches("\n## ").count(), 2, "{out}");
    assert!(out.contains("log of D") && out.contains("log of E"), "{out}");
}

#[test]
fn watch_ci_keeps_long_logs_short() {
    let sandbox = Sandbox::new("ci-long");
    let mut log = String::new();
    for line in 0..500 {
        writeln!(log, "job\tstep\t2024-05-01T10:00:00.0000000Z {line:04}{}", "x".repeat(996)).unwrap();
    }
    let mut list = String::new();
    for id in 61..65 {
        writeln!(list, "{id}\tWorkflow {id}\t{SHA}\tcompleted\tfailure").unwrap();
        sandbox.fixture(&format!("run-view-{id}"), &log);
    }
    sandbox.fixture("run-list", &list);
    let out = fire(&sandbox, "watch-ci", &["main"]);
    assert!(out.len() < 40 * 1024, "{} bytes", out.len());
    assert!(out.contains(&format!("0499{}", "x".repeat(296))), "keeps the end of the log");
    assert!(out.contains("0460") && !out.contains("0459"), "keeps only the end of the log");
    assert!(out.lines().all(|line| line.len() <= 300));
}

#[test]
fn watch_ci_fails_when_gh_fails() {
    let sandbox = Sandbox::new("ci-gh-fails");
    sandbox.fixture("run-list", &format!("71\tCI\t{SHA}\tcompleted\tfailure\n"));
    let output = sandbox.run_with("watch-ci", &["main"], true);
    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    assert_eq!(sandbox.state("pending"), "");
}

#[test]
fn watch_ci_watches_the_default_branch_without_an_argument() {
    let sandbox = Sandbox::new("ci-default-branch");
    sandbox.fixture("repo-view", "trunk\n");
    sandbox.fixture("run-list", &format!("81\tCI\t{SHA}\tcompleted\tsuccess\n"));
    assert_eq!(fire(&sandbox, "watch-ci", &[]), "");
    assert!(sandbox.gh_log().contains(&logged(&["--branch", "trunk"])), "{}", sandbox.gh_log());
}

fn review_fixtures(sandbox: &Sandbox, list: &[(u32, &str)]) {
    let mut text = String::new();
    for (number, head) in list {
        writeln!(text, "{number}\t{head}").unwrap();
        sandbox.fixture(&format!("pr-view-{number}"), &format!("title:\tPR {number}\n--\nbody of {number}\n"));
        sandbox.fixture(&format!("pr-diff-{number}"), &format!("src/file{number}.rs\n"));
    }
    sandbox.fixture("pr-list", &text);
}

#[test]
fn review_requests_go_out_one_at_a_time_lowest_number_first() {
    let sandbox = Sandbox::new("pr-order");
    review_fixtures(&sandbox, &[(7, "sha7"), (3, "sha3"), (5, "sha5")]);
    let out = fire(&sandbox, "review-requests", &[]);
    assert_eq!(out, "Pull request #3 (head sha3)\n\ntitle:\tPR 3\n--\nbody of 3\n\nFiles changed:\nsrc/file3.rs\n");
    assert!(fire(&sandbox, "review-requests", &[]).starts_with("Pull request #5 (head sha5)\n"));
    assert!(fire(&sandbox, "review-requests", &[]).starts_with("Pull request #7 (head sha7)\n"));
    assert_eq!(fire(&sandbox, "review-requests", &[]), "");
    assert!(
        sandbox.gh_log().contains(&logged(&["--search", "draft:false review-requested:@me"])),
        "{}",
        sandbox.gh_log()
    );
}

#[test]
fn review_requests_fire_again_when_the_head_moves() {
    let sandbox = Sandbox::new("pr-head");
    review_fixtures(&sandbox, &[(3, "sha3")]);
    assert!(fire(&sandbox, "review-requests", &[]).starts_with("Pull request #3 (head sha3)\n"));
    assert_eq!(fire(&sandbox, "review-requests", &[]), "");
    review_fixtures(&sandbox, &[(3, "sha3b")]);
    assert!(fire(&sandbox, "review-requests", &[]).starts_with("Pull request #3 (head sha3b)\n"));
    assert_eq!(sandbox.state("seen"), "3 sha3b\n");
}

#[test]
fn review_requests_forget_pull_requests_that_left_the_list() {
    let sandbox = Sandbox::new("pr-forget");
    review_fixtures(&sandbox, &[(3, "sha3"), (4, "sha4")]);
    fire(&sandbox, "review-requests", &[]);
    fire(&sandbox, "review-requests", &[]);
    assert_eq!(sandbox.state("seen"), "3 sha3\n4 sha4\n");
    review_fixtures(&sandbox, &[(4, "sha4")]);
    assert_eq!(fire(&sandbox, "review-requests", &[]), "");
    assert_eq!(sandbox.state("seen"), "4 sha4\n");
}

#[test]
fn review_requests_do_not_mark_a_pull_request_when_gh_fails_midway() {
    let sandbox = Sandbox::new("pr-midway");
    review_fixtures(&sandbox, &[(3, "sha3")]);
    sandbox.remove_fixture("pr-diff-3");
    let output = sandbox.run("review-requests", &[]);
    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    assert_eq!(sandbox.state("seen"), "");
}

#[test]
fn new_issues_go_out_one_at_a_time() {
    let sandbox = Sandbox::new("issues");
    sandbox.fixture("issue-list", "12\n9\n");
    sandbox.fixture("issue-view-9", "title:\tCrash\n--\nsteps\n");
    sandbox.fixture("issue-view-9-comments", "author:\tme\n--\nme too\n");
    sandbox.fixture("issue-view-12", "title:\tTypo\n--\nfix it\n");
    sandbox.fixture("issue-view-12-comments", "");
    assert_eq!(
        fire(&sandbox, "new-issues", &["good first"]),
        "Issue #9\n\ntitle:\tCrash\n--\nsteps\n\nComments:\nauthor:\tme\n--\nme too\n"
    );
    assert!(fire(&sandbox, "new-issues", &["good first"]).starts_with("Issue #12\n"));
    assert_eq!(fire(&sandbox, "new-issues", &["good first"]), "");
    let mut seen: Vec<_> = sandbox.state("seen").lines().map(str::to_owned).collect();
    seen.sort();
    assert_eq!(seen, ["12", "9"]);
    assert!(sandbox.gh_log().contains(&logged(&["--label", "good first", "--limit"])), "{}", sandbox.gh_log());
}

#[test]
fn long_issues_and_pull_requests_are_cut_to_fit_the_prompt() {
    let sandbox = Sandbox::new("long");
    // 一节 24 字节，截断处落在一个汉字中间。
    let long = format!("{}\n", "长文字长文字 text ".repeat(4000));
    sandbox.fixture("issue-list", "3\n");
    sandbox.fixture("issue-view-3", &long);
    sandbox.fixture("issue-view-3-comments", &long);
    let issue = fire(&sandbox, "new-issues", &["bug"]);
    assert!(issue.len() < 40 * 1024, "{}", issue.len());
    assert!(issue.contains("[cut here; gh issue view 3 --comments shows all of them]"), "{issue}");
    assert_eq!(sandbox.state("seen"), "3\n");

    review_fixtures(&sandbox, &[(5, "abc")]);
    sandbox.fixture("pr-view-5", &long);
    sandbox.fixture("pr-diff-5", &"src/some/file.rs\n".repeat(5000));
    let pull = fire(&sandbox, "review-requests", &[]);
    assert!(pull.len() < 40 * 1024, "{}", pull.len());
    assert!(pull.contains("[cut here; gh pr diff 5 --name-only lists all of them]"), "{pull}");
}

#[test]
fn powershell_scripts_start_with_a_bom() {
    for script in RUN_SCRIPTS {
        assert!(script.ps1.starts_with('\u{feff}'), "{}", script.name);
    }
}

#[test]
fn new_issues_needs_a_label() {
    let sandbox = Sandbox::new("issues-no-label");
    let output = sandbox.run("new-issues", &[]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(stdout(&output), "");
    assert!(!output.stderr.is_empty());
    assert_eq!(sandbox.gh_log(), "");
}

/// 脚本用的 gh 列表命令在真的 gh 上照约定的列数出 TSV。要装好、登录好 gh，联网；
/// `cargo test -p runode-todo --test run_scripts -- --ignored` 手动跑。
#[test]
#[ignore = "needs a logged-in gh and the network"]
fn real_gh_prints_the_expected_columns() {
    fn gh(args: &[&str]) -> String {
        let output = Command::new("gh").args(args).env("GH_REPO", "cli/cli").output().unwrap();
        assert_ok(&output);
        String::from_utf8(output.stdout).unwrap()
    }
    #[track_caller]
    fn assert_columns(text: &str, columns: usize) {
        assert!(!text.trim().is_empty(), "no rows");
        for line in text.lines() {
            assert_eq!(line.split('\t').count(), columns, "{line:?}");
        }
    }

    // `gh repo view` 不看 GH_REPO，仓库要写在参数里。
    let branch = gh(&["repo", "view", "cli/cli", "--json", "defaultBranchRef", "--jq", ".defaultBranchRef.name"]);
    assert_columns(&branch, 1);
    let runs = gh(&[
        "run",
        "list",
        "--branch",
        branch.trim(),
        "--limit",
        "60",
        "--json",
        "databaseId,workflowName,headSha,status,conclusion",
        "--jq",
        ".[] | [.databaseId, .workflowName, .headSha, .status, .conclusion] | @tsv",
    ]);
    assert_columns(&runs, 5);
    let pr_list = |search: &str| {
        gh(&[
            "pr",
            "list",
            "--state",
            "open",
            "--search",
            search,
            "--limit",
            "100",
            "--json",
            "number,headRefOid",
            "--jq",
            ".[] | [.number, .headRefOid] | @tsv",
        ])
    };
    // 跑一遍原样的搜索，确认 gh 认得它；请自己评审的多半为空，列数拿只排除草稿的搜索核对。
    pr_list("draft:false review-requested:@me");
    assert_columns(&pr_list("draft:false"), 2);
    let issues = gh(&[
        "issue",
        "list",
        "--state",
        "open",
        "--label",
        "help wanted",
        "--limit",
        "100",
        "--json",
        "number",
        "--jq",
        ".[].number",
    ]);
    assert_columns(&issues, 1);
    assert!(issues.lines().all(|line| line.parse::<u64>().is_ok()), "{issues}");
}
