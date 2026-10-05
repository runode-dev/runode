//! 按 Tab 弹出的命令补全：不靠用户 shell 自己的补全配置，按嵌进二进制的命令规格给出子命令、
//! 选项和参数的候选，参数还可以来自文件列表和跑命令生成的结果；命令名本身来自 shell 报告的
//! 别名、函数、内建命令、关键字和 PATH 里的可执行文件。
//!
//! 这里只有和界面无关的部分：切词、判断光标处该补什么、匹配排序，以及接受候选时要发给
//! shell 的按键。终端视图负责什么时候接管 Tab、画菜单、在后台跑生成器。

mod commands;
mod engine;
pub mod generators;
mod line;
mod paths;
mod specs;
pub mod usage;

use std::{collections::HashMap, ffi::OsString, path::Path, sync::Arc};

use warp_command_signatures::{GeneratorResults, PathSuggestionType, Suggestion, TemplateType};

pub use engine::{Candidate, Edit, Kind, common_prefix, decisive, highlight, rank};
use engine::{Plan, Source};
pub use line::cells;
use line::Segment;

/// shell 集成报告的各种名字，补命令名时用。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShellNames {
    pub aliases: Vec<String>,
    /// 别名展开成什么：`(名字, 值)`。
    pub alias_values: Vec<(String, String)>,
    pub functions: Vec<String>,
    pub builtins: Vec<String>,
    pub keywords: Vec<String>,
}

/// 光标所在 shell 的情况：它报告的 PATH 和各种名字，以及 runode 记下的命令历史里各命令的
/// 常用程度。
#[derive(Clone, Debug, Default)]
pub struct Shell {
    /// 没报告过时为 `None`，用 runode 自己的 PATH。
    pub path: Option<OsString>,
    pub names: ShellNames,
    pub usage: Arc<usage::Usage>,
}

/// 对一行输入的一次补全：光标所在的那一段命令，以及光标处要补什么。
pub struct Request {
    text: String,
    cursor: usize,
    segment: Segment,
    plan: Plan,
}

/// 要在后台跑的一个生成器。
pub struct GeneratorJob {
    /// 要跑的 shell 命令，也用来判断输入变了以后结果还能不能用。
    pub command: String,
    pub parse: fn(&str) -> GeneratorResults,
    /// 结果从当前词的第几个字符起替换。
    pub from: usize,
    /// 同一个参数的几个生成器各是第几个，和 `command`、`from` 一起认出同一个生成器。
    pub group: u32,
    /// 参数在规格里的名字，菜单的分组标签上用。
    pub arg: Option<String>,
}

impl Request {
    /// 输入 `text`、光标在字节位置 `cursor` 时，光标处能不能由 runode 补全：光标在命令的
    /// 参数上（不是命令名本身）、这条命令有规格时才能，否则为 `None`。
    pub fn new(text: &str, cursor: usize) -> Option<Self> {
        let segment = line::parse(text, cursor);
        let plan = engine::plan(&segment, cursor, &specs::lookup)?;
        Some(Self { text: text.to_owned(), cursor, segment, plan })
    }

    fn word(&self) -> &line::Word {
        &self.segment.words[self.segment.current]
    }

    /// 当前词在输入里的起点（字节位置）。
    pub fn word_start(&self) -> usize {
        self.word().raw.start
    }

    /// 当前词之前的那部分输入。它变了，说明光标已经离开了这个词，或者输入整个换了。
    pub fn before_word(&self) -> &str {
        &self.text[..self.word_start()]
    }

    /// 从当前词的起点到光标在终端里占几格。
    pub fn cells_before_cursor(&self) -> usize {
        line::cells(&self.text[self.word_start()..self.cursor])
    }

    /// 当前词在光标之前的部分，去掉了引号和转义；候选按它来匹配。
    pub fn typed(&self) -> &str {
        &self.plan.typed
    }

    /// 不用跑命令就能得到的候选：子命令、选项、参数的固定取值，`cwd` 下列出的文件，以及
    /// `shell` 里的命令名。
    pub fn local_candidates(&self, cwd: Option<&Path>, shell: &Shell) -> Vec<Candidate> {
        let mut candidates = self.plan.candidates.clone();
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        for source in &self.plan.sources {
            match source {
                Source::Commands => command_names(shell, &mut candidates),
                Source::CommandPaths { from } => {
                    let Some(cwd) = cwd else {
                        continue;
                    };
                    let typed: String = self.plan.typed.chars().skip(*from).collect();
                    let from = from + paths::dir_chars(&typed);
                    for entry in paths::list(&typed, cwd, home.as_deref(), paths::Filter::Executables) {
                        candidates.push(engine::path(entry.name, entry.is_dir, None, from));
                    }
                }
                _ => {}
            }
            let Source::Template { kind, filter, from, .. } = source else {
                continue;
            };
            let Some(cwd) = cwd else {
                continue;
            };
            let typed: String = self.plan.typed.chars().skip(*from).collect();
            let listed = match kind {
                TemplateType::Folders { .. } => paths::Filter::Folders,
                _ => paths::Filter::All,
            };
            let filter = filter
                .as_ref()
                .and_then(|name| specs::dynamic(&self.plan.command).and_then(|data| data.filters().get(name)));
            let from = from + paths::dir_chars(&typed);
            for entry in paths::list(&typed, cwd, home.as_deref(), listed) {
                let (name, description) = match filter {
                    Some(filter) => {
                        let kind = if entry.is_dir { PathSuggestionType::Folder } else { PathSuggestionType::File };
                        let Some(kept) = filter.filter(Suggestion::new(entry.name), kind) else {
                            continue;
                        };
                        (kept.exact_string, kept.description)
                    }
                    None => (entry.name, None),
                };
                candidates.push(engine::path(name, entry.is_dir, description, from));
            }
        }
        // 命令名和命令自己的子命令按命令历史标上常用程度。
        for candidate in &mut candidates {
            candidate.usage = match candidate.kind {
                Kind::Command | Kind::Alias | Kind::Function | Kind::Builtin | Kind::Keyword => {
                    shell.usage.command(&candidate.value)
                }
                Kind::Subcommand if self.plan.top_level => shell.usage.subcommand(&self.plan.command, &candidate.value),
                _ => 0,
            };
        }
        candidates
    }

    /// 要在后台跑的生成器，按规格里的顺序。输入里有 shell 元字符、不能安全拼进命令的不算，
    /// 见 `generators::command`。
    pub fn generator_jobs(&self) -> Vec<GeneratorJob> {
        let Some(data) = specs::dynamic(&self.plan.command) else {
            return Vec::new();
        };
        let tokens: Vec<&str> = self.plan.tokens.iter().map(String::as_str).collect();
        let trailing_space = self.plan.typed.is_empty();
        self.plan
            .sources
            .iter()
            .filter_map(|source| {
                let Source::Generator { name, from, group, arg } = source else {
                    return None;
                };
                let generator = data.generators().get(name)?;
                Some(GeneratorJob {
                    command: generators::command(generator, &tokens, trailing_space, &self.plan.env)?,
                    parse: generator.on_complete_callback,
                    from: *from,
                    group: *group,
                    arg: arg.clone(),
                })
            })
            .collect()
    }

    /// 接受 `candidate` 时要发的按键：只改光标前的部分，见 `engine::edit`。
    pub fn accept(&self, candidate: &Candidate) -> Edit {
        engine::edit(&self.text, self.cursor, self.word(), candidate.from, &candidate.value, candidate.finish)
    }

    /// 只把当前词从第 `from` 个字符起换成 `prefix`（候选的公共开头），这个词还没写完。
    pub fn insert_prefix(&self, from: usize, prefix: &str) -> Edit {
        engine::edit(&self.text, self.cursor, self.word(), from, prefix, false)
    }
}

/// 命令名候选：shell 报告的名字在前（同名时它们盖过 PATH 里的可执行文件），PATH 里的可执行
/// 文件在后。别名的说明是它展开成什么，有规格的命令的说明取规格里的。
fn command_names(shell: &Shell, candidates: &mut Vec<Candidate>) {
    let names = &shell.names;
    let values: HashMap<&str, &str> = names.alias_values.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
    for name in &names.aliases {
        let mut candidate = engine::command(name.clone(), Kind::Alias);
        candidate.description = values.get(name.as_str()).map(|value| (*value).to_owned());
        candidates.push(candidate);
    }
    let groups = [(&names.functions, Kind::Function), (&names.builtins, Kind::Builtin), (&names.keywords, Kind::Keyword)];
    for (list, kind) in groups {
        candidates.extend(list.iter().map(|name| engine::command(name.clone(), kind)));
    }
    let path = shell.path.clone().or_else(|| std::env::var_os("PATH")).unwrap_or_default();
    candidates.extend(commands::executables(&path).into_iter().map(|name| {
        let description = specs::description(&name).map(str::to_owned);
        Candidate { description, ..engine::command(name, Kind::Command) }
    }));
}

/// 候选里能列出来的一共有多少个，和 `rank` 用同样的规则（去掉带控制字符的、隐藏的和重复的），
/// 只是不按输入过滤。
pub fn total(candidates: &[Candidate]) -> usize {
    rank(candidates, "").len()
}

/// 生成器的结果换成候选；要求保持顺序的记下各自的位置，其余在菜单里按名字排。
pub fn generated(results: GeneratorResults, job: &GeneratorJob) -> Vec<Candidate> {
    let ordered = results.is_ordered;
    results
        .suggestions
        .into_iter()
        .enumerate()
        .map(|(i, s)| engine::generated(s, job.from, i, ordered, job.arg.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(input: &str) -> Option<Request> {
        let cursor = input.find('^').unwrap();
        Request::new(&input.replacen('^', "", 1), cursor)
    }

    #[test]
    fn ls_lists_the_home_directory() {
        let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
            return;
        };
        let request = request("ls ~/^").unwrap();
        let candidates = request.local_candidates(Some(Path::new("/")), &Shell::default());
        let expected: Vec<String> = std::fs::read_dir(&home)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|name| !name.starts_with('.'))
            .collect();
        assert_eq!(candidates.iter().filter(|c| c.from == 2).count(), expected.len());
        // 目录结尾带 `/`，接受后不补空格；替换从 `~/` 之后开始。
        let folder = candidates.iter().find(|c| c.value.ends_with('/'));
        if let Some(folder) = folder {
            assert!(folder.value.ends_with('/') && !folder.finish && folder.from == 2);
            let edit = request.accept(folder);
            assert_eq!(edit.backspace, 0);
            assert!(!edit.text.ends_with(' '));
        }
    }

    #[test]
    fn the_first_word_lists_shell_names_and_executables() {
        let shell = Shell {
            path: Some("/bin:/usr/bin".into()),
            names: ShellNames {
                aliases: vec!["gst".into(), "ls".into()],
                alias_values: vec![("gst".into(), "git status".into())],
                functions: vec!["greet".into()],
                builtins: vec!["cd".into()],
                keywords: vec!["if".into()],
            },
            usage: Arc::new(usage::Usage::from_commands(["git status", "git log", "gst"])),
        };
        let listed = |input: &str| -> Vec<Candidate> {
            let request = request(input).unwrap();
            let candidates = request.local_candidates(Some(Path::new("/")), &shell);
            rank(&candidates, request.typed()).into_iter().map(|i| candidates[i].clone()).collect()
        };
        let names = |input: &str| -> Vec<(String, Kind)> {
            listed(input).into_iter().map(|c| (c.value, c.kind)).collect()
        };
        let g = names("g^");
        assert!(g.contains(&("gst".to_owned(), Kind::Alias)) && g.contains(&("greet".to_owned(), Kind::Function)));
        // 历史里用得多的在前：`git` 两次、`gst` 一次。
        if std::path::Path::new("/usr/bin/git").exists() {
            assert_eq!(&g[..2], [("git".to_owned(), Kind::Command), ("gst".to_owned(), Kind::Alias)]);
            // 有规格的命令带着规格里的说明。
            let git = listed("gi^").into_iter().find(|c| c.value == "git").unwrap();
            assert_eq!(git.description.as_deref(), Some("The stupid content tracker"));
        }
        // 别名的说明是它展开成什么。
        let gst = listed("gs^").into_iter().find(|c| c.value == "gst").unwrap();
        assert_eq!(gst.description.as_deref(), Some("git status"));
        // 同名的别名和可执行文件只列一次，别名在前。
        let ls = names("ls^");
        assert_eq!(ls[0], ("ls".to_owned(), Kind::Alias));
        assert_eq!(ls.iter().filter(|(v, _)| v == "ls").count(), 1);
        assert!(names("s^").contains(&("sh".to_owned(), Kind::Command)));
        // 写成路径：可执行文件和目录。
        let bin = names("/bin/s^");
        assert!(bin.contains(&("sh".to_owned(), Kind::File)), "{bin:?}");
    }

    #[test]
    fn git_checkout_runs_a_branch_generator() {
        let request = request("git checkout ^").unwrap();
        let jobs = request.generator_jobs();
        assert!(jobs.iter().any(|job| job.command.contains("git")), "{:?}", jobs.iter().map(|j| &j.command).collect::<Vec<_>>());
        assert_eq!(request.before_word(), "git checkout ");
        assert_eq!(request.cells_before_cursor(), 0);
    }

    #[test]
    fn cargo_and_docker_have_specs() {
        let names = |input: &str| -> Vec<String> {
            let request = request(input).unwrap();
            let candidates = request.local_candidates(None, &Shell::default());
            rank(&candidates, request.typed()).into_iter().map(|i| candidates[i].value.clone()).collect()
        };
        assert!(names("cargo b^").contains(&"build".to_owned()));
        assert!(names("docker ru^").contains(&"run".to_owned()));
        assert!(names("npm i^").contains(&"install".to_owned()));
    }
}
