//! 起 agent 时挑模型和强度：各家的命令怎么带上它们（`Pick::command`，能从命令认回来，`Pick::parse`），
//! 以及怎么向 agent 本身问它的模型清单和每个模型能用的强度（`fetch`）。清单不写死：各家出了新模型、
//! 账号能用的模型不同，问到的才对。
//!
//! 命令是在 shell 的提示符上打的，提示词由调用方接在末尾。

use std::{
    ffi::OsString,
    io::{BufRead as _, BufReader, ErrorKind, Read as _, Write as _},
    os::unix::process::CommandExt as _,
    path::Path,
    process::{Command, Stdio},
    sync::mpsc::{self, RecvTimeoutError},
    time::Duration,
};

use serde_json::Value;

/// 问模型清单最多等这么久。
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// 能挑模型和强度的一家 agent。
pub struct Launcher {
    pub label: &'static str,
    /// 命令开头：程序名，有的还带子命令（`opencode2 mini`）。
    pub command: &'static str,
    /// 模型参数在模型名前面的部分，连着前面的空格；没法挑模型的为空。
    pub model_arg: Option<&'static str>,
    /// 强度参数在强度前面的部分，同样连着前面的空格；接在模型名后面的（`'#'high`）没有空格。
    pub effort_arg: Option<&'static str>,
    /// 强度接在模型名后面，不挑模型就挑不了强度。
    pub effort_needs_model: bool,
    /// 不问权限、一路跑完的参数，连着前面的空格；没有、或者本来就不问的为空。见 `with_options`。
    pub auto_arg: Option<&'static str>,
    /// 以自定义 agent 的身份跑的参数，在名字前面，连着前面的空格；没有的为空。
    pub subagent_arg: Option<&'static str>,
    /// 提示词前面的参数（`-i`、`--prompt`），连着前面的空格；提示词直接跟在后面的为空。
    prompt_arg: &'static str,
    /// 接着当前目录里最近一次对话的参数，插在 `command` 后面，连着前面的空格；没有的为空，见 `resume_command`。
    resume_arg: Option<&'static str>,
    /// 开始时指定对话号、续接时按号接回那场对话的两个参数，各连着前后的空格；不能指定对话号的为空，只能用
    /// `resume_arg` 接目录里最近一次的，那可能是用户在同一目录里另开的对话。见 `start_command`。
    session_args: Option<(&'static str, &'static str)>,
    probe: Probe,
}

/// 怎么问模型清单：起 agent 时的参数、写进标准输入的请求（为空时标准输入接空设备）和怎么认回话。
struct Probe {
    args: &'static [&'static str],
    request: &'static str,
    answer: Answer,
}

enum Answer {
    /// 一行行认，见 `Reply`。
    Lines(fn(&str) -> Reply),
    /// 程序自己退出以后，读整段标准输出。
    Output(fn(&str) -> Option<Catalog>),
}

/// 读到一行以后怎么办。
enum Reply {
    /// 不是回话，接着读。
    Wait,
    /// 再写这些进标准输入，接着读：有的协议要等上一句的回话才能说下一句。
    Send(&'static str),
    Done(Catalog),
}

/// 按常用的先后排；第一个和 `DEFAULT_AGENT` 一样，没挑过时用它。同一家不兼容的几个版本用同一个 `label`、挨着排，
/// 新的在前，菜单里只列第一个，找不到它的命令时换下一个（`same_kind`）。
pub const LAUNCHERS: &[Launcher] = &[
    Launcher {
        label: "Claude",
        command: "claude",
        model_arg: Some(" --model "),
        effort_arg: Some(" --effort "),
        effort_needs_model: false,
        auto_arg: Some(" --permission-mode bypassPermissions"),
        subagent_arg: Some(" --agent "),
        prompt_arg: "",
        resume_arg: Some(" --continue"),
        session_args: Some((" --session-id ", " --resume ")),
        // SDK 用的控制协议：初始化的回话里有账号能用的模型和每个模型的强度，不调模型。不读设置文件，免得跑
        // 用户的 hook；`--bare` 也不跑 hook，但它不读登录状态，问到的模型和账号的对不上。
        probe: Probe {
            args: &[
                "--setting-sources",
                "",
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--no-session-persistence",
            ],
            request: "{\"type\":\"control_request\",\"request_id\":\"runode\",\"request\":{\"subtype\":\"initialize\"}}\n",
            answer: Answer::Lines(claude_catalog),
        },
    },
    Launcher {
        label: "Codex",
        command: "codex",
        model_arg: Some(" -m "),
        effort_arg: Some(" -c model_reasoning_effort="),
        effort_needs_model: false,
        // 只免确认（`--full-auto`）还在沙箱里，连不上网，`gh` 推不了分支。
        auto_arg: Some(" --dangerously-bypass-approvals-and-sandbox"),
        subagent_arg: None,
        prompt_arg: "",
        resume_arg: Some(" resume --last"),
        session_args: None,
        // app-server 的 JSON-RPC，不用登录也答；标准输入关了它就不回话直接退出。
        probe: Probe {
            args: &["app-server"],
            request: concat!(
                "{\"id\":0,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"runode\",\"version\":\"0\"}}}\n",
                "{\"method\":\"initialized\"}\n",
                "{\"id\":1,\"method\":\"model/list\",\"params\":{}}\n",
            ),
            answer: Answer::Lines(codex_catalog),
        },
    },
    Launcher {
        label: "Pi",
        command: "pi",
        model_arg: Some(" --model "),
        effort_arg: Some(" --thinking "),
        effort_needs_model: false,
        auto_arg: None,
        subagent_arg: None,
        prompt_arg: "",
        resume_arg: Some(" --continue"),
        session_args: None,
        probe: Probe {
            args: &["--mode", "rpc", "--no-session"],
            request: "{\"id\":\"runode\",\"type\":\"get_available_models\"}\n",
            answer: Answer::Lines(pi_catalog),
        },
    },
    Launcher {
        label: "OMP",
        command: "omp",
        model_arg: Some(" --model "),
        effort_arg: Some(" --thinking "),
        effort_needs_model: false,
        auto_arg: None,
        subagent_arg: None,
        prompt_arg: "",
        resume_arg: Some(" --continue"),
        session_args: None,
        // 只列配了密钥的服务商；它的 RPC 也能问，但一个密钥都没有时起不来。
        probe: Probe { args: &["models", "--json"], request: "", answer: Answer::Output(omp_catalog) },
    },
    Launcher {
        // OpenCode V2（`@opencode/cli`）：命令行和 1.x 不兼容，装上以后 `opencode` 也成了 V2，另外还有 `opencode2`。
        // 有 `opencode2` 就按 V2 写，没有时调用方退到下面同名的 1.x 那一项（`same_kind`）。全屏界面不认 `-m`，
        // 小界面（`mini`）认，强度写成 `服务商/模型#强度`。
        label: "OpenCode",
        command: "opencode2 mini",
        model_arg: Some(" -m "),
        // `#` 加引号：zsh 开了 extendedglob 时不加引号的 `#` 是通配符，找不到文件就报错。
        effort_arg: Some("'#'"),
        effort_needs_model: true,
        auto_arg: None,
        subagent_arg: None,
        prompt_arg: " --prompt",
        resume_arg: None,
        session_args: None,
        // 不带 `--standalone` 时会拉起后台服务、问完了也留着。
        probe: Probe {
            args: &["api", "--standalone", "GET", "/api/model"],
            request: "",
            answer: Answer::Lines(opencode2_catalog),
        },
    },
    Launcher {
        // OpenCode 1.x（`opencode-ai`）。
        label: "OpenCode",
        command: "opencode",
        model_arg: Some(" -m "),
        // 全屏界面没有强度参数，只有 `run` 有。
        effort_arg: None,
        effort_needs_model: false,
        auto_arg: None,
        subagent_arg: None,
        // 直接跟在后面的是项目目录。
        prompt_arg: " --prompt",
        resume_arg: Some(" --continue"),
        session_args: None,
        probe: Probe { args: &["models"], request: "", answer: Answer::Output(opencode_catalog) },
    },
    Launcher {
        label: "GitHub Copilot",
        command: "copilot",
        model_arg: Some(" --model "),
        effort_arg: Some(" --reasoning-effort "),
        effort_needs_model: false,
        auto_arg: Some(" --allow-all-tools --allow-all-paths"),
        subagent_arg: Some(" --agent "),
        prompt_arg: " -i",
        resume_arg: Some(" --continue"),
        session_args: None,
        // 账号能用的模型要登录才问得到（`--headless --stdio` 的 `models.list`），这里读配置说明里列的那些。
        probe: Probe { args: &["help", "config"], request: "", answer: Answer::Output(copilot_catalog) },
    },
    Launcher {
        label: "Cursor",
        command: "cursor-agent",
        // 强度写在模型名里（`…-thinking-high`），没有单独的参数。
        model_arg: Some(" --model "),
        effort_arg: None,
        effort_needs_model: false,
        auto_arg: Some(" --force"),
        subagent_arg: None,
        prompt_arg: "",
        resume_arg: Some(" resume"),
        session_args: None,
        // 要登录才列得出来。
        probe: Probe { args: &["models"], request: "", answer: Answer::Output(cursor_catalog) },
    },
    Launcher {
        label: "Antigravity",
        command: "agy",
        model_arg: Some(" --model "),
        effort_arg: Some(" --effort "),
        effort_needs_model: false,
        auto_arg: None,
        subagent_arg: None,
        // 提示词直接跟在后面会报多余的参数。
        prompt_arg: " -i",
        resume_arg: None,
        session_args: None,
        // `--output-format` 要写在 `models` 前面。要登录（或者配了 API 密钥）才列得出来。
        probe: Probe { args: &["--output-format", "json", "models"], request: "", answer: Answer::Lines(agy_catalog) },
    },
    Launcher {
        label: "Muse",
        command: "muse",
        model_arg: Some(" --model "),
        effort_arg: Some(" --reasoning-effort "),
        effort_needs_model: false,
        auto_arg: None,
        subagent_arg: None,
        prompt_arg: "",
        resume_arg: None,
        session_args: None,
        // `serve` 的 JSON-RPC：初始化答了才能说 `initialized`，再问清单。没登录时清单是空的；0.2.x 没有 `serve`。
        probe: Probe {
            args: &["serve"],
            request: "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"runode\",\"version\":\"0\"}}}\n",
            answer: Answer::Lines(muse_catalog),
        },
    },
];

/// 第 `agent` 个之后、和它是同一家（`label` 一样）的下一个：找不到第 `agent` 个的命令时换它试。
pub fn same_kind(agent: usize) -> Option<usize> {
    let label = LAUNCHERS.get(agent)?.label;
    Some(agent + 1).filter(|next| LAUNCHERS.get(*next).is_some_and(|launcher| launcher.label == label))
}

/// 第 `agent` 个是不是它那一家里排第一的，菜单里只列这些。
pub fn listed(agent: usize) -> bool {
    agent == 0 || LAUNCHERS.get(agent - 1).is_none_or(|before| before.label != LAUNCHERS[agent].label)
}

/// 一个模型：写进命令的名字、菜单里显示的名字、能用的强度。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Model {
    pub id: String,
    pub label: String,
    pub efforts: Vec<String>,
}

/// 向 agent 问到的模型清单。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Catalog {
    pub models: Vec<Model>,
    /// 模型选默认时能用的强度。
    pub default_efforts: Vec<String>,
}

impl Catalog {
    /// 模型 `model`（为空是默认）能用的强度；清单里没有这个模型时按默认的。
    pub fn efforts(&self, model: Option<&str>) -> &[String] {
        model
            .and_then(|model| self.models.iter().find(|known| known.id == model))
            .map_or(&self.default_efforts, |model| &model.efforts)
    }

    /// 菜单里显示的模型名：清单里没有时就是 `id`。
    pub fn label<'a>(&'a self, id: &'a str) -> &'a str {
        self.models.iter().find(|model| model.id == id).map_or(id, |model| &model.label)
    }
}

/// 为什么没问到模型清单。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unanswered {
    /// 找不到这个命令：没装，或者不在 PATH 里。
    Missing,
    /// 跑起来了，没答上来（出错退出、到了时限、回话认不出）。
    NoAnswer,
}

/// 在 `dir` 里问 `launcher` 的模型清单，最多等 `PROBE_TIMEOUT`，会卡住，放到后台跑；`path` 是找 agent 用的 PATH，
/// 为空时用本进程的。`dir` 不在了（任务的目录被删了）时在 `/` 里问：进不去目录也报 NotFound，会被当成没装。
pub fn fetch(launcher: &Launcher, dir: &Path, path: Option<OsString>) -> Result<Catalog, Unanswered> {
    // 问清单时不带命令里的子命令（`opencode2 mini` 的 `mini`）。
    let binary = launcher.command.split(' ').next().unwrap_or_default();
    let dir = if dir.is_dir() { dir } else { Path::new("/") };
    let mut catalog = ask(binary, &launcher.probe, dir, path)?;
    // agent 答的模型名和强度都要拼进 shell 命令，放不进 `quote` 的单引号里的不要。
    catalog.models.retain(|model| usable(&model.id));
    for model in &mut catalog.models {
        model.efforts.retain(|effort| usable(effort));
    }
    catalog.default_efforts.retain(|effort| usable(effort));
    Ok(catalog)
}

/// 在 `dir` 里跑 `binary` 照 `probe` 问模型清单：把请求写进标准输入，再照 `Answer` 读标准输出。过了
/// `PROBE_TIMEOUT`、或者程序退了还没认出来时是 `NoAnswer`。完了杀掉整组。有请求时标准输入一直开着，关了的话
/// 有的 agent（codex）不回话就退出。
fn ask(binary: &str, probe: &Probe, dir: &Path, path: Option<OsString>) -> Result<Catalog, Unanswered> {
    let mut command = Command::new(binary);
    command
        .args(probe.args)
        .current_dir(dir)
        .stdin(if probe.request.is_empty() { Stdio::null() } else { Stdio::piped() })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // 自成一个进程组：包装脚本（npx、node）拉起的真 agent 也在组里，完了一起杀掉。
        .process_group(0);
    if let Some(path) = path {
        command.env("PATH", path);
    }
    let mut child = command.spawn().map_err(|err| match err.kind() {
        ErrorKind::NotFound => Unanswered::Missing,
        _ => Unanswered::NoAnswer,
    })?;
    let group = child.id() as libc::pid_t;
    // SAFETY: 只是给自己拉起的那个进程组发信号；组长还没收尸，组号不会被别的进程占去。
    let kill_group = move || unsafe { libc::killpg(group, libc::SIGKILL) };
    let (done, timer) = mpsc::channel::<()>();
    // 到时限杀掉整组，读着的标准输出跟着到头。
    let watchdog = std::thread::spawn(move || {
        if timer.recv_timeout(PROBE_TIMEOUT) == Err(RecvTimeoutError::Timeout) {
            kill_group();
        }
    });
    let mut stdin = child.stdin.take();
    let mut send = |text: &str| match &mut stdin {
        Some(stdin) => stdin.write_all(text.as_bytes()).is_ok(),
        None => text.is_empty(),
    };
    let found = (|| {
        if !send(probe.request) {
            return None;
        }
        let mut stdout = BufReader::new(child.stdout.take()?);
        match probe.answer {
            Answer::Lines(answer) => {
                for line in stdout.lines() {
                    match answer(&line.ok()?) {
                        Reply::Wait => {}
                        Reply::Send(text) => {
                            if !send(text) {
                                return None;
                            }
                        }
                        Reply::Done(catalog) => return Some(catalog),
                    }
                }
                None
            }
            Answer::Output(answer) => {
                let mut output = String::new();
                stdout.read_to_string(&mut output).ok()?;
                // 等它自己退出再杀组：先杀的话还没退的会算成被杀，退出码就看不出它是不是出错了。只等不收尸
                // （`WNOWAIT`），组号在下面杀组时还是它的。
                // SAFETY: `info` 是纯数据，全零合法；`waitid` 只写它。
                let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
                // SAFETY: 等的是自己拉起的子进程，`info` 指向局部变量。
                unsafe { libc::waitid(libc::P_PID, child.id(), &raw mut info, libc::WEXITED | libc::WNOWAIT) };
                answer(&output)
            }
        }
    })();
    drop(done);
    watchdog.join().ok();
    kill_group();
    drop(send);
    let status = child.wait().ok();
    // 读整段输出的要它正常退出才算：出错退出时标准输出空着，看起来像一份空清单。
    let failed = matches!(probe.answer, Answer::Output(_)) && !status.is_some_and(|status| status.success());
    found.filter(|_| !failed).ok_or(Unanswered::NoAnswer)
}

/// 字符串数组里的字符串。
fn strings(value: &Value) -> Vec<String> {
    value.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect()
}

/// 能放进命令的模型名、强度：单引号放不进 `quote` 加的单引号里，反斜杠在 fish 的单引号里还是转义符，控制字符
/// 宿主不收。
fn usable(word: &str) -> bool {
    !word.is_empty() && !word.contains(|ch: char| ch == '\'' || ch == '\\' || ch.is_control())
}

/// claude 初始化的回话：`models` 里每项是 `value`（写进 `--model`）、`displayName`、`supportedEffortLevels`；
/// `value` 是 `default` 的那项就是默认。
fn claude_catalog(line: &str) -> Reply {
    claude_catalog_of(line).map_or(Reply::Wait, Reply::Done)
}

fn claude_catalog_of(line: &str) -> Option<Catalog> {
    let message: Value = serde_json::from_str(line).ok()?;
    if message["type"] != "control_response" || message["response"]["request_id"] != "runode" {
        return None;
    }
    let mut catalog = Catalog::default();
    // 出错的回话没有 `models`，当成空清单答完，不等到时限。
    for model in message["response"]["response"]["models"].as_array().into_iter().flatten() {
        let (Some(id), efforts) = (model["value"].as_str(), strings(&model["supportedEffortLevels"])) else {
            continue;
        };
        if id == "default" {
            catalog.default_efforts = efforts;
        } else {
            let label = model["displayName"].as_str().unwrap_or(id).to_owned();
            catalog.models.push(Model { id: id.to_owned(), label, efforts });
        }
    }
    Some(catalog)
}

/// codex `model/list` 的回话：`data` 里每项是 `model`、`displayName`、`supportedReasoningEfforts`，
/// `isDefault` 的那项是默认，`hidden` 的不列。
fn codex_catalog(line: &str) -> Reply {
    codex_catalog_of(line).map_or(Reply::Wait, Reply::Done)
}

fn codex_catalog_of(line: &str) -> Option<Catalog> {
    let message: Value = serde_json::from_str(line).ok()?;
    if message["id"] != 1 {
        return None;
    }
    let mut catalog = Catalog::default();
    // shortcut: 只读第一页（`nextCursor` 不跟），模型多到要翻页时再跟。
    for model in message["result"]["data"].as_array().into_iter().flatten() {
        let Some(id) = model["model"].as_str() else { continue };
        let efforts: Vec<String> = model["supportedReasoningEfforts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|effort| effort["reasoningEffort"].as_str().map(str::to_owned))
            .collect();
        if model["isDefault"] == true {
            catalog.default_efforts.clone_from(&efforts);
        }
        if model["hidden"] != true {
            let label = model["displayName"].as_str().unwrap_or(id).to_owned();
            catalog.models.push(Model { id: id.to_owned(), label, efforts });
        }
    }
    Some(catalog)
}

/// pi 能设的思考强度，从低到高。
const PI_THINKING: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// pi `get_available_models` 的回话：每项是 `provider`、`id`、`name`，`--model` 写 `provider/id`。pi 不直接给
/// 每个模型能用的强度，照它自己的 `getSupportedThinkingLevels` 推：不推理的只有 `off`；`thinkingLevelMap`
/// 里写成 null 的不能用，`xhigh`、`max` 要写了才能用。默认模型由 pi 的设置定，这边不知道是哪个，就列全，
/// 用不了的 pi 自己往下挪。
fn pi_catalog(line: &str) -> Reply {
    pi_catalog_of(line).map_or(Reply::Wait, Reply::Done)
}

fn pi_catalog_of(line: &str) -> Option<Catalog> {
    let message: Value = serde_json::from_str(line).ok()?;
    if message["id"] != "runode" || message["type"] != "response" {
        return None;
    }
    let mut catalog =
        Catalog { default_efforts: PI_THINKING.iter().map(|&level| level.to_owned()).collect(), ..Catalog::default() };
    // 没成功（`success` 不是 true）时没有 `models`，当成空清单答完。
    for model in message["data"]["models"].as_array().into_iter().flatten() {
        let (Some(provider), Some(id)) = (model["provider"].as_str(), model["id"].as_str()) else { continue };
        let id = format!("{provider}/{id}");
        let efforts = if model["reasoning"] == true {
            let map = &model["thinkingLevelMap"];
            PI_THINKING
                .iter()
                .filter(|&&level| match map.get(level) {
                    Some(Value::Null) => false,
                    mapped => mapped.is_some() || !matches!(level, "xhigh" | "max"),
                })
                .map(|&level| level.to_owned())
                .collect()
        } else {
            vec!["off".to_owned()]
        };
        let label = format!("{provider} · {}", model["name"].as_str().unwrap_or(&id));
        catalog.models.push(Model { id, label, efforts });
    }
    Some(catalog)
}

/// omp 能设的思考强度：pi 的那些，再加 `auto`。
const OMP_THINKING: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max", "auto"];

/// `omp models --json`：`models` 里每项是 `selector`（写进 `--model`，`服务商/模型`）、`provider`、`name`、
/// `thinking`（能用的强度，不推理的为 null）。默认模型由 omp 的设置定，强度列全。
fn omp_catalog(output: &str) -> Option<Catalog> {
    let message: Value = serde_json::from_str(output).ok()?;
    let mut catalog =
        Catalog { default_efforts: OMP_THINKING.iter().map(|&level| level.to_owned()).collect(), ..Catalog::default() };
    for model in message["models"].as_array()? {
        let Some(id) = model["selector"].as_str() else { continue };
        let provider = model["provider"].as_str().unwrap_or_default();
        let label = format!("{provider} · {}", model["name"].as_str().unwrap_or(id));
        catalog.models.push(Model { id: id.to_owned(), label, efforts: strings(&model["thinking"]) });
    }
    Some(catalog)
}

/// `opencode models`：一行一个 `服务商/模型`。只列配好的服务商，没登录时只有免费的 `opencode/*`。
fn opencode_catalog(output: &str) -> Option<Catalog> {
    let models = output
        .lines()
        .map(str::trim)
        .filter(|line| line.contains('/') && !line.contains(char::is_whitespace))
        .map(|id| Model { id: id.to_owned(), label: id.to_owned(), efforts: Vec::new() })
        .collect();
    Some(Catalog { models, default_efforts: Vec::new() })
}

/// `opencode2 api GET /api/model`：一行 JSON，`data` 里每项是 `providerID`、`id`、`name`、`variants`（能用的强度，
/// 每项的 `id`）。强度要跟着模型写，默认模型没有可挑的强度。
fn opencode2_catalog(line: &str) -> Reply {
    let Ok(message) = serde_json::from_str::<Value>(line) else { return Reply::Wait };
    let Some(data) = message["data"].as_array() else { return Reply::Wait };
    let mut catalog = Catalog::default();
    for model in data {
        let (Some(provider), Some(id)) = (model["providerID"].as_str(), model["id"].as_str()) else { continue };
        let id = format!("{provider}/{id}");
        // 名字里有 `#` 时分不清哪段是强度。
        if id.contains('#') {
            continue;
        }
        let efforts = model["variants"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|variant| variant["id"].as_str().map(str::to_owned))
            .collect();
        let label = format!("{provider} · {}", model["name"].as_str().unwrap_or(&id));
        catalog.models.push(Model { id, label, efforts });
    }
    Reply::Done(catalog)
}

/// copilot `--reasoning-effort` 认的值。
const COPILOT_EFFORTS: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh", "max"];

/// `copilot help config` 里 `` `model` `` 那一项下面列的模型，一行一个 `- "名字"`。不分模型给强度，都按
/// `COPILOT_EFFORTS`。
fn copilot_catalog(output: &str) -> Option<Catalog> {
    let mut lines = output.lines().skip_while(|line| !line.trim_start().starts_with("`model`")).skip(1);
    let efforts: Vec<String> = COPILOT_EFFORTS.iter().map(|&effort| effort.to_owned()).collect();
    let mut models = Vec::new();
    for line in lines.by_ref() {
        let line = line.trim();
        if line.is_empty() && models.is_empty() {
            continue;
        }
        let Some(id) = line.strip_prefix("- \"").and_then(|rest| rest.strip_suffix('"')) else { break };
        models.push(Model { id: id.to_owned(), label: id.to_owned(), efforts: efforts.clone() });
    }
    (!models.is_empty()).then_some(Catalog { models, default_efforts: efforts })
}

/// `cursor-agent models`：`Available models` 下面一行一个 `<名字> - <显示的名字>`，后面可能跟着
/// ` (current, default)`。
fn cursor_catalog(output: &str) -> Option<Catalog> {
    let models: Vec<Model> = output
        .lines()
        .filter_map(|line| {
            let (id, label) = line.trim().split_once(" - ")?;
            let label = label.split(" (current").next().unwrap_or(label).trim();
            (!id.contains(char::is_whitespace)).then(|| Model {
                id: id.to_owned(),
                label: label.to_owned(),
                efforts: Vec::new(),
            })
        })
        .collect();
    (!models.is_empty()).then_some(Catalog { models, default_efforts: Vec::new() })
}

/// agy `--effort` 认的值。
const AGY_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// `agy --output-format json models`：一行 JSON，`command.data.models` 里每项是 `id`、`label`。强度写在 `id`
/// 末尾（`gemini-3.8-flash-high`，显示成 `Gemini 3.8 Flash (High)`），同一个模型的几项并成一个，强度挑末尾的那些；
/// `--model` 写去掉强度的名字，agy 认。
fn agy_catalog(line: &str) -> Reply {
    let Ok(message) = serde_json::from_str::<Value>(line) else { return Reply::Wait };
    let Some(rows) = message["command"]["data"]["models"].as_array() else { return Reply::Wait };
    let mut catalog = Catalog {
        default_efforts: AGY_EFFORTS.iter().map(|&effort| effort.to_owned()).collect(),
        ..Catalog::default()
    };
    for row in rows {
        let Some(id) = row["id"].as_str() else { continue };
        let label = row["label"].as_str().unwrap_or(id);
        let (base, effort) = AGY_EFFORTS
            .iter()
            .find_map(|&effort| Some((id.strip_suffix(effort)?.strip_suffix('-')?, Some(effort))))
            .unwrap_or((id, None));
        let index = match catalog.models.iter().position(|model| model.id == base) {
            Some(index) => index,
            None => {
                // `Gemini 3.1 Pro (Low)` 去掉括号里的强度。
                let label = label.rsplit_once(" (").filter(|_| effort.is_some()).map_or(label, |(name, _)| name);
                catalog.models.push(Model { id: base.to_owned(), label: label.to_owned(), efforts: Vec::new() });
                catalog.models.len() - 1
            }
        };
        if let Some(effort) = effort {
            catalog.models[index].efforts.push(effort.to_owned());
        }
    }
    Reply::Done(catalog)
}

/// muse 初始化答了以后要说的话：`initialized`，再问清单。
const MUSE_LIST: &str = concat!(
    "{\"jsonrpc\":\"2.0\",\"method\":\"initialized\"}\n",
    "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"model/list\",\"params\":{}}\n",
);

/// muse `serve` 的回话：初始化（`id` 1）答了就问清单；清单（`id` 2）的 `models` 里每项是 `modelId`、
/// `displayLabel`、`isDefault`，能用的强度在 `reasoningEffortVariants`（每项的 `tier`）或 `variants` 里。
fn muse_catalog(line: &str) -> Reply {
    let Ok(message) = serde_json::from_str::<Value>(line) else { return Reply::Wait };
    if message["id"] == 1 {
        // 初始化出错（没有 `result`）时当成空清单答完，不等到时限。
        return if message["result"].is_null() { Reply::Done(Catalog::default()) } else { Reply::Send(MUSE_LIST) };
    }
    if message["id"] != 2 {
        return Reply::Wait;
    }
    let mut catalog = Catalog::default();
    for model in message["result"]["models"].as_array().into_iter().flatten() {
        let Some(id) = model["modelId"].as_str() else { continue };
        let tiers: Vec<String> = model["reasoningEffortVariants"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|variant| variant["tier"].as_str().map(str::to_owned))
            .collect();
        let efforts = if tiers.is_empty() { strings(&model["variants"]) } else { tiers };
        if model["isDefault"] == true {
            catalog.default_efforts.clone_from(&efforts);
        }
        let label = model["displayLabel"].as_str().unwrap_or(id).to_owned();
        catalog.models.push(Model { id: id.to_owned(), label, efforts });
    }
    Reply::Done(catalog)
}

/// 开始任务时跑的命令（提示词跟在后面）：那家能指定对话号时把 `session` 插在程序名后面，之后 `resume_command`
/// 凭同一个号接回这场对话。Claude 要求对话号是 UUID。认不出、不能指定的原样。
pub fn start_command(command: &str, session: &str) -> String {
    match launcher_of(command) {
        Some((spec, rest)) if let Some((arg, _)) = spec.session_args => format!("{}{arg}{session}{rest}", spec.command),
        _ => command.to_owned(),
    }
}

/// 加上免确认（`auto`）和以自定义 agent（`subagent`）的身份跑的参数，插在程序名后面，`start_command`、
/// `resume_command` 照样认得出是哪家、续接时也带着。那家没有这个参数、认不出是哪家（自己写的命令）时不加；
/// 名字不是 `subagent_name` 那样的也不加：它要原样拼进 shell 命令，`quote` 不转义单引号。
pub fn with_options(command: &str, auto: bool, subagent: Option<&str>) -> String {
    let Some((spec, rest)) = launcher_of(command) else { return command.to_owned() };
    let mut with = spec.command.to_owned();
    if let Some(arg) = spec.auto_arg.filter(|_| auto) {
        with += arg;
    }
    if let (Some(arg), Some(name)) = (spec.subagent_arg, subagent.filter(|name| subagent_name(name))) {
        with += &format!("{arg}{name}");
    }
    with + rest
}

/// 自定义 agent 的名字能不能用：只有字母、数字和 `-_.:`，拼进 shell 命令不用加引号。
pub fn subagent_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|ch| ch.is_ascii_alphanumeric() || "-_.:".contains(ch))
}

/// 在原来的目录里接着 agent 上次对话的命令：`command` 是开始任务时跑的那条（不带提示词），按开头的程序名认出
/// 是哪家，把那家的续接参数插在程序名后面，别的参数（模型、权限这些）照留，末尾等着提示词的参数去掉。能指定
/// 对话号的按 `start_command` 用过的 `session` 接回，别家接目录里最近一次的。认不出、或者那家没有续接参数时为空。
pub fn resume_command(command: &str, session: &str) -> Option<String> {
    let (spec, rest) = launcher_of(command)?;
    let rest = rest.strip_suffix(spec.prompt_arg).unwrap_or(rest);
    let resume = match spec.session_args {
        Some((_, arg)) => format!("{arg}{session}"),
        None => spec.resume_arg?.to_owned(),
    };
    Some(format!("{}{resume}{rest}", spec.command))
}

/// `command` 开头的程序名是哪家，和后面的参数。
fn launcher_of(command: &str) -> Option<(&'static Launcher, &str)> {
    let command = command.trim();
    LAUNCHERS.iter().find_map(|spec| {
        let rest = command.strip_prefix(spec.command)?;
        (rest.is_empty() || rest.starts_with(char::is_whitespace)).then_some((spec, rest))
    })
}

/// 挑好的 agent（`LAUNCHERS` 里第几个）、模型和强度（空着是默认）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pick {
    pub agent: usize,
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl Pick {
    pub fn launcher(&self) -> &'static Launcher {
        &LAUNCHERS[self.agent]
    }

    /// 存进任务的命令。
    pub fn command(&self) -> String {
        let spec = self.launcher();
        let mut command = spec.command.to_owned();
        if let (Some(arg), Some(model)) = (spec.model_arg, &self.model) {
            command += &format!("{arg}{}", quote(model));
        }
        if let (Some(arg), Some(effort)) = (spec.effort_arg, &self.effort)
            && (self.model.is_some() || !spec.effort_needs_model)
        {
            command += &format!("{arg}{}", quote(effort));
        }
        command + spec.prompt_arg
    }

    /// 认出 `command` 是哪样挑出来的；不是 `command` 那样拼出来的为空。
    pub fn parse(command: &str) -> Option<Self> {
        let command = command.trim();
        LAUNCHERS.iter().enumerate().find_map(|(agent, spec)| {
            let mut rest = command.strip_prefix(spec.command)?.strip_suffix(spec.prompt_arg)?;
            let mut value = |arg: Option<&str>| {
                let (value, left) = unquote(rest.strip_prefix(arg?)?)?;
                rest = left;
                Some(value)
            };
            let model = value(spec.model_arg);
            let effort = value(spec.effort_arg);
            // 拼回去要和原来一字不差：手写的 `--model "opus"`、`$MODEL` 这类 shell 会再解释的，照原样当自己写的命令。
            Some(Self { agent, model, effort }).filter(|pick| rest.is_empty() && pick.command() == command)
        })
    }
}

/// 给 shell 看的一个词：只有安全的字符时原样，否则放进单引号（zsh 里 `[1m]` 这类会当成通配符，`=名字` 会展开成
/// 那个命令的路径）。
fn quote(word: &str) -> String {
    if !word.starts_with('=') && word.chars().all(|ch| ch.is_ascii_alphanumeric() || "._-/:@+=,".contains(ch)) {
        word.to_owned()
    } else {
        format!("'{word}'")
    }
}

/// `text` 开头 `quote` 写出的一个词，和剩下的部分。
fn unquote(text: &str) -> Option<(String, &str)> {
    if let Some(quoted) = text.strip_prefix('\'') {
        let (word, rest) = quoted.split_once('\'')?;
        return Some((word.to_owned(), rest));
    }
    // `quote` 不加引号的词里没有 `'`，接在模型名后面的强度（`服务商/模型'#'high`）从这里分开。
    let end = text.find(|ch: char| ch.is_whitespace() || ch == '\'').unwrap_or(text.len());
    (end > 0).then(|| (text[..end].to_owned(), &text[end..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn done(reply: Reply) -> Option<Catalog> {
        match reply {
            Reply::Done(catalog) => Some(catalog),
            Reply::Wait | Reply::Send(_) => None,
        }
    }

    fn ids(catalog: &Catalog) -> Vec<&str> {
        catalog.models.iter().map(|model| model.id.as_str()).collect()
    }

    #[test]
    fn reads_catalogs() {
        let claude = done(claude_catalog(
            r#"{"type":"control_response","response":{"request_id":"runode","response":{"models":[
                {"value":"default","supportedEffortLevels":["low","high"]},
                {"value":"opus","displayName":"Opus","supportedEffortLevels":["low","max"]},
                {"value":"old","displayName":"Old"}]}}}"#,
        ))
        .unwrap();
        assert_eq!(claude.default_efforts, ["low", "high"]);
        assert_eq!(claude.efforts(Some("opus")), ["low", "max"]);
        assert!(claude.efforts(Some("old")).is_empty());
        assert_eq!(claude.efforts(Some("gone")), ["low", "high"]);
        assert_eq!(claude.label("opus"), "Opus");
        assert!(done(claude_catalog(r#"{"type":"system"}"#)).is_none());
        // 出错的回话也算答完，不等到时限。
        let error = r#"{"type":"control_response","response":{"subtype":"error","request_id":"runode","error":"x"}}"#;
        assert_eq!(done(claude_catalog(error)), Some(Catalog::default()));

        let codex = done(codex_catalog(
            r#"{"id":1,"result":{"data":[
                {"model":"a","displayName":"A","isDefault":true,"supportedReasoningEfforts":[{"reasoningEffort":"low"}]},
                {"model":"b","hidden":true}]}}"#,
        ))
        .unwrap();
        assert_eq!(ids(&codex), ["a"]);
        assert_eq!(codex.default_efforts, ["low"]);
        assert!(done(codex_catalog(r#"{"id":0,"result":{}}"#)).is_none());

        let pi = done(pi_catalog(
            r#"{"id":"runode","type":"response","success":true,"data":{"models":[
                {"provider":"g","id":"flash","name":"Flash","reasoning":true,"thinkingLevelMap":{"off":null,"xhigh":null,"max":"max"}},
                {"provider":"o","id":"gpt-4","name":"GPT-4","reasoning":false}]}}"#,
        ))
        .unwrap();
        assert_eq!(pi.efforts(Some("g/flash")), ["minimal", "low", "medium", "high", "max"]);
        assert_eq!(pi.efforts(Some("o/gpt-4")), ["off"]);
        assert_eq!(pi.label("g/flash"), "g · Flash");

        let omp = omp_catalog(
            r#"{"models":[{"provider":"openai","selector":"openai/codex-mini","name":"Codex Mini","thinking":["low","high"]},
                {"provider":"openai","selector":"openai/gpt-4","name":"GPT-4","thinking":null}]}"#,
        )
        .unwrap();
        assert_eq!(omp.efforts(Some("openai/codex-mini")), ["low", "high"]);
        assert!(omp.efforts(Some("openai/gpt-4")).is_empty());

        let opencode =
            opencode_catalog("opencode/big-pickle\nopencode/space-bunny-free\n\x1b[32mModels cache refreshed\n")
                .unwrap();
        assert_eq!(ids(&opencode), ["opencode/big-pickle", "opencode/space-bunny-free"]);

        let opencode2 = done(opencode2_catalog(
            r#"{"location":{},"data":[{"id":"x","providerID":"opencode","name":"X","variants":[{"id":"low"},{"id":"high"}]}]}"#,
        ))
        .unwrap();
        assert_eq!(opencode2.efforts(Some("opencode/x")), ["low", "high"]);

        let copilot = copilot_catalog(
            "Settings:\n  `model`: the model\n\n    - \"claude-opus-5.5\"\n    - \"gpt-6.1-sol\"\n\n  `theme`: x\n",
        )
        .unwrap();
        assert_eq!(ids(&copilot), ["claude-opus-5.5", "gpt-6.1-sol"]);
        assert_eq!(copilot.efforts(Some("gpt-6.1-sol")), COPILOT_EFFORTS);

        let cursor = cursor_catalog(
            "Available models\n\nauto - Auto (current, default)\nclaude-opus-4-8-thinking-high - Claude Opus 4.8 Thinking High\n\nTip: use --model\n",
        )
        .unwrap();
        assert_eq!(ids(&cursor), ["auto", "claude-opus-4-8-thinking-high"]);
        assert_eq!(cursor.label("auto"), "Auto");

        let agy = done(agy_catalog(
            r#"{"status":"SUCCESS","command":{"name":"models","data":{"models":[
                {"id":"gemini-3.8-flash-high","label":"Gemini 3.8 Flash (High)"},
                {"id":"gemini-3.8-flash-medium","label":"Gemini 3.8 Flash (Medium)"},
                {"id":"plain","label":"Plain"}]}}}"#,
        ))
        .unwrap();
        assert_eq!(ids(&agy), ["gemini-3.8-flash", "plain"]);
        assert_eq!(agy.efforts(Some("gemini-3.8-flash")), ["high", "medium"]);
        assert_eq!(agy.label("gemini-3.8-flash"), "Gemini 3.8 Flash");

        // muse 先答初始化，再答清单。
        assert!(matches!(muse_catalog(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#), Reply::Send(MUSE_LIST)));
        let muse = done(muse_catalog(
            r#"{"jsonrpc":"2.0","id":2,"result":{"models":[{"modelId":"muse-spark-1.3","displayLabel":"Spark","isDefault":true,
                "reasoningEffortVariants":[{"tier":"low"},{"tier":"high"}]}]}}"#,
        ))
        .unwrap();
        assert_eq!(muse.default_efforts, ["low", "high"]);
    }

    #[test]
    fn keeps_unquotable_names_out() {
        assert!(usable("claude-fable-5-1[1m]"));
        assert!(!usable("a'b") && !usable("a\\") && !usable("") && !usable("high\u{1b}"));
        // zsh 把开头的 `=` 展开成命令的路径。
        assert_eq!(quote("=opus"), "'=opus'");
    }

    /// 一行一行认：读到 `want` 就答完，读到 `then` 就再写 `send`。
    fn lines_probe(args: &'static [&'static str], request: &'static str) -> Probe {
        fn answer(line: &str) -> Reply {
            match line {
                "first" => Reply::Send("second\n"),
                "second" => Reply::Done(Catalog::default()),
                _ => Reply::Wait,
            }
        }
        Probe { args, request, answer: Answer::Lines(answer) }
    }

    #[test]
    fn asks_and_stops_the_agent() {
        // 要等一句回话才说下一句、答完了还不退的程序：认出回话就杀掉，不等到时限。
        let started = std::time::Instant::now();
        let script = &["-c", "read a; echo noise; echo \"$a\"; read b; echo \"$b\"; sleep 30"];
        let answer = ask("/bin/sh", &lines_probe(script, "first\n"), Path::new("/"), None);
        assert_eq!(answer, Ok(Catalog::default()));
        assert!(started.elapsed() < Duration::from_secs(10));
        let quiet = ask("/bin/sh", &lines_probe(&["-c", "exit 0"], "first\n"), Path::new("/"), None);
        assert_eq!(quiet, Err(Unanswered::NoAnswer));
        let missing = ask("runode-no-such-agent", &lines_probe(&[], ""), Path::new("/"), None);
        assert_eq!(missing, Err(Unanswered::Missing));
        // 读整段输出的：标准输入接空设备，程序自己退出后再认；出错退出的不算，哪怕输出了点什么。
        let opencode =
            Probe { args: &["-c", "echo a/b; echo c/d"], request: "", answer: Answer::Output(opencode_catalog) };
        assert_eq!(ask("/bin/sh", &opencode, Path::new("/"), None).map(|catalog| catalog.models.len()), Ok(2));
        let broken = Probe { args: &["-c", "echo a/b; exit 1"], request: "", answer: Answer::Output(opencode_catalog) };
        assert_eq!(ask("/bin/sh", &broken, Path::new("/"), None), Err(Unanswered::NoAnswer));
    }
}
