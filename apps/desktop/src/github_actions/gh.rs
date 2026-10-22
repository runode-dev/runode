//! 经用户自己装的 gh 读写 GitHub Actions：列运行、job 和 step，列工作流，列仓库、组织和部署环境的
//! secret 和 variable，重跑、取消运行，删 secret 和 variable；要用户在终端里看着或答话的（看日志、
//! 盯着运行、触发工作流时填输入、填 secret 的值）拼成命令行交给终端去跑。看板也经它读、开任务分支上的
//! PR（`Gh::pull_request`、`Gh::create_pull_request`）。这里不碰界面。
//!
//! gh 在仓库目录里跑，仓库是哪个、登录用哪个账号都由它按 git 远端和自己的配置定。

use std::{
    ffi::OsString,
    fs,
    io::{self, Read},
    os::unix::{fs::OpenOptionsExt as _, process::CommandExt as _},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use serde::{Deserialize, de::DeserializeOwned};

pub const PROGRAM: &str = "gh";
/// 找不到 gh 时 `Gh` 返回的错，页上据此显示怎么装。
pub(super) const MISSING: &str = "gh not found";
/// gh 没登录或者令牌失效时 `Gh` 返回的错，页上据此请用户登录。
pub(super) const LOGIN: &str = "gh auth required";
/// 仓库属于个人、不属于组织时 `Gh::org_settings_url` 返回的错。
pub(super) const NOT_ORG: &str = "repository is not owned by an organization";
/// 在终端里登录 GitHub：gh 一步步问登哪个主机、用浏览器还是令牌。
pub(super) const LOGIN_COMMAND: &str = "gh auth login";
/// gh 一次最多跑多久，到点连同它拉起的进程一起结束：gh 或凭据 helper 卡住时查询报错，不会一直在读。
const TIMEOUT: Duration = Duration::from_secs(60);
/// 本地的工作流文件最多读这么多，多出来的不看。
const MAX_WORKFLOW_BYTES: u64 = 1 << 20;
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
    /// GitHub 给的编号，不一定连续（post 步骤会留空），网页上日志的锚点按它。
    pub number: u64,
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
pub(crate) struct Gh {
    pub dir: PathBuf,
    pub path: Option<OsString>,
}

impl Gh {
    pub(super) fn fetch(&self, query: &Query) -> Result<Data, String> {
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
                    let source = read_workflow(&self.dir.join(&workflow.path));
                    workflow.dispatch = source.is_none_or(|source| dispatchable(&source));
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

    pub fn rerun(&self, run: u64) -> Result<(), String> {
        self.output(&["run", "rerun", &run.to_string()]).map(drop)
    }

    pub fn cancel(&self, run: u64) -> Result<(), String> {
        self.output(&["run", "cancel", &run.to_string()]).map(drop)
    }

    /// 删掉 `scope` 那一级叫 `name` 的 secret（`secret` 为真）或 variable。
    pub(super) fn delete(&self, secret: bool, scope: &Scope, name: &str) -> Result<(), String> {
        // 组织那一级这里只读；`env_args` 对它不加范围参数，照发会删掉仓库里同名的那个。
        if matches!(scope, Scope::Org) {
            return Err(NOT_ORG.to_owned());
        }
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

/// 跑完 `command` 拿输出；`timeout` 内没跑完时杀掉它的整个进程组，返回错误。
fn output_within(command: &mut Command, timeout: Duration) -> Result<Output, String> {
    // 自成一个进程组，到点时连同拉起的凭据 helper 一起杀掉，它们可能拿着输出的管道。
    let spawned = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0).spawn();
    let mut child = spawned.map_err(spawn_error)?;
    // 输出另起线程读，多过管道的缓冲时 gh 不会卡在写上。到点时不等这两个线程：脱离了进程组的子孙进程
    // 可能还拿着管道。
    let (stdout, stderr) = (read_all(child.stdout.take()), read_all(child.stderr.take()));
    let deadline = Instant::now() + timeout;
    loop {
        // 两个管道都读到头了才收尸：收了尸它的进程号就可能给了别的进程，到点时 killpg 会杀到别人。
        let waited = if stdout.is_finished() && stderr.is_finished() { child.try_wait() } else { Ok(None) };
        match waited {
            Ok(Some(status)) => {
                let (stdout, stderr) = (stdout.join().unwrap_or_default(), stderr.join().unwrap_or_default());
                return Ok(Output { status, stdout, stderr });
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            waited => {
                // 进程组号就是 gh 的进程号（`process_group(0)`）。
                unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
                let _ = child.wait();
                return Err(match waited {
                    Err(err) => format!("{PROGRAM}: {err}"),
                    Ok(_) => format!("{PROGRAM} did not finish in {} s", timeout.as_secs()),
                });
            }
        }
    }
}

/// 在另一个线程里读完 `pipe`。
fn read_all(pipe: Option<impl Read + Send + 'static>) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut out = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut out);
        }
        out
    })
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

/// 有没跑完的运行或 job、或者等着登录、找不到 gh 时隔多久重读，别的隔多久重读。
const POLL_RUNNING: Duration = Duration::from_secs(10);
const POLL_IDLE: Duration = Duration::from_secs(60);

/// 每秒问一次：上次读到 `data`、在 `fetched` 读完的一份数据到 `now` 该不该重读。没读过的马上读；等登录
/// 或找不到 gh 的按快的间隔读，在终端里登好、装好或 shell 报上 PATH 后页上自己恢复。差不到一秒算到了，免得每次都晚一秒。
///
/// 读到的 job 都跑完了也照常按慢的间隔重读：等部署审批的、按上一个 job 的输出生成的 matrix job 会后来
/// 才出现，那次尝试不见得结束了。
// shortcut: 早就结束的尝试展开着时也每分钟白读一次，要省时按父运行的状态判断尝试结束了再停。
pub(super) fn due(data: Option<&Result<Data, String>>, fetched: Option<Instant>, now: Instant) -> bool {
    let every = match data {
        Some(Ok(data)) if data.running() => POLL_RUNNING,
        Some(Err(err)) if err == LOGIN || err == MISSING => POLL_RUNNING,
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

/// 本地工作流文件 `path` 的内容，最多读 `MAX_WORKFLOW_BYTES`。不是普通文件或读不了时为空：仓库是别人的，
/// 指向 `/dev/zero` 的符号链接、管道都能让这里读不完或卡住。
fn read_workflow(path: &Path) -> Option<String> {
    // 打开时不跟随符号链接、不等管道的写端，再看打开的是不是普通文件，中间换掉也骗不过。
    let file = fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_WORKFLOW_BYTES).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// 工作流文件里有没有 `workflow_dispatch` 触发，有才能手动触发。
pub(super) fn dispatchable(source: &str) -> bool {
    // shortcut: 不解析 YAML，按字面找；注释里写了它也算，要按 on: 精确判断时再引 YAML 解析。
    source.contains("workflow_dispatch")
}

/// 一个 step 在网页上的位置：job 页加上 step 的编号。
pub(super) fn step_url(job: &Job, step: &Step) -> String {
    format!("{}#step:{}:1", job.url, step.number)
}

/// 打进 shell 的参数：只含常见字符时原样，否则用单引号括起来；开头是 `=` 或 `%` 时也括起来，zsh 会把
/// `=foo` 展开成命令的路径。
fn shell_quote(text: &str) -> String {
    let plain = |c: char| c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+' | '@' | '%' | ':' | ',' | '=');
    if !text.is_empty() && !text.starts_with(['=', '%']) && text.chars().all(plain) {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
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
                "steps":[{"name":"Set up job","number":1,"status":"completed","conclusion":"success"},
                         {"name":"Post checkout","number":4,"status":"completed","conclusion":"success"}]}"#,
        )
        .unwrap();
        assert_eq!(job.steps[0].state(), State::Success);
        assert_eq!(step_url(&job, &job.steps[0]), "https://x/job/9#step:1:1");
        // 编号不连续时按编号，不按第几个。
        assert_eq!(step_url(&job, &job.steps[1]), "https://x/job/9#step:4:1");
    }

    #[test]
    fn polls_running_and_waiting_for_login_fast_and_the_rest_slowly() {
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
        // 等登录和找不到 gh 的十秒一次，别的错一分钟一次。
        assert!(due(Some(&Err(LOGIN.to_owned())), Some(t0), at(9)));
        assert!(due(Some(&Err(MISSING.to_owned())), Some(t0), at(9)));
        assert!(!due(Some(&Err("HTTP 404".to_owned())), Some(t0), at(9)));
        // 在跑的 job 十秒一次；都跑完了也一分钟一次，后来才出现的 job 还读得到。
        let jobs = |status: &str| {
            let job = format!(r#"{{"databaseId":9,"name":"c","status":"{status}","conclusion":"","url":"u"}}"#);
            Ok(Data::Jobs(vec![serde_json::from_str(&job).unwrap()]))
        };
        assert!(due(Some(&jobs("in_progress")), Some(t0), at(9)));
        assert!(!due(Some(&jobs("completed")), Some(t0), at(30)));
        assert!(due(Some(&jobs("completed")), Some(t0), at(59)));
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
    fn a_stuck_command_is_killed_at_the_timeout() {
        let started = Instant::now();
        let err = output_within(Command::new("/bin/sh").args(["-c", "sleep 30"]), Duration::from_millis(200));
        assert_eq!(err.unwrap_err(), "gh did not finish in 0 s");
        assert!(started.elapsed() < Duration::from_secs(10));
        let out = output_within(Command::new("/bin/sh").args(["-c", "echo hi; echo no >&2"]), TIMEOUT).unwrap();
        assert!(out.status.success());
        assert_eq!((&out.stdout[..], &out.stderr[..]), (&b"hi\n"[..], &b"no\n"[..]));
        assert_eq!(output_within(&mut Command::new("/nonexistent/gh"), TIMEOUT).unwrap_err(), MISSING);
    }

    #[test]
    fn finds_the_dispatch_trigger() {
        assert!(dispatchable("on:\n  workflow_dispatch:\n  push:\n"));
        assert!(!dispatchable("on: push\n"));
    }

    /// 只读普通文件：符号链接（哪怕指向 `/dev/zero`）、管道都当本地没有。
    #[test]
    fn reads_only_regular_workflow_files() {
        let dir = std::env::temp_dir().join(format!("runode-gh-workflow-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("ci.yml"), "on: workflow_dispatch\n").unwrap();
        std::os::unix::fs::symlink("/dev/zero", dir.join("zero.yml")).unwrap();
        std::os::unix::fs::symlink("ci.yml", dir.join("link.yml")).unwrap();
        let fifo = std::ffi::CString::new(dir.join("fifo.yml").into_os_string().into_encoded_bytes()).unwrap();
        // SAFETY: `fifo` 是以 NUL 结尾的路径，调用期间活着。
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert_eq!(read_workflow(&dir.join("ci.yml")).as_deref(), Some("on: workflow_dispatch\n"));
        for name in ["zero.yml", "link.yml", "fifo.yml", "missing.yml"] {
            assert_eq!(read_workflow(&dir.join(name)), None, "{name}");
        }
        fs::remove_dir_all(&dir).ok();
    }
}
