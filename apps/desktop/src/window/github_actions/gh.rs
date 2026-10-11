//! 经用户自己装的 gh 读写 GitHub Actions：列运行、job 和 step，列工作流，列仓库、组织和部署环境的
//! secret 和 variable，重跑、取消运行，删 secret 和 variable；要用户在终端里看着或答话的（看日志、
//! 盯着运行、触发工作流时填输入、填 secret 的值）拼成命令行交给终端去跑。这里不碰界面。
//!
//! gh 在仓库目录里跑，仓库是哪个、登录用哪个账号都由它按 git 远端和自己的配置定。

use std::{
    ffi::OsString,
    fs, io,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use serde::{Deserialize, de::DeserializeOwned};

use crate::window::files::shell_quote;

pub(super) const PROGRAM: &str = "gh";
/// 找不到 gh 时 `Gh` 返回的错，页上据此显示怎么装。
pub(super) const MISSING: &str = "gh not found";
/// gh 没登录或者令牌失效时 `Gh` 返回的错，页上据此请用户登录。
pub(super) const LOGIN: &str = "gh auth required";
/// 仓库属于个人、不属于组织时 `Gh::org_settings_url` 返回的错。
pub(super) const NOT_ORG: &str = "repository is not owned by an organization";
/// 在终端里登录 GitHub：gh 一步步问登哪个主机、用浏览器还是令牌。
pub(super) const LOGIN_COMMAND: &str = "gh auth login";
/// 列运行时最多列几条。
const RUN_LIMIT: &str = "20";
const RUN_FIELDS: &str = "databaseId,number,displayTitle,workflowName,status,conclusion,event,headBranch,attempt,url";

/// 一次工作流运行，`gh run list` 的一行。
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Run {
    #[serde(rename = "databaseId")]
    pub id: u64,
    pub number: u64,
    pub display_title: String,
    pub workflow_name: String,
    pub status: String,
    pub conclusion: String,
    pub event: String,
    pub head_branch: String,
    /// 第几次尝试，重跑一次加一。
    pub attempt: u32,
    pub url: String,
}

impl Run {
    pub fn state(&self) -> State {
        State::of(&self.status, &self.conclusion)
    }
}

/// 运行里的一个 job。
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Job {
    #[serde(rename = "databaseId")]
    pub id: u64,
    pub name: String,
    pub status: String,
    pub conclusion: String,
    pub url: String,
    #[serde(default)]
    pub steps: Vec<Step>,
}

impl Job {
    pub fn state(&self) -> State {
        State::of(&self.status, &self.conclusion)
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub(super) struct Step {
    pub name: String,
    pub status: String,
    pub conclusion: String,
}

impl Step {
    pub fn state(&self) -> State {
        State::of(&self.status, &self.conclusion)
    }
}

/// 仓库里的一个工作流，`gh workflow list` 的一行。
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub(super) struct Workflow {
    pub id: u64,
    pub name: String,
    /// 相对仓库根的工作流文件，`.github/workflows/ci.yml`。
    pub path: String,
    /// `active`，或者 `disabled_manually` 这类停用的状态。
    pub state: String,
    /// 能手动触发（有 `workflow_dispatch`），见 `dispatchable`。
    #[serde(skip)]
    pub dispatch: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub(super) struct Variable {
    pub name: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Deserialize)]
struct Named {
    name: String,
}

/// 运行、job 和 step 的状态，图标和颜色跟着它；GitHub 的 status 和 conclusion 合起来定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum State {
    Success,
    Failure,
    Cancelled,
    Skipped,
    InProgress,
    Queued,
    /// 等人批准，或者等部署环境的保护规则。
    Waiting,
    /// 已经结束，要人批准以后才会真的跑（比如来自 fork 的 PR）；图标和等待一样，但不会再变，也取消不了。
    ActionRequired,
    Pending,
}

impl State {
    fn of(status: &str, conclusion: &str) -> Self {
        match (status, conclusion) {
            ("completed", "success" | "neutral") => Self::Success,
            ("completed", "cancelled") => Self::Cancelled,
            ("completed", "skipped" | "stale") => Self::Skipped,
            ("completed", "action_required") => Self::ActionRequired,
            ("waiting" | "action_required", _) => Self::Waiting,
            ("completed", _) => Self::Failure,
            ("in_progress", _) => Self::InProgress,
            ("queued" | "requested", _) => Self::Queued,
            _ => Self::Pending,
        }
    }

    /// 还没跑完，要接着刷新。
    pub fn running(self) -> bool {
        matches!(self, Self::InProgress | Self::Queued | Self::Waiting | Self::Pending)
    }
}

/// secret 和 variable 在哪一级：仓库、组织（只读，列的是这个仓库能用的）或某个部署环境。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Scope {
    Repo,
    Org,
    Env(String),
}

/// 页上要向 GitHub 读的一份数据。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Query {
    /// 这个分支最近的运行。
    BranchRuns(String),
    Workflows,
    /// 这个工作流最近的运行。
    WorkflowRuns(u64),
    /// 一次运行第几次尝试的 job 和 step。
    Jobs(u64, u32),
    Secrets(Scope),
    Variables(Scope),
    Environments,
}

/// 读到的数据，种类跟着 `Query`。
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Data {
    Runs(Vec<Run>),
    Workflows(Vec<Workflow>),
    Jobs(Vec<Job>),
    /// secret 和部署环境只有名字。
    Names(Vec<String>),
    Variables(Vec<Variable>),
}

impl Data {
    /// 里面有没跑完的运行或 job。
    pub fn running(&self) -> bool {
        match self {
            Self::Runs(runs) => runs.iter().any(|run| run.state().running()),
            Self::Jobs(jobs) => jobs.iter().any(|job| job.state().running()),
            _ => false,
        }
    }
}

/// 在 `dir` 里跑 gh；`path` 是终端里 shell 报告的 PATH，从访达打开的 app 自己的 PATH 里没有 brew 装的目录。
#[derive(Clone, Debug)]
pub(super) struct Gh {
    pub dir: PathBuf,
    pub path: Option<OsString>,
}

impl Gh {
    pub fn fetch(&self, query: &Query) -> Result<Data, String> {
        Ok(match query {
            Query::BranchRuns(branch) => {
                Data::Runs(self.json(&["run", "list", "-b", branch, "-L", RUN_LIMIT, "--json", RUN_FIELDS])?)
            }
            Query::WorkflowRuns(id) => {
                let id = id.to_string();
                Data::Runs(self.json(&["run", "list", "-w", &id, "-L", RUN_LIMIT, "--json", RUN_FIELDS])?)
            }
            Query::Workflows => {
                let mut workflows: Vec<Workflow> =
                    self.json(&["workflow", "list", "--all", "-L", "200", "--json", "id,name,path,state"])?;
                for workflow in &mut workflows {
                    // 本地没有这个文件（只在别的分支上）时让 gh 去判断。
                    let source = fs::read_to_string(self.dir.join(&workflow.path));
                    workflow.dispatch = source.map_or(true, |source| dispatchable(&source));
                }
                Data::Workflows(workflows)
            }
            Query::Jobs(run, attempt) => {
                #[derive(Deserialize)]
                struct Jobs {
                    jobs: Vec<Job>,
                }
                let (run, attempt) = (run.to_string(), attempt.to_string());
                let jobs: Jobs = self.json(&["run", "view", &run, "--attempt", &attempt, "--json", "jobs"])?;
                Data::Jobs(jobs.jobs)
            }
            Query::Secrets(Scope::Org) => {
                Data::Names(names(self.api_list("repos/{owner}/{repo}/actions/organization-secrets", "secrets")?))
            }
            Query::Variables(Scope::Org) => {
                Data::Variables(self.api_list("repos/{owner}/{repo}/actions/organization-variables", "variables")?)
            }
            Query::Secrets(scope) => {
                let mut args = vec!["secret", "list", "--json", "name"];
                args.extend(env_args(scope));
                Data::Names(names(self.json(&args)?))
            }
            Query::Variables(scope) => {
                let mut args = vec!["variable", "list", "--json", "name,value"];
                args.extend(env_args(scope));
                Data::Variables(self.json(&args)?)
            }
            Query::Environments => {
                Data::Names(names(self.api_list("repos/{owner}/{repo}/environments", "environments")?))
            }
        })
    }

    pub fn rerun(&self, run: u64) -> Result<(), String> {
        self.output(&["run", "rerun", &run.to_string()]).map(drop)
    }

    pub fn cancel(&self, run: u64) -> Result<(), String> {
        self.output(&["run", "cancel", &run.to_string()]).map(drop)
    }

    /// 删掉 `scope` 那一级叫 `name` 的 secret（`secret` 为真）或 variable。
    pub fn delete(&self, secret: bool, scope: &Scope, name: &str) -> Result<(), String> {
        let mut args = vec![if secret { "secret" } else { "variable" }, "delete", name];
        args.extend(env_args(scope));
        self.output(&args).map(drop)
    }

    /// 仓库所属组织在 GitHub 上管 secret（`secret` 为真）或 variable 的设置页；组织那一级页上只读，要改到那里去改。
    pub fn org_settings_url(&self, secret: bool) -> Result<String, String> {
        let jq = r#".owner | select(.type == "Organization") | .html_url"#;
        let out = self.output(&["api", "repos/{owner}/{repo}", "--jq", jq])?;
        org_settings_url(String::from_utf8_lossy(&out).trim(), secret).ok_or_else(|| NOT_ORG.to_owned())
    }

    /// `gh api` 分页读一个列表，`key` 是回复里装列表的那个字段。
    fn api_list<T: DeserializeOwned>(&self, endpoint: &str, key: &str) -> Result<Vec<T>, String> {
        let jq = format!(".{key}[]");
        let out = self.output(&["api", "--paginate", endpoint, "--jq", &jq])?;
        // `--jq` 每个元素占一行。
        String::from_utf8_lossy(&out)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).map_err(|err| format!("{PROGRAM} api {endpoint}: {err}")))
            .collect()
    }

    fn json<T: DeserializeOwned>(&self, args: &[&str]) -> Result<T, String> {
        let out = self.output(args)?;
        serde_json::from_slice(&out).map_err(|err| format!("{PROGRAM} {}: {err}", args[..2].join(" ")))
    }

    fn output(&self, args: &[&str]) -> Result<Vec<u8>, String> {
        // 目录没了时 spawn 报的也是 NotFound，不能当成没装 gh。
        if !self.dir.is_dir() {
            return Err(format!("{PROGRAM}: {} does not exist", self.dir.display()));
        }
        let mut command = Command::new(PROGRAM);
        if let Some(path) = &self.path {
            command.env("PATH", path);
        }
        let output = command
            .args(args)
            .current_dir(&self.dir)
            // 不在终端里，gh 别问话、别检查更新、别上色。
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .output()
            .map_err(spawn_error)?;
        if output.status.success() {
            Ok(output.stdout)
        } else if needs_login(output.status.code(), &output.stderr) {
            Err(LOGIN.to_owned())
        } else {
            Err(failure(&output.stderr))
        }
    }
}

fn names(named: Vec<Named>) -> Vec<String> {
    named.into_iter().map(|named| named.name).collect()
}

/// 组织主页 `https://<主机>/<组织>` 换成它管 Actions secret 或 variable 的设置页，主机照搬，GitHub Enterprise 也对。
fn org_settings_url(org_page: &str, secret: bool) -> Option<String> {
    let (host, org) = org_page.trim_end_matches('/').rsplit_once('/').filter(|(_, org)| !org.is_empty())?;
    let kind = if secret { "secrets" } else { "variables" };
    Some(format!("{host}/organizations/{org}/settings/{kind}/actions"))
}

fn env_args(scope: &Scope) -> Vec<&str> {
    match scope {
        Scope::Env(env) => vec!["--env", env],
        Scope::Repo | Scope::Org => Vec::new(),
    }
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

/// 有没跑完的运行或 job、或者等着登录时隔多久重读，别的隔多久重读。
const POLL_RUNNING: Duration = Duration::from_secs(10);
const POLL_IDLE: Duration = Duration::from_secs(60);

/// 每秒问一次：上次读到 `data`、在 `fetched` 读完的一份数据到 `now` 该不该重读。没读过的马上读；跑完的
/// 那次尝试的 job 不会再变，读到了就不再读；等登录的按快的间隔读，在终端里登好后页上自己恢复。差不到
/// 一秒算到了，免得每次都晚一秒。
pub(super) fn due(data: Option<&Result<Data, String>>, fetched: Option<Instant>, now: Instant) -> bool {
    let every = match data {
        // 排队的运行可能还没有 job，空的不算跑完。
        Some(Ok(Data::Jobs(jobs))) if !jobs.is_empty() && !jobs.iter().any(|job| job.state().running()) => {
            return false;
        }
        Some(Ok(data)) if data.running() => POLL_RUNNING,
        Some(Err(err)) if err == LOGIN => POLL_RUNNING,
        _ => POLL_IDLE,
    };
    fetched.is_none_or(|at| now.duration_since(at) + Duration::from_secs(1) >= every)
}

/// 在终端里看一个跑完的 job 的整份日志，gh 交给分页器显示。
pub(super) fn logs_command(job: u64) -> String {
    format!("{PROGRAM} run view --log --job {job}")
}

/// 在终端里盯着一次运行，跑完为止。
pub(super) fn watch_command(run: u64) -> String {
    format!("{PROGRAM} run watch {run}")
}

/// 在终端里触发工作流：有输入参数时 gh 一个个问。
pub(super) fn trigger_command(workflow: u64, git_ref: Option<&str>) -> String {
    match git_ref {
        Some(git_ref) => format!("{PROGRAM} workflow run {workflow} --ref {}", shell_quote(git_ref)),
        None => format!("{PROGRAM} workflow run {workflow}"),
    }
}

/// 在终端里设 secret（`secret` 为真）或 variable 的值：gh 提示粘贴，secret 的值不回显，也不进命令行和历史。
pub(super) fn set_command(secret: bool, scope: &Scope, name: &str) -> String {
    let kind = if secret { "secret" } else { "variable" };
    let mut command = format!("{PROGRAM} {kind} set {}", shell_quote(name));
    if let Scope::Env(env) = scope {
        command.push_str(&format!(" --env {}", shell_quote(env)));
    }
    command
}

/// 工作流文件里有没有 `workflow_dispatch` 触发，有才能手动触发。
pub(super) fn dispatchable(source: &str) -> bool {
    // shortcut: 不解析 YAML，按字面找；注释里写了它也算，要按 on: 精确判断时再引 YAML 解析。
    source.contains("workflow_dispatch")
}

/// 一个 step 在网页上的位置：job 页加上 step 的序号（从 1 数）。
pub(super) fn step_url(job: &Job, index: usize) -> String {
    format!("{}#step:{}:1", job.url, index + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn org_settings_url_keeps_the_host() {
        assert_eq!(
            org_settings_url("https://github.com/acme", true).as_deref(),
            Some("https://github.com/organizations/acme/settings/secrets/actions")
        );
        assert_eq!(
            org_settings_url("https://ghe.example.com/acme", false).as_deref(),
            Some("https://ghe.example.com/organizations/acme/settings/variables/actions")
        );
        assert_eq!(
            org_settings_url("https://github.com/acme/", true).as_deref(),
            Some("https://github.com/organizations/acme/settings/secrets/actions")
        );
        // 仓库属于个人时 jq 什么也不输出。
        assert_eq!(org_settings_url("", true), None);
    }

    #[test]
    fn states_follow_status_and_conclusion() {
        assert_eq!(State::of("completed", "success"), State::Success);
        assert_eq!(State::of("completed", "failure"), State::Failure);
        assert_eq!(State::of("completed", "startup_failure"), State::Failure);
        assert_eq!(State::of("completed", "timed_out"), State::Failure);
        assert_eq!(State::of("completed", "cancelled"), State::Cancelled);
        assert_eq!(State::of("completed", "skipped"), State::Skipped);
        assert_eq!(State::of("in_progress", ""), State::InProgress);
        assert_eq!(State::of("queued", ""), State::Queued);
        assert_eq!(State::of("waiting", ""), State::Waiting);
        assert_eq!(State::of("completed", "action_required"), State::ActionRequired);
        assert!(!State::of("completed", "action_required").running());
        assert_eq!(State::of("pending", ""), State::Pending);
        assert!(State::of("pending", "").running());
        assert!(!State::of("completed", "failure").running());
    }

    #[test]
    fn parses_runs_and_jobs() {
        let runs: Vec<Run> = serde_json::from_str(
            r#"[{"attempt":2,"conclusion":"","databaseId":7,"displayTitle":"fix","event":"push","headBranch":"main",
                "number":12,"status":"in_progress","url":"https://x/7","workflowDatabaseId":3,"workflowName":"CI"}]"#,
        )
        .unwrap();
        assert_eq!(runs[0].id, 7);
        assert_eq!(runs[0].attempt, 2);
        assert_eq!(runs[0].state(), State::InProgress);
        assert!(Data::Runs(runs).running());

        let job: Job = serde_json::from_str(
            r#"{"databaseId":9,"name":"check","status":"completed","conclusion":"success","url":"https://x/job/9",
                "steps":[{"name":"Set up job","number":1,"status":"completed","conclusion":"success"}]}"#,
        )
        .unwrap();
        assert_eq!(job.steps[0].state(), State::Success);
        assert_eq!(step_url(&job, 0), "https://x/job/9#step:1:1");
    }

    #[test]
    fn polls_running_and_waiting_for_login_fast_and_finished_jobs_never() {
        let t0 = Instant::now();
        let at = |secs| t0 + Duration::from_secs(secs);
        let runs = |status: &str| {
            let runs = format!(
                r#"[{{"attempt":1,"conclusion":"","databaseId":7,"displayTitle":"x","event":"push","headBranch":"main",
                    "number":1,"status":"{status}","url":"u","workflowName":"CI"}}]"#
            );
            Ok(Data::Runs(serde_json::from_str(&runs).unwrap()))
        };
        // 没读过的马上读。
        assert!(due(None, None, t0));
        // 在跑的十秒一次，读完一秒内不再读。
        assert!(!due(Some(&runs("in_progress")), Some(t0), at(1)));
        assert!(due(Some(&runs("in_progress")), Some(t0), at(9)));
        // 跑完的一分钟一次。
        assert!(!due(Some(&runs("completed")), Some(t0), at(30)));
        assert!(due(Some(&runs("completed")), Some(t0), at(59)));
        // 等登录的十秒一次，别的错一分钟一次。
        assert!(due(Some(&Err(LOGIN.to_owned())), Some(t0), at(9)));
        assert!(!due(Some(&Err("HTTP 404".to_owned())), Some(t0), at(9)));
        // 跑完的那次尝试的 job 不再读，还在跑的照读。
        let jobs = |status: &str| {
            let job = format!(r#"{{"databaseId":9,"name":"c","status":"{status}","conclusion":"","url":"u"}}"#);
            Ok(Data::Jobs(vec![serde_json::from_str(&job).unwrap()]))
        };
        assert!(!due(Some(&jobs("completed")), Some(t0), at(3600)));
        assert!(due(Some(&jobs("in_progress")), Some(t0), at(9)));
        // 还没有 job 的（刚排上队）照常重读。
        assert!(due(Some(&Ok(Data::Jobs(Vec::new()))), Some(t0), at(59)));
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

    #[test]
    fn terminal_commands_quote_what_the_user_named() {
        assert_eq!(trigger_command(5, Some("feat/x y")), "gh workflow run 5 --ref 'feat/x y'");
        assert_eq!(trigger_command(5, None), "gh workflow run 5");
        assert_eq!(set_command(true, &Scope::Repo, "TOKEN"), "gh secret set TOKEN");
        assert_eq!(set_command(false, &Scope::Env("prod env".into()), "URL"), "gh variable set URL --env 'prod env'");
        assert_eq!(logs_command(9), "gh run view --log --job 9");
    }

    #[test]
    fn finds_the_dispatch_trigger() {
        assert!(dispatchable("on:\n  workflow_dispatch:\n  push:\n"));
        assert!(!dispatchable("on: push\n"));
    }
}
