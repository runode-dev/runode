//! 让本机的 AI 编程 agent 给要提交的改动写提交说明，写好了填进提交说明框。有暂存的改动时只看
//! 暂存的，没有时看全部改动，和提交时一样。
//!
//! 用哪个 agent、加什么 CLI 参数、提示词模板是一份预设（`Recipe`），存在 runode 根目录的
//! `commit-message.json`（`Dirs::commit_message_file`）：所有仓库的默认值，和按仓库根目录分开的。
//! 说明框里的 ✨ 按钮按这个仓库的预设直接写，还没存过预设时打开对话框；对话框（`dialog`）里能临时
//! 换着试，也能存成预设。各家 agent 的调用方式见 `agents`；不在其中的，用户可以自己写一条命令
//! （`Recipe::command`）。

mod agents;
mod dialog;

pub(in crate::window) use dialog::CommitMessageDialog;

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs, io,
    io::{ErrorKind, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Mutex, MutexGuard, PoisonError},
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

use crate::git_panel::show_git_error;
use gpui::{Context, Window};
use runode_git::{self as git, GitError, Section};
use runode_paths::Dirs;
use serde::{Deserialize, Serialize};

use crate::window::WindowView;
use agents::Output;

/// 交给 agent 的改动最多这么多字节，多出来的截掉：大改动整份塞进去又慢又贵。
const MAX_PATCH_BYTES: usize = 100_000;
/// 改动的文件列表最多这么多字节。
const MAX_FILES_BYTES: usize = 6_000;
/// 照着写的最近提交说明的条数。
const EXAMPLES: usize = 10;
/// agent 最多等这么久，超时就杀掉报错，免得仓库一直写着在生成、提交不了。
const TIMEOUT: Duration = Duration::from_secs(180);
/// 问登录 shell 的 PATH 最多等这么久：配置里有等终端输入的东西时不至于卡住。
const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(10);
/// 到时限杀掉整组以后，再等输出管道关上这么久。
const KILL_GRACE: Duration = Duration::from_secs(1);

/// 默认的提示词模板：就是内置的提示词。
pub(in crate::window) const DEFAULT_TEMPLATE: &str = "{basePrompt}";
/// 模板里能用的变量（不带花括号），对话框里列给用户看。
pub(in crate::window) const VARIABLES: &[&str] = &["basePrompt", "branch", "stagedFiles", "stagedPatch"];

/// 对话框里鼠标停在变量 `name` 上时的说明：它是什么，下面是按一份示例改动展开后的样子。
pub(in crate::window) fn variable_preview(name: &str) -> String {
    let context = PromptContext {
        staged: true,
        branch: "feature/example".to_owned(),
        files: "M src/example.rs\nA src/example/new.rs".to_owned(),
    };
    let patch = "diff --git a/src/example.rs b/src/example.rs\n+fn example() {}";
    let example = match name {
        "basePrompt" => base_prompt(&context, &["feat: add an example".to_owned()], patch),
        "branch" => context.branch,
        "stagedFiles" => context.files,
        _ => patch.to_owned(),
    };
    format!("{}\n\n{example}", rust_i18n::t!(format!("git.ai_message.var.{name}")))
}

/// 写提交说明的一份预设。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::window) struct Recipe {
    /// `agents::AGENTS` 里的 `id`，或者 `agents::CUSTOM_AGENT`。
    pub agent: String,
    /// 加在 agent 自己的参数后面，按 shell 的规矩分词，不展开变量。
    #[serde(default)]
    pub args: String,
    #[serde(default = "default_template")]
    pub template: String,
    /// 自己定义的命令：按 shell 的规矩分词，开头的 `NAME=value` 是环境变量，接着是程序，参数里的
    /// `{prompt}` 换成提示词；没有 `{prompt}` 时提示词经标准输入给。和选哪个 agent 分开存，换来换去不丢。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
}

fn default_template() -> String {
    DEFAULT_TEMPLATE.to_owned()
}

impl Default for Recipe {
    fn default() -> Self {
        Self {
            agent: agents::DEFAULT_AGENT.to_owned(),
            args: String::new(),
            template: default_template(),
            command: String::new(),
        }
    }
}

/// 预设文件的内容。
#[derive(Default, Serialize, Deserialize)]
struct Recipes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default: Option<Recipe>,
    /// 键是仓库根目录。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    repos: BTreeMap<String, Recipe>,
}

/// 根目录是 `root` 的仓库存着的预设：它自己的，没有时是所有仓库的默认值；第二项说是不是它自己的。
/// 都没存过时为空。
pub(in crate::window) fn saved_recipe(root: &Path) -> Result<Option<(Recipe, bool)>, String> {
    let file = Dirs::from_env().commit_message_file().ok_or("no home directory")?;
    let mut recipes = read_recipes(&file)?;
    Ok(match recipes.repos.remove(&*root.to_string_lossy()) {
        Some(recipe) => Some((recipe, true)),
        None => recipes.default.map(|recipe| (recipe, false)),
    })
}

/// 把 `recipe` 存成根目录是 `root` 的仓库自己的（`root` 为空时存成所有仓库的默认值）。存成默认值时
/// 不动各仓库自己的。读不懂原来的内容时不写，免得把手改的文件冲掉。
pub(in crate::window) fn save_recipe(root: Option<&Path>, recipe: Recipe) -> Result<(), String> {
    // 几个窗口的对话框可能同时在后台存：读、改、写整个过程排着队来，免得后存的拿着旧内容把先存的冲掉。
    static SAVING: Mutex<()> = Mutex::new(());
    let _saving = SAVING.lock().unwrap_or_else(PoisonError::into_inner);
    let file = Dirs::from_env().commit_message_file().ok_or("no home directory")?;
    let mut recipes = read_recipes(&file)?;
    match root {
        Some(root) => {
            recipes.repos.insert(root.to_string_lossy().into_owned(), recipe);
        }
        None => recipes.default = Some(recipe),
    }
    let mut text = serde_json::to_string_pretty(&recipes).map_err(|err| err.to_string())?;
    text.push('\n');
    let dir = file.parent().map(PathBuf::from).unwrap_or_default();
    // 用户手写过的文件：写临时文件再换上去，写到一半出错不留下空的或半截的。
    fs::create_dir_all(&dir)
        .and_then(|()| runode_paths::replace_file(&file, text.as_bytes()))
        .map_err(|err| format!("{}: {err}", file.display()))
}

fn read_recipes(file: &Path) -> Result<Recipes, String> {
    match fs::read_to_string(file) {
        Ok(text) if text.trim().is_empty() => Ok(Recipes::default()),
        Ok(text) => serde_json::from_str(&text).map_err(|err| format!("{}: {err}", file.display())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Recipes::default()),
        Err(err) => Err(format!("{}: {err}", file.display())),
    }
}

/// 提示词里除了改动本身以外的东西，在界面线程上从快照里取好。
struct PromptContext {
    staged: bool,
    branch: String,
    /// 一行一个改动的文件：状态字母和路径。
    files: String,
}

impl WindowView {
    /// 说明框里的 ✨ 按钮：按根目录是 `root` 的仓库存着的预设写；还没存过预设时打开对话框。
    pub(super) fn generate_commit_message(&mut self, root: &Path, window: &mut Window, cx: &mut Context<Self>) {
        match saved_recipe(root) {
            Ok(Some((recipe, _))) => self.write_commit_message(root, recipe, window, cx),
            Ok(None) => self.open_commit_message_dialog(root.to_path_buf(), window, cx),
            Err(message) => show_git_error(&GitError { message }, window, cx),
        }
    }

    /// 按 `recipe` 给根目录是 `root` 的仓库要提交的改动写提交说明，写好了替换提交说明框里的字。经 Git 面板跑，
    /// 和这个仓库别的操作一起排队，写的时候面板写着在写说明。
    pub(in crate::window) fn write_commit_message(
        &mut self,
        root: &Path,
        recipe: Recipe,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project = &self.workspace().project;
        let Some(repo) = project.git.as_ref().and_then(|git| git.iter().find(|repo| repo.root == root)) else {
            return;
        };
        if repo.is_clean() {
            return;
        }
        let staged = !repo.staged.is_empty();
        // 没有暂存的改动时提交会先全部暂存，这时 `unstaged` 就是全部。
        let files: Vec<String> = repo
            .files(if staged { Section::Staged } else { Section::Unstaged })
            .iter()
            .map(|file| format!("{} {}", file.status.letter(), file.path.display()))
            .collect();
        let context = PromptContext {
            staged,
            branch: repo.info.branch.clone().unwrap_or_else(|| "(detached)".to_owned()),
            files: limit(files.join("\n"), MAX_FILES_BYTES),
        };
        // 从访达打开的 app 自己的 PATH 里没有 agent 装的目录，用终端里 shell 报告的 PATH；窗口里
        // 一个终端都没有时在后台问登录 shell。
        let path = self.terminal_path(cx);
        let op =
            move |repo: &git::Repo| generate(repo, &recipe, &context, path).map_err(|message| GitError { message });
        self.with_git_panel(cx, |panel, cx| panel.write_message(root, op, window, cx));
    }
}

fn generate(
    repo: &git::Repo,
    recipe: &Recipe,
    context: &PromptContext,
    path: Option<OsString>,
) -> Result<String, String> {
    // 先看命令写得对不对，再去读改动。
    let command = Invocation::of(recipe)?;
    let patch = limit(repo.pending_diff(context.staged).map_err(|err| err.message)?, MAX_PATCH_BYTES);
    let recent = repo.recent_messages(EXAMPLES).map_err(|err| err.message)?;
    let base = base_prompt(context, &recent, &patch);
    let prompt = render_template(
        &recipe.template,
        &[("basePrompt", &base), ("branch", &context.branch), ("stagedFiles", &context.files), ("stagedPatch", &patch)],
    );
    let (args, stdin) = command.args(&prompt);
    // 内置的 agent 和登录 shell 都在空目录里跑：陌生仓库里的项目配置（`.claude/settings.json` 的钩子、
    // `opencode.json`、direnv 的 `.envrc` 这类）会被它们读到、执行，提示词里已经有要的全部内容。用户自己
    // 写的命令可能就要在仓库里跑（比如自己调 git），照旧在仓库里。
    let scratch = Dirs::from_env()
        .ai_message_dir()
        .ok_or_else(|| "no home directory".to_owned())
        .and_then(|dir| ScratchDir::new(dir).map_err(|err| err.to_string()))?;
    let path = path
        .or_else(|| login_shell_path(&runode_terminal::pty::default_shell(), &scratch.dir))
        .or_else(|| std::env::var_os("PATH"));
    let dir = if command.agent.is_some() { &scratch.dir } else { &repo.root };
    let stdout = run(&command.binary, &args, stdin, dir, path, &command.env, TIMEOUT)?;
    drop(scratch);
    let text = match command.agent.map_or(Output::Text, |agent| agent.output) {
        Output::Text => stdout,
        Output::OpenCodeEvents => opencode_text(&stdout)?,
    };
    let message = clean(&text);
    if message.is_empty() {
        return Err(rust_i18n::t!("git.ai_message.empty").into_owned());
    }
    Ok(message)
}

/// agent 在里面跑的空目录（`Dirs::ai_message_dir`），丢掉时连同 agent 在里面留下的东西一起删掉。路径每次
/// 一样：Claude Code 这类 agent 按工作目录在自己的全局配置里记一条，每次换个新目录的话越记越多。几次生成
/// 同时跑时轮流用它。
struct ScratchDir {
    dir: PathBuf,
    _in_use: MutexGuard<'static, ()>,
}

impl ScratchDir {
    fn new(dir: PathBuf) -> io::Result<Self> {
        static IN_USE: Mutex<()> = Mutex::new(());
        let guard = IN_USE.lock().unwrap_or_else(PoisonError::into_inner);
        // 上次没删掉的（app 被强退）先清掉；`remove_dir_all` 遇到符号链接只删链接本身。
        match fs::remove_dir_all(&dir) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }
        if let Some(parent) = dir.parent() {
            fs::create_dir_all(parent)?;
        }
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut fs::DirBuilder::new(), 0o700).create(&dir)?;
        #[cfg(not(unix))]
        fs::create_dir(&dir)?;
        Ok(Self { dir, _in_use: guard })
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.dir).ok();
    }
}

/// 按预设要跑的命令：哪家 agent（自己定义的命令为空）、程序和用户写的参数。
struct Invocation {
    agent: Option<&'static agents::AgentSpec>,
    /// 自己定义的命令开头 `NAME=value` 写的环境变量，跑的时候加上，盖过同名的。
    env: Vec<(String, String)>,
    binary: String,
    words: Vec<String>,
}

impl Invocation {
    fn of(recipe: &Recipe) -> Result<Self, String> {
        if recipe.agent == agents::CUSTOM_AGENT {
            let mut words = split_args(&recipe.command)?.into_iter().peekable();
            let mut env = Vec::new();
            while let Some(assignment) = words.next_if(|word| env_assignment(word).is_some()) {
                env.extend(env_assignment(&assignment));
            }
            let binary = words.next().ok_or_else(|| rust_i18n::t!("git.ai_message.empty_command").into_owned())?;
            return Ok(Self { agent: None, env, binary, words: words.collect() });
        }
        let agent = agents::agent(&recipe.agent)
            .ok_or_else(|| rust_i18n::t!("git.ai_message.unknown_agent", agent = recipe.agent).into_owned())?;
        let words = split_args(&recipe.args)?;
        Ok(Self { agent: Some(agent), env: Vec::new(), binary: agent.binary.to_owned(), words })
    }

    /// 完整的参数和要经标准输入给的提示词，见 `agents::with_prompt`。
    fn args(&self, prompt: &str) -> (Vec<String>, Option<String>) {
        match self.agent {
            Some(agent) => agent.command_line(self.words.clone(), prompt),
            None => agents::with_prompt(self.words.clone(), prompt),
        }
    }
}

/// `word` 是 shell 里命令前面的 `NAME=value` 时，拆成名字和值。
fn env_assignment(word: &str) -> Option<(String, String)> {
    let (name, value) = word.split_once('=')?;
    let mut chars = name.chars();
    let valid = chars.next().is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
    valid.then(|| (name.to_owned(), value.to_owned()))
}

/// 对话框里给用户看的、按 `recipe` 实际要跑的整条命令，按 shell 的写法加好引号，提示词的位置写成
/// `<prompt>`；第二项说提示词是不是经标准输入给。命令写得不对时是原因。
pub(in crate::window) fn command_preview(recipe: &Recipe) -> Result<(String, bool), String> {
    let command = Invocation::of(recipe)?;
    let (args, stdin) = command.args("<prompt>");
    let env = command.env.iter().map(|(name, value)| format!("{name}={}", shell_quote(value)));
    let words = std::iter::once(&command.binary).chain(&args).map(|word| shell_quote(word));
    let line = env.chain(words).collect::<Vec<_>>();
    Ok((line.join(" "), stdin.is_some()))
}

/// 照 shell 的写法给 `word` 加引号：只有安全的字符时原样，否则整个放进单引号。
fn shell_quote(word: &str) -> String {
    let safe = |ch: char| ch.is_ascii_alphanumeric() || "-_./:=@%+,".contains(ch);
    if !word.is_empty() && word.chars().all(safe) {
        return word.to_owned();
    }
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// 内置的提示词：照着最近几条提交说明的写法写，给它分支、改动的文件和改动。
fn base_prompt(context: &PromptContext, recent: &[String], patch: &str) -> String {
    let recent = if recent.is_empty() { "(none)".to_owned() } else { recent.join("\n---\n") };
    format!(
        "You are generating a single git commit message.
Return only the commit message text. Do not include a preamble, quotes, or code fences.

Rules:
- Match the language, format and style of the recent commit messages below. If there are none, write the \
first line in imperative mood, at most 72 characters, with no trailing period.
- Optional body: blank line, then short wrapped bullet points or prose explaining WHY.
- Capture the primary user-visible or developer-visible change.
- Use only the changes below as context.
- Do not include \"Co-authored-by\" or other git trailers.

Branch: {branch}

Recent commit messages, newest first, separated by ---:
{recent}

Changed files:
{files}

Patch:
```diff
{patch}
```",
        branch = context.branch,
        files = context.files,
    )
}

/// 把模板里的 `{name}` 换成 `vars` 里对应的值，只换一遍：值里碰巧有 `{branch}` 这样的字也不再换。
/// 不认识的 `{…}` 原样留着。
fn render_template(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let var = after.find('}').and_then(|close| {
            let name = &after[..close];
            vars.iter().find(|(key, _)| *key == name).map(|(_, value)| (close, *value))
        });
        match var {
            Some((close, value)) => {
                out.push_str(value);
                rest = &after[close + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// 按 shell 的规矩把 CLI 参数分成一个个参数：空白分开，单引号里的原样；双引号里的反斜杠只转义
/// `"`、`\`、`$`、`` ` `` 和换行，别的字前面的原样留着；引号外的反斜杠转义下一个字，在末尾时原样留着。
/// 反斜杠接换行是续行，两个都去掉。不展开变量、通配符和 `~`：不经 shell 跑。
fn split_args(text: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut current: Option<String> = None;
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\'' | '"' => {
                let arg = current.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some(end) if end == ch => break,
                        Some('\\') if ch == '"' => match chars.next() {
                            Some('\n') => {}
                            Some(next @ ('"' | '\\' | '$' | '`')) => arg.push(next),
                            Some(next) => {
                                arg.push('\\');
                                arg.push(next);
                            }
                            None => return Err(rust_i18n::t!("git.ai_message.unclosed_quote").into_owned()),
                        },
                        Some(other) => arg.push(other),
                        None => return Err(rust_i18n::t!("git.ai_message.unclosed_quote").into_owned()),
                    }
                }
            }
            '\\' => match chars.next() {
                Some('\n') => {}
                next => current.get_or_insert_with(String::new).push(next.unwrap_or('\\')),
            },
            ch if ch.is_whitespace() => args.extend(current.take()),
            ch => current.get_or_insert_with(String::new).push(ch),
        }
    }
    args.extend(current);
    Ok(args)
}

/// 超过 `max` 字节时在那之前的最后一个换行处截断，注明截掉了多少。
fn limit(mut text: String, max: usize) -> String {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let end = text[..end].rfind('\n').unwrap_or(end);
    let omitted = text.len() - end;
    text.truncate(end);
    text.push_str(&format!("\n[truncated: {omitted} bytes omitted]"));
    text
}

/// 用户的登录 shell `shell` 配好以后的 PATH，问不到时为空。交互加登录地起，`.zshrc` 这类只在交互时
/// 读的配置里加的目录也算上；配置里打印的别的东西在 `PATH=` 那行之前，取最后一行。
fn login_shell_path(shell: &str, dir: &Path) -> Option<OsString> {
    let args = ["-i", "-l", "-c", "/usr/bin/env"].map(str::to_owned);
    let env = run(shell, &args, None, dir, None, &[], LOGIN_SHELL_TIMEOUT).ok()?;
    env.lines().rev().find_map(|line| line.strip_prefix("PATH=")).map(OsString::from)
}

/// 在 `dir` 里跑 `binary`，`stdin` 不为空时从标准输入交给它，返回标准输出；过了 `timeout` 杀掉。
/// `env` 加在 `PATH` 之后，用户自己写了 `PATH=` 时用他的。
fn run(
    binary: &str,
    args: &[String],
    stdin: Option<String>,
    dir: &Path,
    path: Option<OsString>,
    env: &[(String, String)],
    timeout: Duration,
) -> Result<String, String> {
    let mut command = Command::new(binary);
    command
        .args(args)
        .current_dir(dir)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // 包装脚本拉起的真 agent、后台的 helper 到时限时一起杀掉。
    crate::subprocess::isolate(&mut command);
    if let Some(path) = path {
        command.env("PATH", path);
    }
    command.envs(env.iter().map(|(name, value)| (name, value)));
    let mut child = command.spawn().map_err(|err| match err.kind() {
        ErrorKind::NotFound => rust_i18n::t!("git.ai_message.missing", binary = binary).into_owned(),
        _ => format!("{binary}: {err}"),
    })?;
    let group = child.id();
    let kill_group = || crate::subprocess::kill(group);
    let timed_out = || rust_i18n::t!("git.ai_message.timeout", binary = binary).into_owned();
    // 提示词和输出都可能比管道的缓冲大，一边等 agent 退出一边写、读，不然两边都停下来等对方。
    let mut pipes = match Pipes::new(&mut child, stdin) {
        Ok(pipes) => pipes,
        Err(err) => {
            kill_group();
            child.wait().ok();
            return Err(format!("{binary}: {err}"));
        }
    };
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|err| err.to_string())? {
            break status;
        }
        let now = Instant::now();
        if now > deadline {
            kill_group();
            child.wait().ok();
            return Err(timed_out());
        }
        pipes.pump((deadline - now).min(POLL_INTERVAL));
    };
    // agent 退了，组里还有进程拿着输出管道时读不到头：等到时限，再杀掉整组读完剩下的。拿着管道的
    // 进程自己另开了进程组或会话、杀不到它时，再等一会儿还读不到头就算超时，不一直等下去；返回时
    // `pipes` 跟着关掉，那个进程再写只会写不进去。
    let mut until = deadline;
    let mut killed = false;
    while !pipes.outputs_closed() {
        let now = Instant::now();
        if now >= until {
            if killed {
                return Err(timed_out());
            }
            kill_group();
            killed = true;
            until = now + KILL_GRACE;
            continue;
        }
        pipes.pump(until - now);
    }
    let (stdout, stderr) = pipes.into_outputs();
    let (stdout, stderr) =
        (String::from_utf8_lossy(&stdout).into_owned(), String::from_utf8_lossy(&stderr).into_owned());
    if !status.success() {
        let detail = [stderr.trim(), stdout.trim()].into_iter().find(|text| !text.is_empty());
        return Err(format!("{binary}: {}", detail.map_or_else(|| status.to_string(), str::to_owned)));
    }
    Ok(stdout)
}

/// 等 agent 退出时最多隔这么久看一次。
const POLL_INTERVAL: Duration = Duration::from_millis(100);

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
    read: std::sync::Arc<(Mutex<Collected>, std::sync::Condvar)>,
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
            thread::Builder::new().name("ai-message-stdin".into()).spawn(move || drop(stdin.write_all(&input)))?;
        }
        let read =
            std::sync::Arc::new((Mutex::new(Collected { open: 2, ..Collected::default() }), std::sync::Condvar::new()));
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
        thread::Builder::new().name("ai-message-stdout".into()).spawn(reader(stdout, false))?;
        thread::Builder::new().name("ai-message-stderr".into()).spawn(reader(stderr, true))?;
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

/// OpenCode `--format json` 的输出里最后一步的文字；出错的事件报它的错。
fn opencode_text(output: &str) -> Result<String, String> {
    let mut parts: Vec<(String, String)> = Vec::new();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let event: serde_json::Value =
            serde_json::from_str(line).map_err(|_| "OpenCode returned invalid JSON events.".to_owned())?;
        let text = |pointer: &str| event.pointer(pointer).and_then(serde_json::Value::as_str);
        match text("/type") {
            Some("step_start") => parts.clear(),
            Some("error") => {
                let message = text("/error/data/message").or(text("/error/message")).or(text("/error/name"));
                return Err(message.unwrap_or("OpenCode reported an error.").to_owned());
            }
            Some("text") => {
                if let Some(part) = text("/part/text") {
                    let id = text("/part/id").map_or_else(|| format!("#{}", parts.len()), str::to_owned);
                    match parts.iter_mut().find(|(known, _)| *known == id) {
                        Some((_, known)) => *known = part.to_owned(),
                        None => parts.push((id, part.to_owned())),
                    }
                }
            }
            _ => {}
        }
    }
    Ok(parts.into_iter().map(|(_, text)| text).collect::<Vec<_>>().join("\n"))
}

/// 去掉 agent 输出里提交说明以外的东西：前后空白、开头的 `<think>…</think>` 推理、把整段包起来的
/// 代码块。
fn clean(text: &str) -> String {
    let mut text = text.replace("\r\n", "\n").trim().to_owned();
    if let Some(rest) = text.strip_prefix("<think>")
        && let Some(close) = rest.find("</think>")
    {
        text = rest[close + "</think>".len()..].trim().to_owned();
    }
    if let Some(rest) = text.strip_prefix("```")
        && let Some(body) = rest.strip_suffix("```")
        && let Some(newline) = body.find('\n')
    {
        text = body[newline + 1..].trim().to_owned();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_variables_once() {
        let vars = [("branch", "main"), ("stagedPatch", "+{branch}")];
        assert_eq!(render_template("{branch}: {stagedPatch} {unknown} {", &vars), "main: +{branch} {unknown} {");
        assert_eq!(render_template("{{branch}}", &vars), "{main}");
    }

    /// 每个变量都有说明（缺了时 `t!` 原样返回键名），`basePrompt` 的示例里带着示例改动。
    #[test]
    fn previews_every_variable() {
        for name in VARIABLES {
            let preview = variable_preview(name);
            assert!(!preview.starts_with("git.ai_message.var."), "{preview}");
        }
        assert!(variable_preview("basePrompt").contains("+fn example() {}"));
    }

    #[test]
    fn splits_args_like_a_shell() {
        assert_eq!(split_args("  --model sonnet ").unwrap(), ["--model", "sonnet"]);
        assert_eq!(split_args(r#"-c 'a b' "x \"y\"" c\ d ''"#).unwrap(), ["-c", "a b", "x \"y\"", "c d", ""]);
        assert!(split_args("'open").is_err());
        // 双引号里只有 `"`、`\`、`$`、`` ` `` 前面的反斜杠是转义，别的原样留着，正则、路径不会被改掉。
        assert_eq!(split_args(r#""\d+" "a\\b" "\$x" "C:\dir""#).unwrap(), [r"\d+", r"a\b", "$x", r"C:\dir"]);
        assert!(split_args(r#""a\""#).is_err());
        // 引号外末尾落单的反斜杠原样留着，不悄悄丢掉；反斜杠接换行是续行。
        assert_eq!(split_args(r"a b\").unwrap(), ["a", r"b\"]);
        assert_eq!(split_args("a\\\nb").unwrap(), ["ab"]);
    }

    #[test]
    fn limits_on_a_line_boundary() {
        assert_eq!(limit("abc".into(), 10), "abc");
        assert_eq!(limit("one\ntwo\nthree".into(), 9), "one\ntwo\n[truncated: 6 bytes omitted]");
    }

    #[test]
    fn cleans_agent_output() {
        assert_eq!(clean("\r\n  fix: x\r\n\r\nbody  \n"), "fix: x\n\nbody");
        assert_eq!(clean("<think>hmm</think>\nfix: x"), "fix: x");
        assert_eq!(clean("```text\nfix: x\n```"), "fix: x");
    }

    #[test]
    fn reads_opencode_events() {
        let output = r#"{"type":"text","part":{"id":"a","text":"draft"}}
{"type":"step_start"}
{"type":"text","part":{"id":"b","text":"fix: x"}}
{"type":"text","part":{"id":"b","text":"fix: y"}}"#;
        assert_eq!(opencode_text(output).unwrap(), "fix: y");
        assert_eq!(opencode_text(r#"{"type":"error","error":{"message":"boom"}}"#).unwrap_err(), "boom");
        assert!(opencode_text("oops").is_err());
    }

    #[test]
    fn puts_the_prompt_on_argv_or_stdin() {
        let claude = agents::agent("claude").unwrap();
        let (args, stdin) = claude.command_line(vec!["--model".into(), "haiku".into()], "P");
        assert_eq!(args.last().map(String::as_str), Some("haiku"));
        assert_eq!(stdin.as_deref(), Some("P"));
        let agy = agents::agent("antigravity").unwrap();
        let (args, stdin) = agy.command_line(vec![], "-P");
        assert_eq!(args, ["--sandbox", "--print=-P"]);
        assert_eq!(stdin, None);
        let (args, stdin) = agents::with_prompt(vec!["--ask".into(), "x{prompt}".into()], "P");
        assert_eq!((args, stdin), (vec!["--ask".to_owned(), "xP".to_owned()], None));
        let muse = agents::agent("muse").unwrap();
        let (args, _) = muse.command_line(vec!["--model".into(), "m".into()], "P");
        assert_eq!(&args[args.len() - 4..], ["--model", "m", "--", "P"]);
    }

    #[test]
    fn previews_the_whole_command() {
        let recipe = Recipe { args: "--model 'my model'".into(), ..Recipe::default() };
        let (line, stdin) = command_preview(&recipe).unwrap();
        assert_eq!(
            line,
            "claude -p --output-format text --permission-mode plan --no-session-persistence --model 'my model'"
        );
        assert!(stdin);
        let recipe = Recipe { agent: "custom".into(), command: "tool --ask {prompt} it's".into(), ..Recipe::default() };
        assert!(command_preview(&recipe).is_err());
        let recipe = Recipe { agent: "custom".into(), command: "tool --ask={prompt}".into(), ..Recipe::default() };
        assert_eq!(command_preview(&recipe).unwrap(), ("tool '--ask=<prompt>'".to_owned(), false));
        // 开头的 `NAME=value` 是环境变量，后面的 `=` 照旧是参数。
        let command = "A=1 _B='x y' tool C=2";
        let recipe = Recipe { agent: "custom".into(), command: command.into(), ..Recipe::default() };
        assert_eq!(command_preview(&recipe).unwrap().0, "A=1 _B='x y' tool C=2");
        let invocation = Invocation::of(&recipe).unwrap();
        assert_eq!(invocation.env, [("A".to_owned(), "1".to_owned()), ("_B".to_owned(), "x y".to_owned())]);
        assert_eq!((invocation.binary.as_str(), invocation.words.as_slice()), ("tool", &["C=2".to_owned()][..]));
        let recipe = Recipe { agent: "custom".into(), command: "A=1".into(), ..Recipe::default() };
        assert!(command_preview(&recipe).is_err());
        let recipe = Recipe { agent: "custom".into(), command: "1A=x ./tool".into(), ..Recipe::default() };
        assert_eq!(Invocation::of(&recipe).unwrap().binary, "1A=x");
    }

    /// 提示词经标准输入给，比管道的缓冲大也不卡；失败时报标准错误。
    #[cfg(unix)]
    #[test]
    fn runs_a_command_with_the_prompt_on_stdin() {
        let dir = std::env::temp_dir();
        let prompt = "x".repeat(200_000);
        let out = run("sh", &["-c".into(), "wc -c".into()], Some(prompt), &dir, None, &[], TIMEOUT).unwrap();
        assert_eq!(out.trim(), "200000");
        let err =
            run("sh", &["-c".into(), "echo boom >&2; exit 3".into()], None, &dir, None, &[], TIMEOUT).unwrap_err();
        assert_eq!(err, "sh: boom");
        let missing = run("runode-no-such-agent", &[], None, &dir, None, &[], TIMEOUT).unwrap_err();
        assert!(missing.contains("runode-no-such-agent"));
        let env = [("RUNODE_TEST_VAR".to_owned(), "a b".to_owned())];
        let out =
            run("sh", &["-c".into(), "echo \"$RUNODE_TEST_VAR\"".into()], None, &dir, None, &env, TIMEOUT).unwrap();
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
        assert_eq!(err, "cmd: boom");
        let started = Instant::now();
        let ping = ["/c".into(), "ping -n 30 127.0.0.1".into()];
        let err = run("cmd", &ping, None, &dir, None, &[], Duration::from_secs(1)).unwrap_err();
        assert!(err.contains("cmd"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[cfg(unix)]
    #[test]
    fn asks_the_login_shell_for_its_path() {
        let path = login_shell_path("sh", &std::env::temp_dir()).unwrap();
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
        assert!(err.contains("sh"));
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
        let err =
            run("bash", &["-c".into(), script.into()], None, &dir, None, &[], Duration::from_secs(1)).unwrap_err();
        // 比整句：错误里本来就带着程序名 bash，只看含不含 "sh" 什么错都能通过。
        assert_eq!(err, rust_i18n::t!("git.ai_message.timeout", binary = "bash"));
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    /// 那样的后代一直拿着输出管道时，`run` 返回时把管道也关上了：没有读线程替它收着，它之后再写
    /// 只会写不进去（EPIPE），不会每重试一次就漏一份。
    #[cfg(unix)]
    #[test]
    fn closes_pipes_held_outside_the_process_group() {
        let dir = std::env::temp_dir();
        let marker = dir.join(format!("runode-ai-message-pipes-{}", std::process::id()));
        fs::remove_file(&marker).ok();
        // 后台的子 shell 自成一个进程组、不理 SIGPIPE，等 `run` 返回以后往标准输出写一行，把写不写得
        // 进去记到 `$1` 里。和上面一样指名用 bash 开作业控制。
        let script = r#"set -m; (trap '' PIPE; sleep 5; if echo late 2>/dev/null; then r=open; else r=closed; fi
            echo $r > "$1") & echo hi"#;
        let args = ["-c".into(), script.into(), "sh".into(), marker.display().to_string()];
        let started = Instant::now();
        let err = run("bash", &args, None, &dir, None, &[], Duration::from_secs(1)).unwrap_err();
        assert_eq!(err, rust_i18n::t!("git.ai_message.timeout", binary = "bash"));
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

    /// 每次都是空目录，用完连里面的东西一起删掉；上次没删掉的下次清空。
    #[test]
    fn scratch_dirs_are_fresh_and_removed() {
        let dir = std::env::temp_dir().join(format!("runode-ai-message-test-{}", std::process::id()));
        let a = ScratchDir::new(dir.clone()).unwrap();
        assert_eq!(fs::read_dir(&a.dir).unwrap().count(), 0);
        fs::write(a.dir.join("left-over"), "x").unwrap();
        drop(a);
        assert!(!dir.exists());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("crashed"), "x").unwrap();
        let b = ScratchDir::new(dir).unwrap();
        assert_eq!(fs::read_dir(&b.dir).unwrap().count(), 0);
    }

    #[test]
    fn recipe_files_keep_defaults_and_repos_apart() {
        let recipes: Recipes = serde_json::from_str(r#"{"repos": {"/r": {"agent": "codex"}}}"#).unwrap();
        assert_eq!(recipes.repos["/r"], Recipe { agent: "codex".into(), ..Recipe::default() });
        assert!(recipes.default.is_none());
        let text = serde_json::to_string(&Recipes { default: Some(Recipe::default()), ..Default::default() }).unwrap();
        assert_eq!(text, r#"{"default":{"agent":"claude","args":"","template":"{basePrompt}"}}"#);
    }
}
