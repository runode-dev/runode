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

use runode_shared_types::shell::ShellNames;
/// 生成器跑出来的结果，交回 `generated` 换成候选。
pub use warp_command_signatures::GeneratorResults;
use warp_command_signatures::{PathSuggestionType, Suggestion, TemplateType};

pub use commands::is_command;
pub use engine::{Candidate, Edit, Kind, common_prefix, decisive, highlight, rank};
use engine::{Plan, Source};
use line::Segment;
pub use line::cells;
pub use paths::is_executable;

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
        let home = runode_paths::Dirs::from_env().home;
        for source in &self.plan.sources {
            match source {
                Source::Commands => command_names(shell, &mut candidates),
                Source::CommandPaths { from } => {
                    let Some(cwd) = cwd else {
                        continue;
                    };
                    let listed = paths::Filter::Executables;
                    candidates.extend(
                        self.path_candidates(*from, cwd, home.as_deref(), listed, |entry| Some((entry.name, None))),
                    );
                }
                Source::Template { kind, filter, from } => {
                    let Some(cwd) = cwd else {
                        continue;
                    };
                    let listed = match kind {
                        TemplateType::Folders { .. } => paths::Filter::Folders,
                        _ => paths::Filter::All,
                    };
                    let filter = filter
                        .as_ref()
                        .and_then(|name| specs::dynamic(&self.plan.command).and_then(|data| data.filters().get(name)));
                    candidates.extend(self.path_candidates(*from, cwd, home.as_deref(), listed, |entry| {
                        let Some(filter) = filter else {
                            return Some((entry.name, None));
                        };
                        let kind = if entry.is_dir { PathSuggestionType::Folder } else { PathSuggestionType::File };
                        let kept = filter.filter(Suggestion::new(entry.name), kind)?;
                        Some((kept.exact_string, kept.description))
                    }));
                }
                Source::Generator { .. } => {}
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

    /// 当前词从第 `from` 个字符起写的是路径：列出它的目录部分指向的目录，按 `listed` 挑选，
    /// 再由 `keep` 决定每一项留不留、用什么名字和说明。候选从目录部分之后开始替换。
    fn path_candidates(
        &self,
        from: usize,
        cwd: &Path,
        home: Option<&Path>,
        listed: paths::Filter,
        keep: impl Fn(paths::Entry) -> Option<(String, Option<String>)>,
    ) -> Vec<Candidate> {
        let typed: String = self.plan.typed.chars().skip(from).collect();
        let from = from + paths::dir_chars(&typed);
        paths::list(&typed, cwd, home, listed)
            .into_iter()
            .filter_map(|entry| {
                let is_dir = entry.is_dir;
                let (name, description) = keep(entry)?;
                Some(engine::path(name, is_dir, description, from))
            })
            .collect()
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
    let groups =
        [(&names.functions, Kind::Function), (&names.builtins, Kind::Builtin), (&names.keywords, Kind::Keyword)];
    for (list, kind) in groups {
        candidates.extend(list.iter().map(|name| engine::command(name.clone(), kind)));
    }
    let path = shell.path.clone().or_else(|| std::env::var_os("PATH")).unwrap_or_default();
    candidates.extend(commands::executables(&path).into_iter().map(|name| {
        let description = specs::description(&name).map(str::to_owned);
        Candidate { description, ..engine::command(name, Kind::Command) }
    }));
}

/// 有规格的命令 `command` 在已经认出子命令 `path` 之后，`word` 是不是下一层的子命令。
pub fn is_subcommand(command: &str, path: &[String], word: &str) -> bool {
    let Some(spec) = specs::lookup(command) else {
        return false;
    };
    let mut signature = &spec.signature;
    for name in path {
        match signature.subcommands().iter().find(|s| s.name == *name) {
            Some(sub) => signature = sub,
            None => return false,
        }
    }
    signature.subcommands().iter().any(|s| s.name == word)
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
