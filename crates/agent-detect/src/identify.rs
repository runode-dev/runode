//! 按前台进程认出是哪个 agent。
//!
//! 只看进程名不够：很多 agent 是 node、bun 或 python 脚本，进程名是 `node`，要看参数里跑的
//! 是哪个脚本；有的用 `process.title` 改了 argv[0]；装在 npm 包里的要认包里入口文件的路径。
//! 一组前台进程里先看组长，组长认不出再从整组里挑最可信的一个。
//!
//! `AMBIGUOUS_NAMES` 里的名字容易和别的程序撞名（`kilo` 编辑器、自己编的 `./amp`、`node cn.js`），
//! 光名字对上不算，还要进程装在 agent 常见的安装位置，见 `installed`。

use runode_shared_types::agent::AgentKind;

/// 终端前台进程组里的一个进程。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ForegroundProcess {
    pub pid: u32,
    /// 内核记的进程名。
    pub name: String,
    /// 启动时执行的文件路径（execve 收到的那个，可能是符号链接），读不到时为 `None`。
    pub exe: Option<String>,
    /// argv[0] 的文件名，去掉了登录 shell 的「-」前缀；程序运行中改了它（比如 node 的
    /// `process.title`）时是改过的。读不到时为 `None`。
    pub argv0: Option<String>,
    /// 完整的参数，读不到时为 `None`。
    pub argv: Option<Vec<String>>,
}

/// 终端的前台进程组。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ForegroundJob {
    /// 进程组号，也就是组长的进程号。
    pub leader: u32,
    pub processes: Vec<ForegroundProcess>,
}

/// 认出前台进程组里跑的是哪个 agent，认不出时为 `None`。
pub fn identify_job(job: &ForegroundJob) -> Option<AgentKind> {
    let usable = |process: &ForegroundProcess, name: &str, kind: AgentKind| {
        (kind != AgentKind::Letta || letta_is_interactive(process))
            && (!AMBIGUOUS_NAMES.contains(&lookup_name(basename(name)).as_str()) || installed(process))
    };
    if let Some(leader) = job.processes.iter().find(|process| process.pid == job.leader) {
        let name = effective_name(leader);
        if let Some(kind) = agent_from_name(&name)
            && usable(leader, &name, kind)
        {
            return Some(kind);
        }
    }
    // 组长认不出时在整组里挑：从参数里认出来的最可信，其次是进程名本身就是 agent 的，最后是
    // 一个通用的运行时或 shell 恰好叫这个名字。一样可信的取先出现的。
    let mut best: Option<(u8, AgentKind)> = None;
    for process in &job.processes {
        let name = effective_name(process);
        let Some(kind) = agent_from_name(&name).filter(|&kind| usable(process, &name, kind)) else {
            continue;
        };
        let score = if !name.eq_ignore_ascii_case(&process.name) {
            3
        } else if !is_runtime_or_shell(&name) {
            2
        } else {
            1
        };
        if best.is_none_or(|(best, _)| score > best) {
            best = Some((score, kind));
        }
    }
    best.map(|(_, kind)| kind)
}

/// 容易和别的程序撞名的 agent 名字（查表用的名字，见 `lookup_name`）。前台进程按这些名字认出来时
/// 还要 `installed` 成立才算 agent；别名里带 agent 字样的（`kilo-code`、`trae-cli`）不在这里。
const AMBIGUOUS_NAMES: &[&str] = &["kilo", "cn", "amp", "pi", "vibe", "muse", "jules", "goose", "crush", "trae"];

/// 进程是不是装在 agent 常见的安装位置（`install_location`）：运行时或 shell 跑脚本时看脚本，
/// 运行时自己装在哪不算；否则看可执行文件和 argv[0]。node、bun 用 `process.title` 把 argv[0]
/// 改成了别的名字的也算，agent 的 npm 包常这样做。
fn installed(process: &ForegroundProcess) -> bool {
    let argv = process.argv.as_deref().unwrap_or_default();
    if let Some(script) = argv.first().and_then(|runtime| script_path(runtime, argv)) {
        return install_location(script);
    }
    let retitled = matches!(lookup_name(&process.name).as_str(), "node" | "bun")
        && process.argv0.as_deref().is_some_and(|argv0| !argv0.eq_ignore_ascii_case(&process.name));
    retitled || process.exe.as_deref().into_iter().chain(argv.first().map(String::as_str)).any(install_location)
}

/// 路径（原样或顺着符号链接找到的真正文件）在包管理器装程序的地方：npm、pnpm、bun 的
/// `node_modules`，pipx、uv 和各家安装脚本用的 `~/.local/bin`，Homebrew 的 `Cellar`。只有名字、
/// 没有目录的不算。
fn install_location(path: &str) -> bool {
    let path = std::path::Path::new(path.trim_matches(['"', '\'']));
    if path.components().count() < 2 {
        return false;
    }
    let known = |path: &std::path::Path| {
        let parts: Vec<&str> = path.iter().filter_map(|part| part.to_str()).collect();
        parts.iter().any(|part| matches!(*part, "node_modules" | "Cellar"))
            || parts.windows(2).any(|pair| pair == [".local", "bin"])
    };
    known(path) || std::fs::canonicalize(path).is_ok_and(|real| known(&real))
}

/// 按名字认 agent：命令名、路径或别名都行，不分大小写，`.exe`、`.js` 这类后缀不算。
pub fn agent_from_name(name: &str) -> Option<AgentKind> {
    use AgentKind::*;
    let name = lookup_name(name);
    let kind = match basename(&name) {
        "pi" => Pi,
        "claude" | "claude-code" => Claude,
        "codex" => Codex,
        "gemini" => Gemini,
        "cursor" | "cursor-agent" => Cursor,
        "devin" | "devin-cli" | "devin cli" => Devin,
        "agy" | "antigravity" | "antigravity-cli" => Antigravity,
        "cline" | ".cline" => Cline,
        "omp" => Omp,
        "mastracode" | "mastra-code" | "mastra code" => Mastracode,
        "opencode" | "opencode2" | "open-code" => OpenCode,
        "copilot" | "github-copilot" | "ghcs" => GithubCopilot,
        "kimi" | "kimi-code" | "kimi code" => Kimi,
        "kiro" | "kiro-cli" => Kiro,
        "droid" => Droid,
        "amp" | "amp-local" => Amp,
        "grok" | "grok-build" => Grok,
        "hermes" | "hermes-agent" => Hermes,
        "kilo" | "kilo-code" | "kilo code" => Kilo,
        "qodercli" | "qoderclicn" | "qoder" | "qodercn" => Qodercli,
        "qwen" | "qwen-code" | "qwen code" => Qwen,
        "letta" | "letta-code" | "letta code" => Letta,
        "maki" => Maki,
        "muse" | "muse-code" | "muse-cli" => Muse,
        "aider" => Aider,
        "goose" => Goose,
        "crush" => Crush,
        "auggie" | "augment" => Auggie,
        "cn" | "continue-cli" => ContinueCli,
        "junie" => Junie,
        "openhands" | "open-hands" => OpenHands,
        "trae" | "trae-cli" | "trae-agent" => Trae,
        "codebuddy" | "codebuddy-code" => CodeBuddy,
        "iflow" | "iflow-cli" => Iflow,
        "codebuff" => Codebuff,
        "vibe" | "mistral-vibe" => MistralVibe,
        "jules" => Jules,
        "plandex" | "pdx" => Plandex,
        // muse 的启动脚本最后执行的是带版本号的 `muse-bin-<版本>`。
        other if other.strip_prefix("muse-bin-").is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit())) => {
            Muse
        }
        _ => return None,
    };
    Some(kind)
}

/// 查表用的名字：去掉首尾空白、转成小写、去掉一个可执行文件或脚本的后缀。
fn lookup_name(name: &str) -> String {
    let mut name = name.trim().to_lowercase();
    if let Some(suffix) = [".exe", ".cmd", ".bat", ".ps1", ".js"].iter().find(|suffix| name.ends_with(*suffix)) {
        name.truncate(name.len() - suffix.len());
    }
    name
}

/// 路径的最后一段，结尾的分隔符不算。
fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).find(|part| !part.is_empty()).unwrap_or(path)
}

/// 一个进程实际代表的程序名：通用的运行时或 shell 看它跑的脚本，自己就是 agent 的用自己，
/// 否则看参数里的 argv[0]。都认不出时是 argv[0] 或进程名。
fn effective_name(process: &ForegroundProcess) -> String {
    let own = process.argv0.as_deref().unwrap_or(&process.name);
    let argv = process.argv.as_deref();
    if is_runtime_or_shell(own)
        && let Some(wrapped) = argv.and_then(|argv| wrapped_agent(own, argv))
    {
        return wrapped;
    }
    if agent_from_name(own).is_some() {
        return own.to_owned();
    }
    // argv[0] 被改成别的名字的 node、bun 进程（比如 `MainThread`）：参数里跑的还是那个脚本。
    // 只对这几个 agent 这样认，它们的进程名常常认不出来。
    if let Some(argv) = argv
        && let Some(runtime) = argv.first()
        && matches!(lookup_name(basename(runtime)).as_str(), "node" | "bun")
        && let Some(wrapped) = wrapped_agent(runtime, argv)
        && matches!(agent_from_name(&wrapped), Some(AgentKind::Qwen | AgentKind::Cline | AgentKind::Letta))
    {
        return wrapped;
    }
    if let Some(name) = argv.and_then(|argv| argv.first()).and_then(|first| agent_from_path(first)) {
        return name;
    }
    own.to_owned()
}

/// node、bun 直接给代码的选项。
const JS_EVAL_FLAGS: &[&str] = &["-e", "--eval", "-p", "--print"];

/// 运行时或 shell 的参数里跑的那个 agent 的短名。`-e`、`-c` 这类直接给代码的不算。
fn wrapped_agent(runtime: &str, argv: &[String]) -> Option<String> {
    if is_python(&lookup_name(basename(runtime)))
        && let Some(hermes) = hermes_installer(argv)
    {
        return Some(hermes);
    }
    script_path(runtime, argv).and_then(agent_from_path)
}

/// 运行时或 shell 的参数里要跑的脚本，见 `script_index`。
fn script_path<'a>(runtime: &str, argv: &'a [String]) -> Option<&'a str> {
    let i = match lookup_name(basename(runtime)).as_str() {
        "node" | "bun" => script_index(argv, JS_EVAL_FLAGS, &[]),
        "sh" | "bash" | "zsh" | "fish" => script_index(argv, &["-c"], &[]),
        name if is_python(name) => script_index(argv, &["-c"], &["-m"]),
        _ => None,
    }?;
    Some(&argv[i])
}

/// 参数里第一个不是选项的（或者 `--` 后面那个）就是要跑的脚本，返回它在 `argv` 里的下标。遇到
/// `eval_flags` 或 `module_flags`（直接给代码、跑模块）时为空。
fn script_index(argv: &[String], eval_flags: &[&str], module_flags: &[&str]) -> Option<usize> {
    let mut i = 1;
    while let Some(arg) = argv.get(i) {
        if arg == "--" {
            return (i + 1 < argv.len()).then_some(i + 1);
        }
        if flag_matches(arg, eval_flags) || flag_matches(arg, module_flags) {
            return None;
        }
        if arg.starts_with('-') {
            i += if option_takes_value(arg) { 2 } else { 1 };
            continue;
        }
        return Some(i);
    }
    None
}

/// `arg` 是不是 `flags` 里的某个选项：`-e`、`-ecode`（短选项连着值）或 `--eval=code`。
fn flag_matches(arg: &str, flags: &[&str]) -> bool {
    flags.iter().any(|flag| {
        arg == *flag
            || (!flag.starts_with("--") && flag.starts_with('-') && arg.len() > flag.len() && arg.starts_with(flag))
            || (flag.starts_with("--") && arg.strip_prefix(flag).is_some_and(|rest| rest.starts_with('=')))
    })
}

/// 运行时里后面要跟一个值的选项。
fn option_takes_value(arg: &str) -> bool {
    matches!(
        arg,
        "-r" | "--require"
            | "--loader"
            | "--import"
            | "--experimental-loader"
            | "--inspect-port"
            | "-W"
            | "-X"
            | "-S"
            | "-L"
            | "-o"
    )
}

/// 一个路径或命令名指的是哪个 agent，返回短名：先看文件名，再认 npm 包里的入口文件，最后
/// 顺着符号链接找到真正的文件再看一次文件名。
fn agent_from_path(token: &str) -> Option<String> {
    let token = token.trim_matches(['"', '\'']);
    if token.is_empty() || token.starts_with('-') {
        return None;
    }
    agent_from_name(basename(token))
        .or_else(|| agent_from_package_path(token))
        .or_else(|| {
            let path = std::path::Path::new(token);
            if path.components().count() < 2 {
                return None;
            }
            let resolved = std::fs::canonicalize(path).ok()?;
            agent_from_name(resolved.file_name()?.to_str()?)
        })
        .map(|kind| kind.label().to_owned())
}

/// 装在 npm 包里的 agent 的入口文件。
fn agent_from_package_path(path: &str) -> Option<AgentKind> {
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|part| !part.is_empty()).collect();
    let ends_with = |suffix: &[&str]| {
        parts.len() >= suffix.len()
            && parts[parts.len() - suffix.len()..]
                .iter()
                .zip(suffix)
                .all(|(part, want)| part.eq_ignore_ascii_case(want))
    };
    if ends_with(&["node_modules", "@earendil-works", "pi-coding-agent", "dist", "cli.js"])
        || ends_with(&["node_modules", "@earendil-works", "pi-coding-agent", "dist", "bundle", "cli.js"])
    {
        return Some(AgentKind::Pi);
    }
    if ends_with(&["node_modules", "@oh-my-pi", "pi-coding-agent", "dist", "cli.js"]) {
        return Some(AgentKind::Omp);
    }
    if ends_with(&["node_modules", "@moonshot-ai", "kimi-code", "dist", "main.mjs"]) {
        return Some(AgentKind::Kimi);
    }
    // 下面几个入口可能带也可能不带 `.js` 后缀，按查表用的名字比。
    let names: Vec<String> = parts.iter().map(|part| lookup_name(part)).collect();
    let has = |window: &[&str]| names.windows(window.len()).any(|w| w.iter().zip(window).all(|(a, b)| a == b));
    if has(&["node_modules", "@qwen-code", "qwen-code", "dist", "index"]) {
        return Some(AgentKind::Qwen);
    }
    if has(&["node_modules", "mastracode", "dist", "cli"]) {
        return Some(AgentKind::Mastracode);
    }
    if has(&["node_modules", "@letta-ai", "letta-code", "letta"]) {
        return Some(AgentKind::Letta);
    }
    None
}

fn is_runtime_or_shell(name: &str) -> bool {
    let name = lookup_name(basename(name));
    is_python(&name)
        || matches!(
            name.as_str(),
            "sh" | "bash" | "zsh" | "fish" | "tmux" | "node" | "bun" | "cmd" | "powershell" | "pwsh"
        )
}

/// `python`、`python3`、`python3.12` 这样的名字。
fn is_python(name: &str) -> bool {
    name == "python"
        || name.strip_prefix("python").is_some_and(|version| {
            !version.is_empty()
                && version.split('.').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// letta 的进程是不是交互式会话：带 `--prompt`、`--json` 这类一次性参数，或者跑的是
/// `server`、`agents list` 这类子命令的都不算，它们不会停下来等用户。
fn letta_is_interactive(process: &ForegroundProcess) -> bool {
    let Some(argv) = process.argv.as_deref().filter(|argv| !argv.is_empty()) else {
        return true;
    };
    let args = letta_entry(argv).map_or(argv, |i| &argv[i + 1..]);
    let one_shot = args.iter().any(|arg| {
        let option = arg.split_once('=').map_or(arg.as_str(), |(name, _)| name);
        matches!(
            option,
            "-p" | "--print"
                | "--prompt"
                | "--json"
                | "--stream-json"
                | "--run"
                | "--disable-memory-guard"
                | "--output-format"
                | "--input-format"
                | "--include-partial-messages"
                | "--from-agent"
                | "--environment"
                | "--env"
                | "--pre-load-skills"
                | "--tags"
                | "--ephemeral"
                | "--stateless"
                | "--max-turns"
                | "--memfs-startup"
                | "-h"
                | "--help"
                | "-v"
                | "--version"
                | "--info"
                | "--update"
                | "--upgrade"
        )
    });
    if one_shot {
        return false;
    }
    // 跳过选后端的参数后，第一个参数要么没有，要么是选项；是别的词就是子命令或一次性提示词。
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if arg == "--backend" {
            rest.next();
            continue;
        }
        if arg.starts_with("--backend=") {
            continue;
        }
        return arg.starts_with('-');
    }
    true
}

/// letta 入口（它自己，或者 node、bun 跑的 letta 脚本）在参数里的位置。
fn letta_entry(argv: &[String]) -> Option<usize> {
    let is_letta = |arg: &str| agent_from_path(arg).as_deref() == Some(AgentKind::Letta.label());
    if is_letta(&argv[0]) {
        return Some(0);
    }
    if !matches!(lookup_name(basename(&argv[0])).as_str(), "node" | "bun") {
        return None;
    }
    script_index(argv, JS_EVAL_FLAGS, &[]).filter(|&i| is_letta(&argv[i]))
}

/// hermes 的安装程序用 `python -I -c <固定的启动代码>` 启动它。只认这段启动代码原样出现、
/// 里面两处安装目录一致的情况，不去解析任意 python 代码。
fn hermes_installer(argv: &[String]) -> Option<String> {
    let [_, isolation, flag, code, rest @ ..] = argv else {
        return None;
    };
    if isolation != "-I"
        || flag != "-c"
        || rest.first().is_some_and(|arg| matches!(arg.as_str(), "--run-module" | "--print-runtime-command"))
    {
        return None;
    }
    let rest = code.strip_prefix(HERMES_PREFIX)?;
    let (root, rest) = rest.split_once("')\n")?;
    if root.is_empty() || root.contains(['\'', '\\', '\n', '\r']) {
        return None;
    }
    let rest = rest.strip_prefix(HERMES_MIDDLE)?.strip_prefix(root)?;
    (rest == HERMES_SUFFIX).then(|| AgentKind::Hermes.label().to_owned())
}

// hermes 安装程序的启动代码，只有安装目录会变，出现在 `sys.path.insert` 和 `Path(...)` 两处。
const HERMES_PREFIX: &str = "import os, re, sys
os.environ.pop('PYTHONHOME', None)
os.environ.pop('PYTHONPATH', None)
sys.path.insert(0, '";
const HERMES_MIDDLE: &str = "if sys.argv[1:2] == ['--print-runtime-command']: sys.dont_write_bytecode = True
from hermes_constants import get_default_hermes_root
os.environ['HERMES_HOME'] = os.environ.get('HERMES_HOME') or str(get_default_hermes_root())
if sys.argv[1:2] == ['--print-runtime-command']:
    from pathlib import Path
    from hermes_cli._launchers import print_runtime_command
    print_runtime_command(Path('";
const HERMES_SUFFIX: &str = r"'), sys.argv[2:])
    sys.exit(0)
import hermes_bootstrap
if sys.argv[1:2] == ['--run-module']:
    import runpy
    if len(sys.argv) < 3: sys.exit('hermes: --run-module needs a module')
    module = sys.argv.pop(2)
    del sys.argv[1]
    runpy.run_module(module, run_name='__main__', alter_sys=True)
    sys.exit(0)
from hermes_cli.main import main
sys.argv[0] = re.sub(r'(-script\.pyw|\.exe)?$', '', sys.argv[0])
sys.exit(main())
";

#[cfg(test)]
mod tests {
    use super::*;
    use AgentKind::*;

    fn process(pid: u32, name: &str, argv: &[&str]) -> ForegroundProcess {
        ForegroundProcess {
            pid,
            name: name.into(),
            exe: None,
            argv0: None,
            argv: Some(argv.iter().map(|arg| (*arg).to_owned()).collect()),
        }
    }

    fn job(processes: Vec<ForegroundProcess>) -> ForegroundJob {
        ForegroundJob { leader: 123, processes }
    }

    fn alone(name: &str, argv: &[&str]) -> Option<AgentKind> {
        identify_job(&job(vec![process(123, name, argv)]))
    }

    #[test]
    fn hermes_installer_bootstrap() {
        let root = "/opt/hermes";
        let code = format!("{HERMES_PREFIX}{root}')\n{HERMES_MIDDLE}{root}{HERMES_SUFFIX}");
        assert_eq!(alone("python3", &["python3", "-I", "-c", &code]), Some(Hermes));
        assert_eq!(alone("python3", &["python3", "-I", "-c", &code, "--run-module", "x"]), None);
        let mismatched = format!("{HERMES_PREFIX}{root}')\n{HERMES_MIDDLE}/elsewhere{HERMES_SUFFIX}");
        assert_eq!(alone("python3", &["python3", "-I", "-c", &mismatched]), None);
    }
}
