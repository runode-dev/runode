//! 沿命令规格往下走，弄清光标所在的词该补什么：子命令、选项、选项的值还是位置参数，再给出
//! 候选和还要去取的动态来源。

use std::collections::HashSet;

use warp_command_signatures::{Argument, ArgumentType, Opt, Signature};

use super::{Candidate, Kind, Plan, Source};
use crate::{
    line::{self, Segment, Word},
    specs::Spec,
};

/// 光标所在的词能不能由 runode 补全，能的话要补什么。`lookup` 按命令名找规格。
///
/// 光标在命令名上（一段命令跳过变量赋值后的第一个词，或者 `sudo` 这类命令后面那个本身是
/// 命令的参数）时补命令名。在重定向目标上、正在写变量赋值、或者参数所属的命令没有规格时
/// 为 `None`，这时 Tab 交给 shell。
pub fn plan(segment: &Segment, cursor: usize, lookup: &dyn Fn(&str) -> Option<std::sync::Arc<Spec>>) -> Option<Plan> {
    let words = &segment.words;
    let current = &words[segment.current];
    if current.redirect {
        return None;
    }
    let typed = current.value_before(cursor);
    // 跳过开头的变量赋值，第一个词是命令。
    let start = words[..segment.current].iter().position(|w| !line::is_assignment(&w.value()));
    let env: Vec<String> = words[..start.unwrap_or(segment.current)].iter().map(Word::value).collect();
    let Some(start) = start else {
        // 光标就在命令名上；正在写的是变量赋值时不管。
        return (!line::is_assignment(&typed)).then(|| command_plan(typed, env));
    };
    let mut command_at = start;
    loop {
        let name = command_name(&words[command_at].value());
        let spec = lookup(&name)?;
        // 光标前的、不是重定向目标的参数。
        let args: Vec<(usize, String)> = (command_at + 1..segment.current)
            .filter(|&i| !words[i].redirect)
            .map(|i| (i, words[i].value()))
            .collect();
        match walk(&spec, &args) {
            Walked::Nested(at) => {
                // `sudo git ...` 这种参数本身是一条命令：从它重新开始。
                command_at = at;
            }
            Walked::At(state) => {
                // 光标正在本身是一条命令的参数上：补的是命令名。
                if state.at_command(&typed) {
                    return Some(command_plan(typed, env));
                }
                let mut tokens: Vec<String> =
                    (command_at..segment.current).filter(|&i| !words[i].redirect).map(|i| words[i].value()).collect();
                if !typed.is_empty() {
                    tokens.push(typed.clone());
                }
                let top_level = std::ptr::eq(state.signature, &spec.signature);
                let mut plan =
                    Plan { command: name, candidates: Vec::new(), sources: Vec::new(), typed, tokens, env, top_level };
                state.candidates(&spec, &mut plan);
                return Some(plan);
            }
        }
    }
}

/// 补命令名：写成路径（含 `/`）时列出可执行文件和目录，否则列出 shell 报告的名字和 PATH 里的
/// 可执行文件。
fn command_plan(typed: String, env: Vec<String>) -> Plan {
    let source = if typed.contains('/') { Source::CommandPaths { from: 0 } } else { Source::Commands };
    Plan {
        command: String::new(),
        candidates: Vec::new(),
        sources: vec![source],
        typed,
        tokens: Vec::new(),
        env,
        top_level: false,
    }
}

/// 命令名：写成路径时取最后一段。
fn command_name(word: &str) -> String {
    word.rsplit('/').next().unwrap_or(word).to_owned()
}

enum Walked<'a> {
    /// 第几个词是一条新命令的开头。
    Nested(usize),
    At(State<'a>),
}

/// 走完光标前的各个词之后停在哪里。
struct State<'a> {
    signature: &'a Signature,
    /// 下一个位置参数是第几个。
    arg: usize,
    /// 已经过了 `--`，后面都是参数。
    after_dashes: bool,
    /// 当前这一层已经用过的选项名。
    used: HashSet<String>,
    /// 前一个选项还要接参数：选项和它的第几个参数。
    pending: Option<(&'a Opt, usize)>,
}

fn walk<'a>(spec: &'a Spec, args: &[(usize, String)]) -> Walked<'a> {
    let mut state = State {
        signature: &spec.signature,
        arg: 0,
        after_dashes: false,
        used: HashSet::new(),
        pending: None,
    };
    for (index, word) in args {
        if let Some((opt, k)) = state.pending.take() {
            let params = opt.arguments();
            let variadic = params.get(k).is_some_and(Argument::is_variadic);
            // 可变个数的选项参数一直吃到下一个选项为止。
            if !(variadic && word.starts_with('-') && word.len() > 1) {
                if k + 1 < params.len() {
                    state.pending = Some((opt, k + 1));
                } else if variadic {
                    state.pending = Some((opt, k));
                }
                continue;
            }
        }
        if !state.after_dashes && word == "--" {
            state.after_dashes = true;
            continue;
        }
        if !state.after_dashes && word.starts_with('-') && word.len() > 1 {
            state.option(word);
            continue;
        }
        if state.arg == 0
            && !state.after_dashes
            && let Some(sub) = state.signature.subcommands().iter().find(|s| s.name == *word)
        {
            state.signature = sub;
            state.used.clear();
            continue;
        }
        let params = state.signature.arguments();
        match params.get(state.arg) {
            Some(param) if param.is_command() => return Walked::Nested(*index),
            Some(param) if param.is_variadic() => {}
            _ => state.arg += 1,
        }
    }
    Walked::At(state)
}

impl<'a> State<'a> {
    fn find(&self, name: &str) -> Option<&'a Opt> {
        self.signature.options().iter().find(|opt| opt.has_name(name))
    }

    /// 光标所在的词（光标前写了 `typed`）是不是一个本身是命令的位置参数。
    fn at_command(&self, typed: &str) -> bool {
        self.pending.is_none()
            && (self.after_dashes || !typed.starts_with('-'))
            && self.signature.arguments().get(self.arg).is_some_and(Argument::is_command)
    }

    /// 要接参数的选项：第一个参数是必填的。可选参数只在写成 `--opt=value` 时才算。
    fn takes_value(opt: &Opt) -> bool {
        opt.arguments().first().is_some_and(Argument::is_required)
    }

    /// 光标前的一个选项：记下用过，要接参数时等下一个词。
    fn option(&mut self, word: &str) {
        if let Some((name, _)) = word.split_once('=') {
            self.used.insert(name.to_owned());
            return;
        }
        if let Some(opt) = self.find(word) {
            self.used.insert(word.to_owned());
            if Self::takes_value(opt) {
                self.pending = Some((opt, 0));
            }
            return;
        }
        // 合并在一起的短选项，比如 `-am`；要接参数的那个之后的字是它的值，比如 `-ofile`。
        if word.starts_with("--") {
            return;
        }
        let letters: Vec<char> = word[1..].chars().collect();
        for (i, c) in letters.iter().enumerate() {
            let name = format!("-{c}");
            let Some(opt) = self.find(&name) else {
                return;
            };
            self.used.insert(name);
            if Self::takes_value(opt) {
                if i + 1 == letters.len() {
                    self.pending = Some((opt, 0));
                }
                return;
            }
        }
    }

    /// 光标所在的词的候选。
    fn candidates(&self, spec: &Spec, plan: &mut Plan) {
        let typed = plan.typed.clone();
        if let Some((opt, k)) = self.pending {
            // 可变个数的选项参数遇到 `-` 开头的词时当作新选项。
            let variadic = opt.arguments().get(k).is_some_and(Argument::is_variadic);
            if !(variadic && typed.starts_with('-')) {
                if let Some(param) = opt.arguments().get(k) {
                    argument(param, 0, plan);
                }
                return;
            }
        }
        if !self.after_dashes && typed.starts_with('-') {
            if let Some((name, _)) = typed.split_once('=') {
                if let Some(param) = self.find(name).and_then(|opt| opt.arguments().first()) {
                    argument(param, name.chars().count() + 1, plan);
                }
                return;
            }
            self.options(spec, plan);
            return;
        }
        if self.arg == 0 && !self.after_dashes {
            for sub in self.signature.subcommands() {
                plan.candidates.push(Candidate {
                    description: sub.description.clone(),
                    priority: sub.priority,
                    hidden: spec.hidden.contains(&sub.name),
                    ..Candidate::new(sub.name.clone(), Kind::Subcommand)
                });
            }
        }
        let params = self.signature.arguments();
        let param = params.get(self.arg).or_else(|| params.last().filter(|p| p.is_variadic()));
        if let Some(param) = param.filter(|p| !p.is_command()) {
            argument(param, 0, plan);
        }
        // 当前词不以 `-` 开头时只在别的什么都没有时才列选项，免得几十个选项淹没子命令和参数。
        if typed.is_empty() && !self.after_dashes && plan.candidates.is_empty() && plan.sources.is_empty() {
            self.options(spec, plan);
        }
    }

    /// 每个选项一项：几个名字合在一起显示（短的在前，如 `-q, --quiet`），插入时用和当前词
    /// 对得上的那个名字，见 `option_name`。
    fn options(&self, spec: &Spec, plan: &mut Plan) {
        for opt in self.signature.options() {
            let repeatable = opt.names().any(|n| spec.repeatable.contains(n));
            if !repeatable && opt.names().any(|n| self.used.contains(n)) {
                continue;
            }
            let mut names: Vec<&str> = opt.names().collect();
            names.sort_by_key(|name| (name.len(), *name));
            let Some(name) = option_name(&names, &plan.typed) else {
                continue;
            };
            let equals = spec.requires_equals.contains(name);
            let value = if equals && !name.ends_with('=') { format!("{name}=") } else { name.to_owned() };
            plan.candidates.push(Candidate {
                label: names.join(", "),
                finish: !value.ends_with('='),
                description: opt.description.clone(),
                priority: opt.priority,
                hidden: opt.names().any(|n| spec.hidden.contains(n)),
                ..Candidate::new(value, Kind::Option)
            });
        }
    }
}

/// 一个选项的几个名字（短的在前）里插入哪一个：当前词 `typed` 是哪个名字的开头就用哪个（都是
/// 时取短的）；都不是时，`typed` 以 `--` 开头取长名，否则取短名。
fn option_name<'a>(names: &[&'a str], typed: &str) -> Option<&'a str> {
    let lower = typed.to_lowercase();
    if let Some(name) = names.iter().find(|name| name.to_lowercase().starts_with(&lower)) {
        return Some(name);
    }
    if typed.starts_with("--") {
        names.iter().rev().find(|name| name.starts_with("--")).or(names.last()).copied()
    } else {
        names.first().copied()
    }
}

/// 参数的各种来源的组，从这里往后数，按规格里列出的顺序；用来区分同一个参数的几个生成器。
const ARGUMENTS: u32 = 1;

/// 一个参数的候选：固定取值直接给出，文件和生成器记作还要去取的来源。
fn argument(param: &Argument, from: usize, plan: &mut Plan) {
    for (i, kind) in param.argument_types.iter().enumerate() {
        let group = ARGUMENTS + i as u32;
        match kind {
            ArgumentType::Suggestion(suggestion) => plan.candidates.push(Candidate {
                label: suggestion.display_name.clone().unwrap_or_else(|| suggestion.exact_string.clone()),
                description: suggestion.description.clone(),
                priority: suggestion.priority,
                hidden: suggestion.is_hidden,
                from,
                ..Candidate::new(suggestion.exact_string.clone(), Kind::Value)
            }),
            ArgumentType::Template(template) => plan.sources.push(Source::Template {
                kind: template.type_name.clone(),
                filter: template.filter_name.clone(),
                from,
            }),
            ArgumentType::Generator(name) => {
                plan.sources.push(Source::Generator { name: name.clone(), from, group, arg: param.display_name.clone() })
            }
            ArgumentType::Alias(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{engine::rank, line::parse, specs};

    fn plan_at(input: &str) -> Option<Plan> {
        let cursor = input.find('^').unwrap();
        let text = input.replacen('^', "", 1);
        let segment = parse(&text, cursor);
        plan(&segment, cursor, &specs::lookup)
    }

    /// 排好的候选值。
    fn ranked(input: &str) -> Vec<String> {
        let plan = plan_at(input).unwrap();
        rank(&plan.candidates, &plan.typed).into_iter().map(|i| plan.candidates[i].value.clone()).collect()
    }

    #[test]
    fn hands_unknown_commands_and_redirects_to_the_shell() {
        assert!(plan_at("no-such-command-xyz a^").is_none());
        assert!(plan_at("git status > ou^").is_none());
        // 正在写变量赋值。
        assert!(plan_at("FOO=^").is_none());
        // 变量赋值后面的命令照样补。
        assert_eq!(plan_at("FOO=1 git ch^").unwrap().env, ["FOO=1"]);
    }

    fn is_command_plan(plan: &Plan) -> bool {
        matches!(plan.sources.as_slice(), [Source::Commands])
    }

    #[test]
    fn the_first_word_completes_command_names() {
        for input in ["gi^", "^", "FOO=1 gi^", "ls | gr^", "make && ca^", "sudo gi^"] {
            let plan = plan_at(input).unwrap_or_else(|| panic!("{input}"));
            assert!(is_command_plan(&plan), "{input}: {:?}", plan.sources);
        }
        assert_eq!(plan_at("FOO=1 gi^").unwrap().env, ["FOO=1"]);
        // 写成路径时列可执行文件和目录。
        for input in ["./scr^", "/usr/b^", "~/bin/^", "bin/x^"] {
            let plan = plan_at(input).unwrap();
            assert!(matches!(plan.sources.as_slice(), [Source::CommandPaths { from: 0 }]), "{input}");
        }
    }

    #[test]
    fn completes_git_subcommands() {
        let values = ranked("git ch^");
        assert!(values.contains(&"checkout".to_owned()), "{values:?}");
        assert!(values.contains(&"cherry-pick".to_owned()), "{values:?}");
        // 有前缀匹配时只列前缀匹配的。
        assert!(values.iter().all(|v| v.starts_with("ch")), "{values:?}");
        // 完整路径写的命令名也认。
        assert!(ranked("/usr/bin/git ch^").contains(&"checkout".to_owned()));
        // 管道后面的那一段。
        assert!(ranked("ls | git ch^").contains(&"checkout".to_owned()));
    }

    #[test]
    fn lists_options_after_a_dash() {
        let plan = plan_at("git checkout -^").unwrap();
        assert!(plan.candidates.iter().all(|c| c.kind == Kind::Option));
        let values = ranked("git checkout -^");
        assert!(values.contains(&"-b".to_owned()), "{values:?}");
        // 同一个选项的几个名字合成一项，短名在前；只写了 `-` 时插入短名。
        let quiet = plan.candidates.iter().find(|c| c.label == "-q, --quiet").unwrap();
        assert_eq!(quiet.value, "-q");
        assert!(!plan.candidates.iter().any(|c| c.label == "--quiet"));
        let plan = plan_at("git checkout --q^").unwrap();
        assert!(plan.candidates.iter().any(|c| c.label == "-q, --quiet" && c.value == "--quiet"));
        // 用过的选项不再列。
        assert!(!ranked("git checkout --quiet --q^").contains(&"--quiet".to_owned()));
        let cargo = ranked("cargo build --^");
        assert!(cargo.contains(&"--release".to_owned()), "{cargo:?}");
        assert!(cargo.iter().all(|v| v.starts_with("--")));
    }

    #[test]
    fn checkout_branches_come_from_a_generator() {
        let plan = plan_at("git checkout ^").unwrap();
        assert!(plan.sources.iter().any(|s| matches!(s, Source::Generator { .. })), "{:?}", plan.sources);
        assert_eq!(plan.tokens, ["git", "checkout"]);
        // 空词、还有别的候选时不列选项。
        assert!(plan.candidates.iter().all(|c| c.kind != Kind::Option));
        assert!(plan_at("git ^").unwrap().candidates.iter().all(|c| c.kind != Kind::Option));
    }

    #[test]
    fn options_are_listed_when_nothing_else_fits() {
        // `git status` 后面只接路径（文件列表），也不列选项；`git commit -m x` 后面也没有别的了。
        let plan = plan_at("cargo build ^").unwrap();
        assert!(!plan.candidates.is_empty() && plan.candidates.iter().all(|c| c.kind == Kind::Option));
    }

    #[test]
    fn picks_the_option_name_that_fits() {
        let names = ["-q", "--quiet"];
        assert_eq!(option_name(&names, ""), Some("-q"));
        assert_eq!(option_name(&names, "-"), Some("-q"));
        assert_eq!(option_name(&names, "--"), Some("--quiet"));
        assert_eq!(option_name(&names, "--qu"), Some("--quiet"));
        assert_eq!(option_name(&["--version"], "-"), Some("--version"));
        // 都对不上时：`--` 开头取长名，否则取短名。
        assert_eq!(option_name(&names, "--x"), Some("--quiet"));
        assert_eq!(option_name(&names, "-x"), Some("-q"));
    }

    #[test]
    fn options_take_their_values() {
        // `-b` 要接一个新分支名，没有固定取值，也就没有候选。
        let plan = plan_at("git checkout -b ^").unwrap();
        assert!(plan.candidates.iter().all(|c| c.kind != Kind::Subcommand && c.kind != Kind::Option));
        // 接过值以后又回到位置参数。
        let plan = plan_at("git checkout -b new ^").unwrap();
        assert!(plan.sources.iter().any(|s| matches!(s, Source::Generator { .. })));
        // `--opt=value`：只换 `=` 后面的部分。
        let plan = plan_at("git commit --cleanup=s^").unwrap();
        assert!(plan.candidates.iter().all(|c| c.from == "--cleanup=".len()), "{:?}", plan.candidates);
        let values: Vec<String> =
            rank(&plan.candidates, &plan.typed).into_iter().map(|i| plan.candidates[i].value.clone()).collect();
        assert!(values.contains(&"strip".to_owned()), "{values:?}");
    }

    #[test]
    fn after_double_dash_only_arguments() {
        let plan = plan_at("git checkout -- ^").unwrap();
        assert!(plan.candidates.iter().all(|c| c.kind != Kind::Option));
    }

    #[test]
    fn nested_commands_start_over() {
        assert!(ranked("sudo git ch^").contains(&"checkout".to_owned()));
    }

    #[test]
    fn ls_lists_files() {
        let plan = plan_at("ls ~/^").unwrap();
        assert!(plan.sources.iter().any(|s| matches!(s, Source::Template { .. })), "{:?}", plan.sources);
        assert_eq!(plan.typed, "~/");
    }
}
