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
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use gpui::{Context, Window};
use runode_git::{self as git, GitError, Section};
use runode_paths::Dirs;
use serde::{Deserialize, Serialize};

use super::{rows::Busy, run::show_git_error};
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

/// 默认的提示词模板：就是内置的提示词。
pub(in crate::window) const DEFAULT_TEMPLATE: &str = "{basePrompt}";
/// 模板里能用的变量，对话框里列给用户看。
pub(in crate::window) const VARIABLES: &[&str] = &["{basePrompt}", "{branch}", "{stagedFiles}", "{stagedPatch}"];

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
    /// 自己定义的命令：按 shell 的规矩分词，第一个词是程序，参数里的 `{prompt}` 换成提示词；没有
    /// `{prompt}` 时提示词经标准输入给。和选哪个 agent 分开存，换来换去不丢。
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
    fs::create_dir_all(&dir).and_then(|()| fs::write(&file, text)).map_err(|err| format!("{}: {err}", file.display()))
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

    /// 按 `recipe` 给根目录是 `root` 的仓库要提交的改动写提交说明，写好了替换提交说明框里的字。
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
        // 从访达打开的 app 自己的 PATH 里没有 agent 装的目录，用终端里 shell 报告的 PATH。
        let path = self.focused_view().and_then(|view| view.read(cx).meta().shell_path.clone());
        let path = path.or_else(|| std::env::var_os("PATH"));
        let target = root.to_path_buf();
        let op =
            move |repo: &git::Repo| generate(repo, &recipe, &context, path).map_err(|message| GitError { message });
        self.run_git(root, Busy::Message, window, cx, op, move |this, message, _, cx| {
            if let Some(area) = this.commit_box(&target) {
                area.update(cx, |area, cx| area.set_text(message, cx));
            }
        });
    }
}

fn generate(
    repo: &git::Repo,
    recipe: &Recipe,
    context: &PromptContext,
    path: Option<OsString>,
) -> Result<String, String> {
    // 先看命令写得对不对，再去读改动。
    let (agent, binary, words) = if recipe.agent == agents::CUSTOM_AGENT {
        let mut words = split_args(&recipe.command)?;
        if words.is_empty() {
            return Err(rust_i18n::t!("git.ai_message.empty_command").into_owned());
        }
        (None, words.remove(0), words)
    } else {
        let agent = agents::agent(&recipe.agent)
            .ok_or_else(|| rust_i18n::t!("git.ai_message.unknown_agent", agent = recipe.agent).into_owned())?;
        (Some(agent), agent.binary.to_owned(), split_args(&recipe.args)?)
    };
    let patch = limit(repo.pending_diff(context.staged).map_err(|err| err.message)?, MAX_PATCH_BYTES);
    let recent = repo.recent_messages(EXAMPLES).map_err(|err| err.message)?;
    let base = base_prompt(context, &recent, &patch);
    let prompt = render_template(
        &recipe.template,
        &[("basePrompt", &base), ("branch", &context.branch), ("stagedFiles", &context.files), ("stagedPatch", &patch)],
    );
    let (args, stdin) = match agent {
        Some(agent) => agent.command_line(words, &prompt),
        None => agents::with_prompt(words, &prompt),
    };
    let stdout = run(&binary, &args, stdin, &repo.root, path)?;
    let text = match agent.map_or(Output::Text, |agent| agent.output) {
        Output::Text => stdout,
        Output::OpenCodeEvents => opencode_text(&stdout)?,
    };
    let message = clean(&text);
    if message.is_empty() {
        return Err(rust_i18n::t!("git.ai_message.empty").into_owned());
    }
    Ok(message)
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

/// 按 shell 的规矩把 CLI 参数分成一个个参数：空白分开，单引号、双引号里的原样，引号外的反斜杠转义
/// 下一个字。不展开变量、通配符和 `~`：不经 shell 跑。
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
                        Some('\\') if ch == '"' => arg.extend(chars.next()),
                        Some(other) => arg.push(other),
                        None => return Err(rust_i18n::t!("git.ai_message.unclosed_quote").into_owned()),
                    }
                }
            }
            '\\' => current.get_or_insert_with(String::new).extend(chars.next()),
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

/// 在 `dir` 里跑 `binary`，`stdin` 不为空时从标准输入交给它，返回标准输出。
fn run(
    binary: &str,
    args: &[String],
    stdin: Option<String>,
    dir: &Path,
    path: Option<OsString>,
) -> Result<String, String> {
    let mut command = Command::new(binary);
    command
        .args(args)
        .current_dir(dir)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(path) = path {
        command.env("PATH", path);
    }
    let mut child = command.spawn().map_err(|err| match err.kind() {
        ErrorKind::NotFound => rust_i18n::t!("git.ai_message.missing", binary = binary).into_owned(),
        _ => format!("{binary}: {err}"),
    })?;
    // 提示词可能比管道的缓冲大，另起线程写，agent 卡住不读时也不至于卡在这里等不到超时。
    if let (Some(mut pipe), Some(stdin)) = (child.stdin.take(), stdin) {
        thread::spawn(move || pipe.write_all(stdin.as_bytes()));
    }
    // 输出可能比管道的缓冲大，也另起线程读，不然 agent 写满了管道停下来，这边又在等它退出。
    let stdout = child.stdout.take().map(|pipe| thread::spawn(move || read_all(pipe)));
    let stderr = child.stderr.take().map(|pipe| thread::spawn(move || read_all(pipe)));
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|err| err.to_string())? {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().ok();
            child.wait().ok();
            return Err(rust_i18n::t!("git.ai_message.timeout", binary = binary).into_owned());
        }
        thread::sleep(Duration::from_millis(100));
    };
    let joined = |reader: Option<thread::JoinHandle<String>>| reader.and_then(|reader| reader.join().ok());
    let (stdout, stderr) = (joined(stdout).unwrap_or_default(), joined(stderr).unwrap_or_default());
    if !status.success() {
        let detail = [stderr.trim(), stdout.trim()].into_iter().find(|text| !text.is_empty());
        return Err(format!("{binary}: {}", detail.map_or_else(|| status.to_string(), str::to_owned)));
    }
    Ok(stdout)
}

fn read_all(mut pipe: impl io::Read) -> String {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes).ok();
    String::from_utf8_lossy(&bytes).into_owned()
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

    #[test]
    fn splits_args_like_a_shell() {
        assert_eq!(split_args("  --model sonnet ").unwrap(), ["--model", "sonnet"]);
        assert_eq!(split_args(r#"-c 'a b' "x \"y\"" c\ d ''"#).unwrap(), ["-c", "a b", "x \"y\"", "c d", ""]);
        assert!(split_args("'open").is_err());
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

    /// 提示词经标准输入给，比管道的缓冲大也不卡；失败时报标准错误。
    #[test]
    fn runs_a_command_with_the_prompt_on_stdin() {
        let dir = std::env::temp_dir();
        let prompt = "x".repeat(200_000);
        let out = run("sh", &["-c".into(), "wc -c".into()], Some(prompt), &dir, None).unwrap();
        assert_eq!(out.trim(), "200000");
        let err = run("sh", &["-c".into(), "echo boom >&2; exit 3".into()], None, &dir, None).unwrap_err();
        assert_eq!(err, "sh: boom");
        assert!(run("runode-no-such-agent", &[], None, &dir, None).unwrap_err().contains("runode-no-such-agent"));
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
