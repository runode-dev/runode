//! 新建任务那一页左边的模板：内置的几个（修 issue、处理 PR、评审 PR），和用户放进 `Dirs::todo_templates_dir`
//! 的 `*.md`。一个模板是一段 Markdown，正文是提示词，开头 `---` 两行之间写几行 `键: 值`：
//!
//! - `name`：列表里显示的名字，没写时用文件名；
//! - `input`：要用户另外填的一项的说明（比如 `Issue 号或链接`），标题和提示词里的 `{{input}}` 保存时换成填的值；
//! - `title`：标题，同样可以写 `{{input}}`；
//! - `auto`、`worktree`：写 `true` 时免确认一路跑完、在独立的工作树里做；
//! - `skills`：要用的 skill，逗号隔开。
//!
//! skill 和自定义 agent 也照这样读开头的 `name:`（`front_matter`）。

use std::path::{Path, PathBuf};

/// 填进 `input` 的值在标题、提示词里的写法。
const INPUT: &str = "{{input}}";

/// 内置模板：名字和 `input` 的说明在 locales 的 `kanban.template.<id>`、`kanban.template.<id>_input` 里。
const BUILTIN: &[(&str, &str)] = &[
    ("issue", include_str!("templates/issue.md")),
    ("pr_fix", include_str!("templates/pr_fix.md")),
    ("pr_review", include_str!("templates/pr_review.md")),
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Template {
    /// 内置的是 `BUILTIN` 里的 id，用户的是 `user:` 加文件名，元素 id 按它取。
    pub(crate) id: String,
    pub(crate) name: String,
    /// 要用户另外填的那一项的说明；为空时不用填。
    pub(crate) input: Option<String>,
    pub(crate) title: String,
    pub(crate) prompt: String,
    pub(crate) auto: bool,
    pub(crate) worktree: bool,
    pub(crate) skills: Vec<String>,
}

impl Template {
    /// 照 `text` 的写法读出模板，见模块说明。
    fn parse(id: String, name: &str, text: &str) -> Self {
        let (fields, body) = front_matter(text);
        let field = |key: &str| fields.iter().find(|(k, _)| *k == key).map(|(_, value)| *value);
        let flag = |key: &str| field(key).is_some_and(|value| value.eq_ignore_ascii_case("true"));
        Self {
            id,
            name: field("name").filter(|name| !name.is_empty()).unwrap_or(name).to_owned(),
            input: field("input").filter(|input| !input.is_empty()).map(str::to_owned),
            title: field("title").unwrap_or_default().to_owned(),
            prompt: body.trim().to_owned(),
            auto: flag("auto"),
            worktree: flag("worktree"),
            skills: field("skills")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|skill| !skill.is_empty())
                .map(str::to_owned)
                .collect(),
        }
    }
}

/// `text` 里的 `{{input}}` 换成 `input`。
pub(crate) fn fill(text: &str, input: &str) -> String {
    text.replace(INPUT, input)
}

/// 内置的模板，再是 `dir` 下的 `*.md`（按文件名排）。读不了的文件跳过。会读文件，放到后台调。
pub(crate) fn load(dir: Option<&Path>) -> Vec<Template> {
    let mut templates: Vec<_> = BUILTIN
        .iter()
        .map(|(id, text)| {
            let mut template = Template::parse((*id).to_owned(), "", text);
            let (name, input) = (format!("kanban.template.{id}"), format!("kanban.template.{id}_input"));
            template.name = rust_i18n::t!(&name).into_owned();
            template.input = Some(rust_i18n::t!(&input).into_owned());
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

#[cfg(test)]
mod tests {
    use super::*;

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
                worktree: false,
                skills: vec!["review".into(), "tdd".into()],
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
    fn builtin_templates_ask_for_their_input() {
        for (id, text) in BUILTIN {
            let template = Template::parse((*id).to_owned(), id, text);
            assert!(template.prompt.contains(INPUT) && template.title.contains(INPUT), "{id}");
            assert!(template.auto, "{id}");
        }
    }
}
