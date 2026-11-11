//! 让本机的 AI 编程 agent 给要提交的改动写提交说明里不碰界面的部分：预设、提示词，在后台跑 agent、
//! 取它的输出。有暂存的改动时只看暂存的，没有时看全部改动，和提交时一样。
//!
//! 用哪个 agent、加什么 CLI 参数、提示词模板是一份预设（`Recipe`），存在 runode 根目录的
//! `commit-message.json`（`Dirs::commit_message_file`）：所有仓库的默认值，和按仓库根目录分开的。
//! 内置的 agent 各有调用方式（`agents::AGENTS`）；不在其中的，用户可以自己写一条命令（`Recipe::command`）。

pub(crate) mod agents;

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs, io,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use crate::git_view::git_error_text;
use runode_agent::oneshot::{self, RunError};
use runode_paths::Dirs;
use serde::{Deserialize, Serialize};

use crate::project_host::ProjectRepo;

/// 交给 agent 的改动最多这么多字节，多出来的截掉：大改动整份塞进去又慢又贵。
const MAX_PATCH_BYTES: usize = 100_000;
/// 提示词放在参数里时整个提示词最多这么多字节，超出时先少给改动。Windows 的整条命令行最多 32767 个 UTF-16
/// 码元，参数里的引号要转义，最坏时翻一倍；Linux 的单个参数最多 128 KiB。
const MAX_ARGV_PROMPT_BYTES: usize = if cfg!(windows) { 15_000 } else { 120_000 };
/// 改动的文件列表最多这么多字节。
pub(crate) const MAX_FILES_BYTES: usize = 6_000;
/// 照着写的最近提交说明的条数。
const EXAMPLES: usize = 10;
/// agent 最多等这么久，超时就杀掉报错，免得仓库一直写着在生成、提交不了。
const TIMEOUT: Duration = Duration::from_secs(180);

/// 默认的提示词模板：就是内置的提示词。
pub(crate) const DEFAULT_TEMPLATE: &str = "{basePrompt}";
/// 模板里能用的变量（不带花括号），对话框里列给用户看。
pub(crate) const VARIABLES: &[&str] = &["basePrompt", "branch", "stagedFiles", "stagedPatch"];

/// 对话框里鼠标停在变量 `name` 上时的说明：它是什么，下面是按一份示例改动展开后的样子。
pub(crate) fn variable_preview(name: &str) -> String {
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
pub(crate) struct Recipe {
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
pub(crate) fn saved_recipe(root: &Path) -> Result<Option<(Recipe, bool)>, String> {
    let file = Dirs::from_env().commit_message_file().ok_or("no home directory")?;
    let mut recipes = read_recipes(&file)?;
    Ok(match recipes.repos.remove(&*root.to_string_lossy()) {
        Some(recipe) => Some((recipe, true)),
        None => recipes.default.map(|recipe| (recipe, false)),
    })
}

/// 把 `recipe` 存成根目录是 `root` 的仓库自己的（`root` 为空时存成所有仓库的默认值）。存成默认值时
/// 不动各仓库自己的。读不懂原来的内容时不写，免得把手改的文件冲掉。
pub(crate) fn save_recipe(root: Option<&Path>, recipe: Recipe) -> Result<(), String> {
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
pub(crate) struct PromptContext {
    pub(crate) staged: bool,
    pub(crate) branch: String,
    /// 一行一个改动的文件：状态字母和路径。
    pub(crate) files: String,
}

pub(crate) fn generate(
    repo: &ProjectRepo,
    recipe: &Recipe,
    context: &PromptContext,
    path: Option<OsString>,
) -> Result<String, String> {
    // 先看命令写得对不对，再去读改动。
    let command = Invocation::of(recipe)?;
    let diff = repo.pending_diff(context.staged).map_err(|err| git_error_text(&err))?;
    let recent = repo.recent_messages(EXAMPLES).map_err(|err| git_error_text(&err))?;
    let render = |patch: &str| {
        let base = base_prompt(context, &recent, patch);
        render_template(
            &recipe.template,
            &[
                ("basePrompt", &base),
                ("branch", &context.branch),
                ("stagedFiles", &context.files),
                ("stagedPatch", patch),
            ],
        )
    };
    let on_argv = command.args("").1.is_none();
    let max_patch = if on_argv { argv_patch_budget(&render) } else { MAX_PATCH_BYTES };
    let prompt = render(&limit(diff, max_patch));
    if on_argv && prompt.len() > MAX_ARGV_PROMPT_BYTES {
        return Err(rust_i18n::t!("git.ai_message.prompt_too_long", binary = command.binary).into_owned());
    }
    let (args, stdin) = command.args(&prompt);
    // 第一次问登录 shell 会卡住，先问好再占着 `ScratchDir`，别让同时排着的生成跟着等。
    let path = crate::process_env::user_path(path);
    // 内置的 agent 在空目录里跑：陌生仓库里的项目配置（`.claude/settings.json` 的钩子、
    // `opencode.json`、direnv 的 `.envrc` 这类）会被它读到、执行，提示词里已经有要的全部内容。用户自己
    // 写的命令可能就要在仓库里跑（比如自己调 git），照旧在仓库里。
    let scratch = Dirs::from_env()
        .ai_message_dir()
        .ok_or_else(|| "no home directory".to_owned())
        .and_then(|dir| ScratchDir::new(dir).map_err(|err| err.to_string()))?;
    // 远端的仓库目录这台机器上没有，命令同样在空目录里跑。
    let dir = if command.agent.is_some() || repo.is_remote() { &scratch.dir } else { &repo.root };
    let program = program_file(&command.binary, path.as_deref(), &command.env);
    let stdout = oneshot::run(program, &args, stdin, dir, path, &command.env, TIMEOUT)
        .map_err(|err| run_error_text(&command.binary, err))?;
    drop(scratch);
    let message = clean(&stdout);
    if message.is_empty() {
        return Err(rust_i18n::t!("git.ai_message.empty").into_owned());
    }
    Ok(message)
}

/// 提示词放在参数里时最多给多少字节的改动，整个提示词才不超过 `MAX_ARGV_PROMPT_BYTES`。`render` 按改动
/// 生成提示词；模板里可以不止一处放改动。
fn argv_patch_budget(render: &impl Fn(&str) -> String) -> usize {
    /// `limit` 截断时在末尾补的说明最多这么长。
    const TRUNCATION_NOTE: usize = 64;
    let fixed = render("").len();
    let copies = render("\n").len() - fixed;
    if copies == 0 {
        return MAX_PATCH_BYTES;
    }
    let room = MAX_ARGV_PROMPT_BYTES.saturating_sub(fixed) / copies;
    room.saturating_sub(TRUNCATION_NOTE).min(MAX_PATCH_BYTES)
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
        let agent = agents::agent(&recipe.agent).ok_or_else(|| {
            match agents::REMOVED.iter().find(|(id, _)| *id == recipe.agent) {
                Some((_, label)) => rust_i18n::t!("git.ai_message.removed_agent", agent = label).into_owned(),
                None => rust_i18n::t!("git.ai_message.unknown_agent", agent = recipe.agent).into_owned(),
            }
        })?;
        let words = split_args(&recipe.args)?;
        Ok(Self { agent: Some(agent), env: Vec::new(), binary: agent.cli.binary.to_owned(), words })
    }

    /// 完整的参数和要经标准输入给的提示词：参数里放了提示词时后者为空。
    fn args(&self, prompt: &str) -> (Vec<String>, Option<String>) {
        match self.agent {
            Some(agent) => agent.cli.command_line(self.words.clone(), prompt),
            None => oneshot::with_prompt(self.words.clone(), prompt),
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
pub(crate) fn command_preview(recipe: &Recipe) -> Result<(String, bool), String> {
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
pub(crate) fn limit(mut text: String, max: usize) -> String {
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

/// 起 `binary` 时交给 `oneshot::run` 的程序。Windows 上不带目录的程序名照最后起作用的 PATH 找：`env` 里写的
/// `PATH`（名字不分大小写）压过 `path`，都没有时用本进程的；npm 装的 agent 是 `.cmd`，`Command` 自己只补
/// `.exe`。别的系统上，和找不到时，原样交出去，由拉起时报错。
fn program_file(binary: &str, path: Option<&OsStr>, env: &[(String, String)]) -> OsString {
    if !cfg!(windows) || binary.contains(['/', '\\']) {
        return binary.into();
    }
    let env_path =
        env.iter().rev().find(|(name, _)| name.eq_ignore_ascii_case("PATH")).map(|(_, value)| OsStr::new(value));
    let inherited = std::env::var_os("PATH");
    let dirs: Vec<_> = env_path
        .or(path)
        .or(inherited.as_deref())
        .map(|path| std::env::split_paths(path).filter(|dir| dir.is_absolute()).collect())
        .unwrap_or_default();
    runode_terminal::shells::find_command(&dirs, binary).map_or_else(|| binary.into(), PathBuf::into_os_string)
}

/// 跑 `binary` 没拿到输出时给用户看的原因。
fn run_error_text(binary: &str, err: RunError) -> String {
    match err {
        RunError::Missing => rust_i18n::t!("git.ai_message.missing", binary = binary).into_owned(),
        RunError::Timeout => rust_i18n::t!("git.ai_message.timeout", binary = binary).into_owned(),
        RunError::Failed(detail) => format!("{binary}: {detail}"),
        RunError::Io(err) => format!("{binary}: {err}"),
    }
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

    /// 提示词放在参数里时，模板里每放一份改动，能给的改动就少一份，整个提示词不超过上限。
    #[test]
    fn fits_the_patch_into_the_argv_budget() {
        let fixed = "x".repeat(1_000);
        let once = |patch: &str| format!("{fixed}{patch}");
        let twice = |patch: &str| format!("{fixed}{patch}{patch}");
        let budget = argv_patch_budget(&once);
        assert!(budget <= MAX_PATCH_BYTES);
        let patch = "+line\n".repeat(MAX_PATCH_BYTES);
        assert!(once(&limit(patch.clone(), budget)).len() <= MAX_ARGV_PROMPT_BYTES);
        assert!(twice(&limit(patch, argv_patch_budget(&twice))).len() <= MAX_ARGV_PROMPT_BYTES);
        assert_eq!(argv_patch_budget(&|_: &str| fixed.clone()), MAX_PATCH_BYTES);
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
    fn explains_why_a_removed_agent_is_gone() {
        let removed = Recipe { agent: "kimi".into(), ..Recipe::default() };
        let err = command_preview(&removed).unwrap_err();
        assert!(err.starts_with("Kimi "), "{err}");
        let unknown = Recipe { agent: "nobody".into(), ..Recipe::default() };
        assert!(command_preview(&unknown).unwrap_err().contains("nobody"));
        for (id, _) in agents::REMOVED {
            assert!(agents::agent(id).is_none(), "{id} is still built in");
        }
    }

    #[test]
    fn previews_the_whole_command() {
        let recipe = Recipe { args: "--model 'my model'".into(), ..Recipe::default() };
        let (line, stdin) = command_preview(&recipe).unwrap();
        assert_eq!(
            line,
            "claude -p --output-format text --permission-mode plan --tools '' --strict-mcp-config --no-session-persistence --model 'my model'"
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
