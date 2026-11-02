//! 经用户自己装的 gh 读写 GitHub：在仓库目录里跑 gh，出错时整理成给人看的原因；读、开分支上的 PR。
//! 仓库是哪个、登录用哪个账号都由 gh 按 git 远端和自己的配置定。这里不碰界面。

use std::{
    ffi::OsString,
    io,
    path::PathBuf,
    process::{Command, Output},
    time::Duration,
};

use serde::{Deserialize, de::DeserializeOwned};

use crate::subprocess::RunError;

pub const PROGRAM: &str = "gh";
/// 找不到 gh 时 `Gh` 返回的错，页上据此显示怎么装。
pub(crate) const MISSING: &str = "gh not found";
/// gh 没登录或者令牌失效时 `Gh` 返回的错，页上据此请用户登录。
pub(crate) const LOGIN: &str = "gh auth required";
/// gh 一次最多跑多久，到点连同它拉起的进程一起结束：gh 或凭据 helper 卡住时查询报错，不会一直在读。
const TIMEOUT: Duration = Duration::from_secs(60);

/// 一个分支上的 PR，`gh pr view` 读到的。
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PullRequest {
    pub number: u64,
    /// `OPEN`、`CLOSED` 或 `MERGED`。
    pub state: String,
    pub url: String,
    /// 开出来的时刻，Unix 时间的秒数。
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub is_draft: bool,
    /// `APPROVED`、`CHANGES_REQUESTED`、`REVIEW_REQUIRED`；仓库不要求评审时为空。
    #[serde(default)]
    pub review_decision: String,
    #[serde(default, rename = "statusCheckRollup")]
    checks: Vec<Check>,
}

/// PR 上的一项检查：GitHub Actions 的是 check run（`status`、`conclusion`），别家 CI 报的是 commit status（`state`）。
#[derive(Clone, Debug, PartialEq, Deserialize)]
struct Check {
    #[serde(default)]
    status: String,
    #[serde(default)]
    conclusion: String,
    #[serde(default)]
    state: String,
}

/// PR 的检查合起来怎么样。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Checks {
    None,
    Pending,
    Passing,
    Failing,
}

impl PullRequest {
    /// 有一项失败就算失败，否则有一项没跑完就算在跑。
    pub fn checks(&self) -> Checks {
        let failed = |check: &Check| {
            matches!(
                check.conclusion.as_str(),
                "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE"
            ) || matches!(check.state.as_str(), "FAILURE" | "ERROR")
        };
        let pending = |check: &Check| {
            (!check.status.is_empty() && check.status != "COMPLETED")
                || matches!(check.state.as_str(), "PENDING" | "EXPECTED")
        };
        if self.checks.is_empty() {
            Checks::None
        } else if self.checks.iter().any(failed) {
            Checks::Failing
        } else if self.checks.iter().any(pending) {
            Checks::Pending
        } else {
            Checks::Passing
        }
    }
}

/// 在 `dir` 里跑 gh；`path` 是终端里 shell 报告的 PATH，从访达打开的 app 自己的 PATH 里没有 brew 装的目录。
#[derive(Clone, Debug)]
pub(crate) struct Gh {
    pub dir: PathBuf,
    pub path: Option<OsString>,
}

impl Gh {
    /// `branch` 上最近的 PR，没有时为 `None`。
    pub fn pull_request(&self, branch: &str) -> Result<Option<PullRequest>, String> {
        const FIELDS: &str = "number,state,url,createdAt,isDraft,reviewDecision,statusCheckRollup";
        match self.json(&["pr", "view", branch, "--json", FIELDS, "--jq", ".createdAt |= fromdateiso8601"]) {
            Ok(pr) => Ok(Some(pr)),
            Err(err) if err.starts_with("no pull requests found") => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// 给已经推上去的 `branch` 开 PR，标题和正文按分支上的提交填。
    pub fn create_pull_request(&self, branch: &str) -> Result<(), String> {
        self.output(&["pr", "create", "--head", branch, "--fill"]).map(drop)
    }

    /// `gh api` 分页读一个列表，`key` 是回复里装列表的那个字段。
    pub(crate) fn api_list<T: DeserializeOwned>(&self, endpoint: &str, key: &str) -> Result<Vec<T>, String> {
        let jq = format!(".{key}[]");
        let out = self.output(&["api", "--paginate", endpoint, "--jq", &jq])?;
        // `--jq` 每个元素占一行。
        String::from_utf8_lossy(&out)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).map_err(|err| format!("{PROGRAM} api {endpoint}: {err}")))
            .collect()
    }

    pub(crate) fn json<T: DeserializeOwned>(&self, args: &[&str]) -> Result<T, String> {
        let out = self.output(args)?;
        serde_json::from_slice(&out).map_err(|err| format!("{PROGRAM} {}: {err}", args[..2].join(" ")))
    }

    pub(crate) fn output(&self, args: &[&str]) -> Result<Vec<u8>, String> {
        // 目录没了时 spawn 报的也是 NotFound，不能当成没装 gh。
        if !self.dir.is_dir() {
            return Err(format!("{PROGRAM}: {} does not exist", self.dir.display()));
        }
        let mut command = Command::new(PROGRAM);
        if let Some(path) = &self.path {
            command.env("PATH", path);
        }
        command
            .args(args)
            .current_dir(&self.dir)
            // 不在终端里，gh 别问话、别检查更新、别上色。
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("NO_COLOR", "1");
        let output = output_within(&mut command, TIMEOUT)?;
        if output.status.success() {
            Ok(output.stdout)
        } else if needs_login(output.status.code(), &output.stderr) {
            Err(LOGIN.to_owned())
        } else {
            Err(failure(&output.stderr))
        }
    }
}

/// 跑完 `command` 拿输出（`subprocess::output_within`），没拿到时是给用户看的原因。
fn output_within(command: &mut Command, timeout: Duration) -> Result<Output, String> {
    crate::subprocess::output_within(command, timeout).map_err(|err| match err {
        RunError::Spawn(err) => spawn_error(err),
        RunError::Wait(err) => format!("{PROGRAM}: {err}"),
        RunError::Timeout => format!("{PROGRAM} did not finish in {} s", timeout.as_secs()),
    })
}

fn spawn_error(err: io::Error) -> String {
    match err.kind() {
        io::ErrorKind::NotFound => MISSING.to_owned(),
        _ => format!("{PROGRAM}: {err}"),
    }
}

/// 要登录：没登录时 gh 的退出码是 4；令牌失效或被撤销时退出码是 1，stderr 里是 HTTP 401。
fn needs_login(code: Option<i32>, stderr: &[u8]) -> bool {
    code == Some(4) || String::from_utf8_lossy(stderr).contains("HTTP 401")
}

/// gh 出错时 stderr 的第一行就是原因（没登录时是「请运行 gh auth login」）。
fn failure(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text.lines().map(str::trim).find(|line| !line.is_empty());
    line.map_or_else(|| format!("{PROGRAM} failed"), str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 检查有一项失败就算失败，没失败但有没跑完的算在跑；check run 和 commit status 都认。
    #[test]
    fn pull_request_checks_roll_up() {
        let pr = |checks: &str| {
            let json = format!(r#"{{"number":1,"state":"OPEN","url":"u","statusCheckRollup":[{checks}]}}"#);
            serde_json::from_str::<PullRequest>(&json).unwrap().checks()
        };
        let done = r#"{"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS"}"#;
        assert_eq!(pr(""), Checks::None);
        assert_eq!(pr(done), Checks::Passing);
        assert_eq!(pr(&format!(r#"{done},{{"status":"IN_PROGRESS","conclusion":""}}"#)), Checks::Pending);
        assert_eq!(pr(&format!(r#"{done},{{"state":"PENDING"}}"#)), Checks::Pending);
        assert_eq!(pr(r#"{"state":"ERROR"},{"status":"QUEUED"}"#), Checks::Failing);
        assert_eq!(pr(r#"{"status":"COMPLETED","conclusion":"TIMED_OUT"}"#), Checks::Failing);
    }

    #[test]
    fn error_is_the_first_line_of_stderr() {
        assert_eq!(
            failure(b"\nTo get started with GitHub CLI, please run:  gh auth login\nAlternatively..."),
            "To get started with GitHub CLI, please run:  gh auth login"
        );
        assert_eq!(failure(b""), "gh failed");
    }

    #[test]
    fn asks_to_log_in_when_signed_out_or_the_token_is_bad() {
        assert!(needs_login(Some(4), b"To get started with GitHub CLI, please run:  gh auth login"));
        assert!(needs_login(Some(1), b"failed to get runs: HTTP 401: Bad credentials (https://api.github.com/...)"));
        assert!(!needs_login(Some(1), b"HTTP 404: Not Found"));
    }

    #[cfg(unix)]
    #[test]
    fn a_stuck_command_is_killed_at_the_timeout() {
        let started = std::time::Instant::now();
        let err = output_within(Command::new("/bin/sh").args(["-c", "sleep 30"]), Duration::from_millis(200));
        assert_eq!(err.unwrap_err(), "gh did not finish in 0 s");
        assert!(started.elapsed() < Duration::from_secs(10));
        let out = output_within(Command::new("/bin/sh").args(["-c", "echo hi; echo no >&2"]), TIMEOUT).unwrap();
        assert!(out.status.success());
        assert_eq!((&out.stdout[..], &out.stderr[..]), (&b"hi\n"[..], &b"no\n"[..]));
        assert_eq!(output_within(&mut Command::new("/nonexistent/gh"), TIMEOUT).unwrap_err(), MISSING);
    }
}
