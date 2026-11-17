//! 新建任务时「模板」菜单里的模板：内置的几个（修 issue、处理 PR、评审 PR，和巡检 CI、评审新 PR、处理新 issue 这几个
//! 定时），和用户放进 `Dirs::todo_templates_dir` 的 `*.md`。一个模板是一段 Markdown，正文是提示词，开头 `---` 两行之间
//! 写几行 `键: 值`：
//!
//! - `name`：列表里显示的名字，没写时用文件名；
//! - `input`：要用户另外填的一项的说明（比如 `Issue 号或链接`），标题、提示词和 `run` 里的 `{{input}}` 保存时换成填的值；
//!   `input_default` 是那一项先填好的值；
//! - `title`：标题，同样可以写 `{{input}}`；
//! - `auto`、`worktree`、`auto_done`：写 `true` 时免确认一路跑完、在独立的工作树里做、agent 做完自动标完成；
//! - `time_limit`：时限，分钟数；
//! - `skills`：要用的 skill，逗号隔开。
//!
//! 写了 `daily`（`HH:MM`，可以再写 `days`，`mon-fri`、`mon,wed` 这样）、`every`（分钟数）、`cron`（五段的表达式）
//! 三者之一的是定时模板，只能写一个，写了几个时按这个次序取第一个。定时模板还能写到点先跑的命令 `run` 和它的超时
//! `run_timeout`（分钟数）；`run` 里的 `{{script:<名字>}}` 换成调用内置脚本（`runode_todo::run_scripts::RUN_SCRIPTS`）
//! 的命令，`{{input}}` 换成填的值时按 shell 的规矩加引号（`fill_run`）。值以 `{{` 开头时要加引号。
//!
//! skill 和自定义 agent 也照这样读开头的 `name:`（`front_matter`）。

use std::path::{Path, PathBuf};

/// 填进 `input` 的值在标题、提示词里的写法。
const INPUT: &str = "{{input}}";

/// `run` 里调用内置脚本的写法：`{{script:` 加脚本名加 `}}`。
const SCRIPT_OPEN: &str = "{{script:";
const SCRIPT_CLOSE: &str = "}}";

/// 内置模板：名字和 `input` 的说明在 locales 的 `kanban.template.<id>`、`kanban.template.<id>_input` 里。
const BUILTIN: &[(&str, &str)] = &[
    ("issue", include_str!("templates/issue.md")),
    ("pr_fix", include_str!("templates/pr_fix.md")),
    ("pr_review", include_str!("templates/pr_review.md")),
    ("watch_ci", include_str!("templates/watch_ci.md")),
    ("review_requests", include_str!("templates/review_requests.md")),
    ("new_issues", include_str!("templates/new_issues.md")),
];

/// 定时模板什么时候跑，存原文，由对话框照它自己的规矩查。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Repeat {
    /// `days` 是 `When::Daily` 的 `days`，0 是每天。
    Daily {
        clock: String,
        days: u8,
    },
    Every {
        minutes: String,
    },
    Cron {
        expr: String,
    },
}

/// 模板写得不对的地方：照能读懂的那部分用，挑它时告诉用户。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Problem {
    /// `daily`、`every`、`cron` 写了不止一个。
    ManyRepeats,
    /// 读不懂的 `days`，当作每天。
    Days(String),
    /// `run` 里的 `{{script:…}}` 没有这个脚本（或者它的路径没法安全地写进命令），`run` 照原文留着。
    UnknownScript(String),
}

impl Problem {
    /// 给人看的一句说明。
    pub(crate) fn message(&self) -> String {
        match self {
            Self::ManyRepeats => rust_i18n::t!("kanban.dialog.template_many_repeats").into_owned(),
            Self::Days(days) => rust_i18n::t!("kanban.dialog.template_bad_days", days = days).into_owned(),
            Self::UnknownScript(name) => {
                rust_i18n::t!("kanban.dialog.template_unknown_script", name = name).into_owned()
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Template {
    /// 内置的是 `BUILTIN` 里的 id，用户的是 `user:` 加文件名，元素 id 按它取。
    pub(crate) id: String,
    pub(crate) name: String,
    /// 要用户另外填的那一项的说明；为空时不用填。
    pub(crate) input: Option<String>,
    /// 要另外填的那一项先填好的值。
    pub(crate) input_default: String,
    pub(crate) title: String,
    pub(crate) prompt: String,
    pub(crate) auto: bool,
    pub(crate) auto_done: bool,
    pub(crate) worktree: bool,
    pub(crate) skills: Vec<String>,
    /// 时限的分钟数原文，空着是不限。
    pub(crate) time_limit: String,
    /// 有它就是定时模板。
    pub(crate) repeat: Option<Repeat>,
    /// 到点先跑的命令；`load` 读出的已经换好 `{{script:…}}`，`{{input}}` 留到保存时换。不是定时模板时为空。
    pub(crate) run: String,
    /// 先跑的命令的超时，分钟数原文。不是定时模板时为空。
    pub(crate) run_timeout: String,
    pub(crate) problem: Option<Problem>,
}

impl Template {
    /// 照 `text` 的写法读出模板；`run` 还是原文。
    fn parse(id: String, name: &str, text: &str) -> Self {
        let (fields, body) = front_matter(text);
        let field = |key: &str| fields.iter().find(|(k, _)| *k == key).map(|(_, value)| *value);
        let flag = |key: &str| field(key).is_some_and(|value| value.eq_ignore_ascii_case("true"));
        let text = |key: &str| field(key).unwrap_or_default().to_owned();
        let mut repeats = ["daily", "every", "cron"].into_iter().filter_map(|key| Some((key, field(key)?)));
        let first = repeats.next();
        let mut problem = repeats.next().map(|_| Problem::ManyRepeats);
        let repeat = first.map(|(key, value)| match key {
            "daily" => {
                let days = match field("days").map(str::trim).filter(|days| !days.is_empty()) {
                    None => 0,
                    Some(days) => runode_todo::parse_days(days).unwrap_or_else(|| {
                        problem.get_or_insert_with(|| Problem::Days(days.to_owned()));
                        0
                    }),
                };
                Repeat::Daily { clock: value.to_owned(), days }
            }
            "every" => Repeat::Every { minutes: value.to_owned() },
            _ => Repeat::Cron { expr: value.to_owned() },
        });
        let (run, run_timeout) =
            if repeat.is_some() { (text("run"), text("run_timeout")) } else { (String::new(), String::new()) };
        Self {
            id,
            name: field("name").filter(|name| !name.is_empty()).unwrap_or(name).to_owned(),
            input: field("input").filter(|input| !input.is_empty()).map(str::to_owned),
            input_default: text("input_default"),
            title: text("title"),
            prompt: body.trim().to_owned(),
            auto: flag("auto"),
            auto_done: flag("auto_done"),
            worktree: flag("worktree"),
            skills: field("skills")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|skill| !skill.is_empty())
                .map(str::to_owned)
                .collect(),
            time_limit: text("time_limit"),
            repeat,
            run,
            run_timeout,
            problem,
        }
    }

    /// 标题、提示词或 `run` 里有 `{{input}}`。
    fn wants_input(&self) -> bool {
        [&self.title, &self.prompt, &self.run].iter().any(|text| text.contains(INPUT))
    }

    /// `run` 里的 `{{script:…}}` 换成调用 `scripts` 下脚本的命令（`expand_run`）；换不了时照原文留着、记下问题。
    fn expand_run(&mut self, scripts: Option<&Path>, home: Option<&Path>) {
        match expand_run(&self.run, scripts, home) {
            Ok(run) => self.run = run,
            Err(name) => self.problem = Some(Problem::UnknownScript(name)),
        }
    }
}

/// `text` 里的 `{{input}}` 换成 `input`。
pub(crate) fn fill(text: &str, input: &str) -> String {
    text.replace(INPUT, input)
}

/// `run` 里的 `{{input}}` 换成 `input` 当一个参数的写法：Windows（`windows`）以外按 sh 的规矩用单引号括起，Windows 上
/// 用双引号括起，那里 `input` 带 `"`、`%`（cmd 会展开环境变量）或以 `\` 结尾（会和收尾的引号连成 `\"`）时没法
/// 原样传过去，为 `Err`。没有 `{{input}}` 时原样。
pub(crate) fn fill_run(run: &str, input: &str, windows: bool) -> Result<String, ()> {
    if !run.contains(INPUT) {
        return Ok(run.to_owned());
    }
    let quoted = if windows {
        if input.contains(['"', '%']) || input.ends_with('\\') {
            return Err(());
        }
        format!("\"{input}\"")
    } else {
        sh_quote(input)
    };
    Ok(run.replace(INPUT, &quoted))
}

/// `text` 用单引号括起，当 sh 的一个词；里面的 `'` 写成 `'\''`。
fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// `text` 里第一个 `{{script:NAME}}`：它的起止位置和 `NAME`。
fn next_script(text: &str) -> Option<(usize, usize, &str)> {
    let start = text.find(SCRIPT_OPEN)?;
    let rest = &text[start + SCRIPT_OPEN.len()..];
    let len = rest.find(SCRIPT_CLOSE)?;
    Some((start, start + SCRIPT_OPEN.len() + len + SCRIPT_CLOSE.len(), rest[..len].trim()))
}

/// 本机的 `run` 里的 `{{script:…}}` 换成调用 `dir` 下脚本的命令（`expand_scripts`）；`dir` 为空（没有家目录）时有
/// `{{script:…}}` 就换不了。
pub(crate) fn expand_run(run: &str, dir: Option<&Path>, home: Option<&Path>) -> Result<String, String> {
    match dir {
        Some(dir) => expand_scripts(run, dir, home, cfg!(windows)),
        None => next_script(run).map_or_else(|| Ok(run.to_owned()), |(_, _, name)| Err(name.to_owned())),
    }
}

/// `run` 里的每个 `{{script:NAME}}` 换成调用 `dir` 下内置脚本 `NAME` 的命令：Windows（`windows`）以外是 `sh` 加
/// 脚本路径，路径在家目录 `home` 下、又只有字母数字和 `/._-` 时写成 `~/…`，不然单引号括起绝对路径；Windows 上是
/// `powershell -NoProfile -ExecutionPolicy Bypass -File "<路径>"`。`NAME` 不是内置脚本、或者 Windows 上路径带 `"`、
/// `%` 时为 `Err`，带着这个 `NAME`。
pub(crate) fn expand_scripts(run: &str, dir: &Path, home: Option<&Path>, windows: bool) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = run;
    while let Some((start, end, name)) = next_script(rest) {
        if !runode_todo::run_scripts::RUN_SCRIPTS.iter().any(|script| script.name == name) {
            return Err(name.to_owned());
        }
        let path = dir.join(format!("{name}.{}", if windows { "ps1" } else { "sh" }));
        let command = script_command(&path, home, windows).ok_or_else(|| name.to_owned())?;
        out.push_str(&rest[..start]);
        out.push_str(&command);
        rest = &rest[end..];
    }
    out.push_str(rest);
    Ok(out)
}

/// 跑脚本 `path` 的一条命令，带上解释器；路径没法安全地写进命令时为空。
fn script_command(path: &Path, home: Option<&Path>, windows: bool) -> Option<String> {
    let full = path.to_str()?;
    if windows {
        return (!full.contains(['"', '%']))
            .then(|| format!("powershell -NoProfile -ExecutionPolicy Bypass -File \"{full}\""));
    }
    let plain = |text: &str| text.chars().all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c));
    let short = home
        .and_then(|home| runode_paths::home_relative(path, home, '/'))
        .filter(|short| short.strip_prefix("~/").is_some_and(|rest| !rest.is_empty() && plain(rest)));
    Some(format!("sh {}", short.unwrap_or_else(|| sh_quote(full))))
}

/// 内置的模板，再是 `dir` 下的 `*.md`（按文件名排）。读不了的文件跳过。`run` 里的 `{{script:…}}` 换成调用 `scripts`
/// 下脚本的命令，`home` 是家目录。会读文件，放到后台调。
pub(crate) fn load(dir: Option<&Path>, scripts: Option<&Path>, home: Option<&Path>) -> Vec<Template> {
    let mut templates: Vec<_> = BUILTIN
        .iter()
        .map(|(id, text)| {
            let mut template = Template::parse((*id).to_owned(), "", text);
            let (name, input) = (format!("kanban.template.{id}"), format!("kanban.template.{id}_input"));
            template.name = rust_i18n::t!(&name).into_owned();
            template.input = template.wants_input().then(|| rust_i18n::t!(&input).into_owned());
            template
        })
        .collect();
    let mut files: Vec<PathBuf> = dir
        .and_then(|dir| std::fs::read_dir(dir).ok())
        .into_iter()
        .flatten()
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .collect();
    files.sort();
    templates.extend(files.iter().filter_map(|path| {
        let stem = path.file_stem()?.to_string_lossy();
        let text = std::fs::read_to_string(path).ok()?;
        Some(Template::parse(format!("user:{stem}"), &stem, &text))
    }));
    for template in &mut templates {
        template.expand_run(scripts, home);
    }
    templates
}

/// `text` 开头 `---` 两行之间顶格写的 `键: 值`（值两边的引号去掉），和后面的正文；开头不是 `---` 时全是正文。
// shortcut: 只认一行一个的 `键: 值`，不是完整的 YAML；要写列表、多行的值时再换成真的 YAML 解析。
pub(crate) fn front_matter(text: &str) -> (Vec<(&str, &str)>, &str) {
    let Some(rest) = text.strip_prefix("---").filter(|rest| rest.starts_with(['\n', '\r'])) else {
        return (Vec::new(), text);
    };
    let Some((head, body)) = rest.split_once("\n---") else { return (Vec::new(), text) };
    let body = body.split_once('\n').map_or("", |(_, body)| body);
    let fields = head
        .lines()
        // 缩进的是上一项写成多行的值（`description: >`），不是键。
        .filter(|line| !line.starts_with(char::is_whitespace))
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| {
            let value = value.trim();
            let unquoted = ['"', '\''].iter().find_map(|quote| value.strip_prefix(*quote)?.strip_suffix(*quote));
            // 没加引号的值里 ` #` 往后是注释。
            (
                key.trim(),
                unquoted.unwrap_or_else(|| value.split_once(" #").map_or(value, |(value, _)| value.trim_end())),
            )
        })
        .collect();
    (fields, body)
}

/// 把定时先跑命令用的内置脚本（`runode_todo::run_scripts::RUN_SCRIPTS`）本机那份写进 `dir`，总是换成
/// 这一版编进去的；内容一样的不重写。整个换上（`runode_paths::replace_file`），定时正好在跑脚本时读到的
/// 不会是写了一半的。返回写了几个。
pub(crate) fn install_scripts(dir: &Path) -> std::io::Result<usize> {
    let mut written = 0;
    for script in runode_todo::run_scripts::RUN_SCRIPTS {
        let (name, contents) = script.for_this_os();
        let path = dir.join(name);
        if std::fs::read_to_string(&path).is_ok_and(|existing| existing == contents) {
            continue;
        }
        std::fs::create_dir_all(dir)?;
        runode_paths::replace_file(&path, contents.as_bytes())?;
        written += 1;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_scripts_are_written_once() {
        let dir = std::env::temp_dir().join(format!("rdr-schedule-scripts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let scripts = runode_todo::run_scripts::RUN_SCRIPTS;
        assert_eq!(install_scripts(&dir).unwrap(), scripts.len());
        for script in scripts {
            let (name, contents) = script.for_this_os();
            assert_eq!(std::fs::read_to_string(dir.join(name)).unwrap(), contents);
        }
        assert_eq!(install_scripts(&dir).unwrap(), 0);
        // 被改过的换回自带的那份。
        let (name, contents) = scripts[0].for_this_os();
        std::fs::write(dir.join(&name), "edited").unwrap();
        assert_eq!(install_scripts(&dir).unwrap(), 1);
        assert_eq!(std::fs::read_to_string(dir.join(name)).unwrap(), contents);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reads_the_front_matter_and_the_prompt() {
        let text = "---\nname: \"Ship it\"\ninput: PR\ntitle: Ship {{input}}\nauto: true\nskills: review, , tdd\n---\nShip {{input}}.\n";
        let template = Template::parse("user:ship".into(), "ship", text);
        assert_eq!(
            template,
            Template {
                id: "user:ship".into(),
                name: "Ship it".into(),
                input: Some("PR".into()),
                title: "Ship {{input}}".into(),
                prompt: "Ship {{input}}.".into(),
                auto: true,
                skills: vec!["review".into(), "tdd".into()],
                ..Template::default()
            }
        );
        assert_eq!(fill(&template.prompt, "#12"), "Ship #12.");
        // 没有开头那段时全是正文，名字用文件名。
        let plain = Template::parse("user:plain".into(), "plain", "Just do it\n---\nmore");
        assert_eq!((plain.name.as_str(), plain.prompt.as_str()), ("plain", "Just do it\n---\nmore"));
        assert_eq!(front_matter("---\n---\nbody"), (Vec::new(), "body"));
        assert_eq!(
            front_matter("---\nname: review # code review\nnote: \"a # b\"\n---\n").0,
            [("name", "review"), ("note", "a # b")]
        );
    }

    #[test]
    fn reads_the_schedule_fields() {
        let text = "---\ndaily: 09:30\ndays: mon-fri\nrun: \"{{script:watch-ci}} main\"\nrun_timeout: 5\n\
                    time_limit: 45\nauto_done: true\ninput: Label\ninput_default: agent\n---\nGo.\n";
        let template = Template::parse("user:ci".into(), "ci", text);
        assert_eq!(template.repeat, Some(Repeat::Daily { clock: "09:30".into(), days: 0b011_1110 }));
        assert_eq!(
            (template.run.as_str(), template.run_timeout.as_str(), template.time_limit.as_str()),
            ("{{script:watch-ci}} main", "5", "45")
        );
        assert!(template.auto_done && !template.auto);
        assert_eq!((template.input_default.as_str(), template.problem), ("agent", None));
        let every = Template::parse("user:e".into(), "e", "---\nevery: 15\n---\n");
        assert_eq!(every.repeat, Some(Repeat::Every { minutes: "15".into() }));
        let cron = Template::parse("user:c".into(), "c", "---\ncron: \"0 9 * * 1-5\"\n---\n");
        assert_eq!(cron.repeat, Some(Repeat::Cron { expr: "0 9 * * 1-5".into() }));
        // 不是定时模板时不用 `run`，时限照用。
        let task = Template::parse("user:t".into(), "t", "---\nrun: make\nrun_timeout: 5\ntime_limit: 30\n---\n");
        assert_eq!((task.repeat, task.run.as_str(), task.run_timeout.as_str()), (None, "", ""));
        assert_eq!(task.time_limit, "30");
    }

    #[test]
    fn schedule_template_problems() {
        // 写了几个时按 daily、every、cron 的次序取第一个。
        let many = Template::parse("user:m".into(), "m", "---\ncron: 0 9 * * *\nevery: 15\n---\n");
        assert_eq!(many.repeat, Some(Repeat::Every { minutes: "15".into() }));
        assert_eq!(many.problem, Some(Problem::ManyRepeats));
        let days = Template::parse("user:d".into(), "d", "---\ndaily: 09:00\ndays: weekdays\n---\n");
        assert_eq!(days.repeat, Some(Repeat::Daily { clock: "09:00".into(), days: 0 }));
        assert_eq!(days.problem, Some(Problem::Days("weekdays".into())));
        let mut unknown = Template::parse("user:u".into(), "u", "---\nevery: 5\nrun: \"{{script:nope}}\"\n---\n");
        unknown.expand_run(Some(Path::new("/tmp/scripts")), None);
        assert_eq!(
            (unknown.run.as_str(), unknown.problem),
            ("{{script:nope}}", Some(Problem::UnknownScript("nope".into())))
        );
    }

    // 断言里的路径用 `/` 拼，Windows 上 `Path::join` 拼出的是 `\`。
    #[cfg(unix)]
    #[test]
    fn expands_scripts_for_sh() {
        let home = Path::new("/Users/me");
        let dir = home.join(".runode/schedule-scripts");
        assert_eq!(
            expand_scripts("{{script:watch-ci}} main", &dir, Some(home), false).unwrap(),
            "sh ~/.runode/schedule-scripts/watch-ci.sh main"
        );
        // 不在家目录下、或者路径里有别的字符时单引号括起绝对路径。
        assert_eq!(
            expand_scripts("x && {{script:new-issues}} 'a'", Path::new("/opt/s"), Some(home), false).unwrap(),
            "x && sh '/opt/s/new-issues.sh' 'a'"
        );
        let odd = home.join("it's here");
        assert_eq!(
            expand_scripts("{{script:review-requests}}", &odd, Some(home), false).unwrap(),
            "sh '/Users/me/it'\\''s here/review-requests.sh'"
        );
        assert_eq!(expand_scripts("echo {{input}}", &dir, None, false).unwrap(), "echo {{input}}");
        assert_eq!(expand_scripts("{{script:nope}}", &dir, Some(home), false), Err("nope".into()));
        assert_eq!(expand_run("{{script:watch-ci}}", None, None), Err("watch-ci".into()));
        assert_eq!(expand_run("make check", None, None).unwrap(), "make check");
    }

    #[test]
    fn expands_scripts_for_powershell() {
        let dir = Path::new("/home/me/.runode/schedule-scripts");
        let path = dir.join("new-issues.ps1");
        assert_eq!(
            expand_scripts("{{script:new-issues}} \"bug\"", dir, Some(Path::new("/home/me")), true).unwrap(),
            format!("powershell -NoProfile -ExecutionPolicy Bypass -File \"{}\" \"bug\"", path.display())
        );
        assert_eq!(expand_scripts("{{script:watch-ci}}", Path::new("/a\"b"), None, true), Err("watch-ci".into()));
        assert_eq!(expand_scripts("{{script:watch-ci}}", Path::new("/%TEMP%"), None, true), Err("watch-ci".into()));
    }

    #[test]
    fn fills_the_input_into_run() {
        assert_eq!(fill_run("sh x.sh {{input}}", "it's", false).unwrap(), "sh x.sh 'it'\\''s'");
        assert_eq!(fill_run("x.ps1 {{input}}", "good first", true).unwrap(), "x.ps1 \"good first\"");
        assert_eq!(fill_run("x.ps1 {{input}}", "a\"b", true), Err(()));
        assert_eq!(fill_run("x.ps1 {{input}}", "%PATH%", true), Err(()));
        assert_eq!(fill_run("x.ps1 {{input}}", "dir\\", true), Err(()));
        assert_eq!(fill_run("make", "%PATH%", true).unwrap(), "make");
    }

    #[test]
    fn builtin_templates_ask_for_their_input() {
        for (id, text) in BUILTIN {
            let template = Template::parse((*id).to_owned(), id, text);
            assert!(template.auto, "{id}");
            if template.repeat.is_none() {
                assert!(template.prompt.contains(INPUT) && template.title.contains(INPUT), "{id}");
            }
        }
        let home = Path::new("/Users/me");
        let scripts = home.join(".runode/schedule-scripts");
        let templates = load(None, Some(&scripts), Some(home));
        let input = |id: &str| templates.iter().find(|template| template.id == id).unwrap().input.is_some();
        assert!(["issue", "pr_fix", "pr_review", "new_issues"].into_iter().all(input));
        assert!(!["watch_ci", "review_requests"].into_iter().any(input));
    }

    #[test]
    fn builtin_schedules_use_builtin_scripts() {
        let home = Path::new("/Users/me");
        let scripts = home.join(".runode/schedule-scripts");
        for (id, text) in BUILTIN {
            let template = Template::parse((*id).to_owned(), id, text);
            if template.repeat.is_none() {
                continue;
            }
            assert_eq!(template.problem, None, "{id}");
            let (_, _, name) = next_script(&template.run).unwrap_or_else(|| panic!("{id} runs no script"));
            assert!(runode_todo::run_scripts::RUN_SCRIPTS.iter().any(|script| script.name == name), "{id}");
            for windows in [false, true] {
                let run = expand_scripts(&template.run, &scripts, Some(home), windows).unwrap();
                assert!(!run.contains(SCRIPT_OPEN), "{id}");
            }
        }
        let new_issues = load(None, Some(&scripts), Some(home)).into_iter().find(|t| t.id == "new_issues").unwrap();
        assert_eq!(new_issues.input_default, "agent");
        if cfg!(unix) {
            assert_eq!(new_issues.run, "sh ~/.runode/schedule-scripts/new-issues.sh {{input}}");
        }
    }
}
