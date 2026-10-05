//! 沿命令规格往下走，弄清光标所在的词该补什么：子命令、选项、选项的值还是位置参数，再给出
//! 候选和还要去取的动态来源；以及候选的匹配排序、公共前缀和插入时要发给 shell 的按键。

use std::collections::HashSet;

use warp_command_signatures::{
    Argument, ArgumentType, FilterTemplateSuggestion, GeneratorName, Opt, Priority, Signature, TemplateType,
};

use super::{
    line::{self, Quote, Segment, Word},
    specs::Spec,
};

/// 候选属于哪一类，菜单里按它分组标出、着色。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// PATH 里的可执行文件。
    Command,
    Alias,
    Function,
    Builtin,
    Keyword,
    Subcommand,
    Option,
    /// 规格里写好的参数取值。
    Value,
    /// 跑命令生成的参数取值。
    Generated,
    Folder,
    File,
}

/// 菜单里的一项。
#[derive(Clone, Debug)]
pub struct Candidate {
    /// 菜单里显示的名字。
    pub label: String,
    pub kind: Kind,
    /// 接受时写进去的文字，还没转义。
    pub value: String,
    pub description: Option<String>,
    pub priority: Priority,
    /// 只在输入和它完全一样时才列出来。
    pub hidden: bool,
    /// 从当前词（去掉引号后）的第几个字符起换成 `value`：文件只换最后一段，`--opt=value`
    /// 只换 `=` 后面的部分。
    pub from: usize,
    /// 接受后这个词就写完了：闭合引号、补上空格。目录和要接 `=` 值的选项为假。
    pub finish: bool,
    /// 在 runode 的命令历史里用过几次，见 `usage`；匹配程度和优先级都一样时常用的在前。
    pub usage: u32,
    /// 生成器要求保持原样顺序的结果在其中的位置；这些排在按名字排的候选前面。
    pub fixed: Option<u32>,
    /// 分组标签的补充说明：生成器结果所属参数在规格里的名字。
    pub detail: Option<String>,
}

impl Candidate {
    /// 一个显示名和插入文字相同、其余取默认值的候选。
    pub fn new(value: impl Into<String>, kind: Kind) -> Self {
        let value = value.into();
        Self {
            label: value.clone(),
            kind,
            value,
            description: None,
            priority: Priority::Default,
            hidden: false,
            from: 0,
            finish: true,
            usage: 0,
            fixed: None,
            detail: None,
        }
    }
}

/// 还要去取候选的地方。
#[derive(Clone, Debug)]
pub enum Source {
    /// 列出文件或目录，候选从当前词里第 `from` 个字符起（去掉目录部分之前）。
    Template { kind: TemplateType, filter: Option<FilterTemplateSuggestion>, from: usize },
    /// 跑命令生成，候选从当前词里第 `from` 个字符起；`arg` 是参数在规格里的名字。
    Generator { name: GeneratorName, from: usize, group: u32, arg: Option<String> },
    /// 命令名：shell 报告的别名、函数、内建命令、关键字，以及 PATH 里的可执行文件。
    Commands,
    /// 写成路径的命令名：列出可执行文件和目录，候选从当前词里第 `from` 个字符起。
    CommandPaths { from: usize },
}

/// 光标所在的词要补的东西。
#[derive(Clone, Debug)]
pub struct Plan {
    /// 命令名，查动态补全数据用。
    pub command: String,
    /// 不用再去取的候选：子命令、选项、参数的固定取值。
    pub candidates: Vec<Candidate>,
    pub sources: Vec<Source>,
    /// 光标所在的词在光标之前的部分，去掉了引号和转义。
    pub typed: String,
    /// 生成器要的：从命令名起到当前词的各个词，当前词只取光标前的部分，空的不算。
    pub tokens: Vec<String>,
    /// 写在命令前面的变量赋值。
    pub env: Vec<String>,
    /// 候选里的子命令是命令本身的子命令（不是子命令的子命令），按历史统计常用程度时用。
    pub top_level: bool,
}

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

/// 生成器给出的一个结果，是这个生成器的第 `index` 个；标明了是文件或目录的归到文件、目录两组。
/// `ordered` 时保持生成器给的顺序，`arg` 是参数在规格里的名字。
pub fn generated(
    suggestion: warp_command_signatures::Suggestion,
    from: usize,
    index: usize,
    ordered: bool,
    arg: Option<String>,
) -> Candidate {
    use warp_command_signatures::IconType;
    let kind = match suggestion.icon {
        Some(IconType::Folder) => Kind::Folder,
        Some(IconType::File) => Kind::File,
        _ if suggestion.exact_string.ends_with('/') => Kind::Folder,
        _ => Kind::Generated,
    };
    Candidate {
        label: suggestion.display_name.unwrap_or_else(|| suggestion.exact_string.clone()),
        finish: !suggestion.exact_string.ends_with('/'),
        description: suggestion.description,
        priority: suggestion.priority,
        hidden: suggestion.is_hidden,
        from,
        fixed: ordered.then_some(index as u32),
        detail: (kind == Kind::Generated).then_some(arg).flatten(),
        ..Candidate::new(suggestion.exact_string, kind)
    }
}

/// 列出的一个文件或目录；`from` 是目录部分之后。
pub fn path(name: String, is_dir: bool, description: Option<String>, from: usize) -> Candidate {
    let value = if is_dir { format!("{name}/") } else { name };
    Candidate {
        description,
        from,
        finish: !is_dir,
        ..Candidate::new(value, if is_dir { Kind::Folder } else { Kind::File })
    }
}

/// 命令名候选：别名、函数、内建命令、关键字或者 PATH 里的可执行文件。
pub fn command(name: String, kind: Kind) -> Candidate {
    Candidate::new(name, kind)
}

/// 菜单里要高亮的字：显示名 `label` 里和 `query` 匹配上的那些字的下标（按字算，不分大小写）。
/// 插入的文字 `value` 出现在显示名里、又以 `query` 开头时（比如 `-q, --quiet` 里的 `--quiet`）
/// 高亮它开头那几个字；显示名以 `query` 开头时高亮开头；否则是按顺序含有的那些字；都不是时
/// 为空。
pub fn highlight(label: &str, value: &str, query: &str) -> Vec<usize> {
    let query: Vec<char> = query.to_lowercase().chars().collect();
    if query.is_empty() {
        return Vec::new();
    }
    let lower_value: Vec<char> = value.to_lowercase().chars().collect();
    let label: Vec<char> = label.to_lowercase().chars().collect();
    if lower_value.starts_with(&query)
        && let Some(at) = label.windows(lower_value.len().max(1)).position(|w| w == lower_value.as_slice())
    {
        return (at..at + query.len()).collect();
    }
    if label.starts_with(&query) {
        return (0..query.len()).collect();
    }
    let mut positions = Vec::with_capacity(query.len());
    let mut next = query.iter().peekable();
    for (i, c) in label.iter().enumerate() {
        if next.peek() == Some(&c) {
            positions.push(i);
            next.next();
        }
    }
    if next.peek().is_some() { Vec::new() } else { positions }
}

/// `query` 和候选名 `name` 匹配到哪一档：完全一样 0，前缀 1，按顺序含有这些字 2，不匹配
/// `None`。不区分大小写。
fn tier(name: &str, query: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(1);
    }
    let name = name.to_lowercase();
    let query = query.to_lowercase();
    if name == query {
        return Some(0);
    }
    if name.starts_with(&query) {
        return Some(1);
    }
    let mut rest = name.chars();
    query.chars().all(|q| rest.any(|c| c == q)).then_some(2)
}

/// 当前词光标前写了 `typed` 时，候选里能列出来的那些，排好序。
///
/// 有完全一样或以它开头的候选时只列这些，一个都没有时才列按顺序含有这些字的（模糊匹配）。
/// 先按匹配程度（完全一样在前），同一档里依次按：规格里的优先级高的在前、在命令历史里用得
/// 多的在前、（只对命令名）短的在前、生成器要求保持顺序的按原样排在前面、其余按名字的字母序
/// （选项不看开头的横线）。
/// 同一处换成同样文字的只留第一个。
///
/// 名字或插入的文字里有控制字符的候选一律不要：文件名和命令输出里可能带着回车之类，原样
/// 写进 shell 就会执行。菜单、直接插入和公共开头都只从这里拿候选，在这一处挡住就够了。
pub fn rank(candidates: &[Candidate], typed: &str) -> Vec<usize> {
    let mut seen = HashSet::new();
    let mut ranked: Vec<(u8, usize)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            if c.value.chars().chain(c.label.chars()).any(char::is_control) {
                return None;
            }
            let tier = candidate_tier(c, typed)?;
            if c.hidden && tier != 0 {
                return None;
            }
            seen.insert((c.from, c.value.as_str())).then_some((tier, i))
        })
        .collect();
    if ranked.iter().any(|&(tier, _)| tier < 2) {
        ranked.retain(|&(tier, _)| tier < 2);
    }
    ranked.sort_by_cached_key(|&(tier, i)| {
        let c = &candidates[i];
        let command = matches!(c.kind, Kind::Command | Kind::Alias | Kind::Function | Kind::Builtin | Kind::Keyword);
        let length = if command { c.value.chars().count() } else { 0 };
        // 选项按去掉开头横线的名字排，短选项和长选项不会因为横线个数分成两堆。
        let name = if c.kind == Kind::Option { c.label.trim_start_matches('-') } else { c.value.as_str() };
        let order = match c.fixed {
            Some(index) => (0, index, String::new(), String::new()),
            None => (1, 0, name.to_lowercase(), name.to_owned()),
        };
        (tier, std::cmp::Reverse(c.priority), std::cmp::Reverse(c.usage), length, order, i)
    });
    ranked.into_iter().map(|(_, i)| i).collect()
}

/// 按 Tab 后决定直接插入、先插入公共开头还是交还 shell 时看的那些候选（`ranked` 的开头一段）：
/// 有完全一样或按前缀匹配上的就只看它们，一个都没有时才看只按顺序含有的。
pub fn decisive<'a>(candidates: &[Candidate], ranked: &'a [usize], typed: &str) -> &'a [usize] {
    let close = ranked.iter().take_while(|&&i| candidate_tier(&candidates[i], typed).is_some_and(|t| t < 2)).count();
    if close == 0 { ranked } else { &ranked[..close] }
}

/// 候选和当前词光标前的 `typed` 匹配到哪一档，见 `tier`；插入的文字和显示的名字取好的那个。
fn candidate_tier(c: &Candidate, typed: &str) -> Option<u8> {
    let query: String = typed.chars().skip(c.from).collect();
    let by_value = tier(&c.value, &query);
    let by_label = (c.label != c.value).then(|| tier(&c.label, &query)).flatten();
    by_value.into_iter().chain(by_label).min()
}

/// 列出的候选都以 `typed` 开头（不区分大小写）、从同一处换起时，它们共同的开头（按候选
/// 原样的大小写）；比已经写出的长才有。
pub fn common_prefix(candidates: &[Candidate], ranked: &[usize], typed: &str) -> Option<(usize, String)> {
    let first = &candidates[*ranked.first()?];
    let from = first.from;
    let query: String = typed.chars().skip(from).collect();
    let query = query.to_lowercase();
    let mut prefix: Vec<char> = first.value.chars().collect();
    for &i in ranked {
        let c = &candidates[i];
        if c.from != from || !c.value.to_lowercase().starts_with(&query) {
            return None;
        }
        let same = prefix.iter().zip(c.value.chars()).take_while(|(a, b)| **a == *b).count();
        prefix.truncate(same);
    }
    (prefix.len() > query.chars().count()).then(|| (from, prefix.into_iter().collect()))
}

/// 改写当前词时要发给 shell 的按键：先退格删掉光标前要换的部分，再写新的字。光标不动、
/// 光标后面的字一概不碰：那里可能是 shell 或插件画的灰字建议，屏幕上和真的输入分不出来，
/// 往右走或删掉它都会把建议当成输入接受下来。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub backspace: usize,
    pub text: String,
}

/// 把输入 `text`（光标在字节位置 `cursor`）里当前词 `word` 从第 `from` 个字到光标的部分换成
/// `value`，光标后面的部分不动。
///
/// 写完后要回到光标处原来的引号状态，光标后面的字才照旧解析：删掉的部分里有引号开关时
/// 补上相应的引号。`finish` 且光标在词尾时这个词算写完了：闭合引号，后面不是空白就补一个
/// 空格；光标在词中间时不补空格，免得把后面的字拆成另一个词。
pub fn edit(text: &str, cursor: usize, word: &Word, from: usize, value: &str, finish: bool) -> Edit {
    let cursor = cursor.clamp(word.raw.start, word.raw.end);
    let at_cursor = line::quote_at(text, word.raw.start, cursor);
    let (start, quote) = match word.char_at(from) {
        Some(found) if from < word.chars_before(cursor) => found,
        // 光标前没有要换的字：就在光标处写。
        _ => (cursor, at_cursor),
    };
    let mut out = line::escape(value, quote, start == word.raw.start);
    let close = |out: &mut String, quote: Quote| match quote {
        Quote::None => {}
        Quote::Single => out.push('\''),
        Quote::Double => out.push('"'),
    };
    if finish && cursor == word.raw.end {
        close(&mut out, quote);
        if !text[cursor..].starts_with([' ', '\t']) {
            out.push(' ');
        }
    } else if quote != at_cursor {
        close(&mut out, quote);
        close(&mut out, at_cursor);
    }
    Edit { backspace: line::edit_chars(&text[start..cursor]), text: out }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::{line::parse, specs};

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
    fn highlights_prefix_or_scattered_matches() {
        assert_eq!(highlight("Checkout", "Checkout", "ch"), [0, 1]);
        assert_eq!(highlight("cherry-pick", "cherry-pick", "chpk"), [0, 1, 7, 10]);
        assert!(highlight("status", "status", "xyz").is_empty());
        assert!(highlight("status", "status", "").is_empty());
        // 合并显示的选项：高亮插入的那个名字的开头。
        assert_eq!(highlight("-q, --quiet", "--quiet", "--q"), [4, 5, 6]);
        assert_eq!(highlight("-q, --quiet", "-q", "-"), [0]);
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

    fn candidate(value: &str, priority: Priority) -> Candidate {
        Candidate { priority, ..Candidate::new(value, Kind::Value) }
    }

    #[test]
    fn ranks_exact_then_prefix_then_fuzzy() {
        use warp_command_signatures::{Importance, Order};
        let list = vec![
            candidate("xcheck", Priority::Default),
            candidate("check-ignore", Priority::Default),
            candidate("Check", Priority::Default),
            candidate("cherry-pick", Priority::Default),
            candidate("checkout", Priority::Global(Importance::More(Order(80)))),
            candidate("status", Priority::Default),
        ];
        let order: Vec<&str> = rank(&list, "check").into_iter().map(|i| list[i].value.as_str()).collect();
        // 有前缀匹配时，只按顺序含有的 `xcheck`、`cherry-pick` 不列。
        assert_eq!(order, ["Check", "checkout", "check-ignore"]);
        // 没有前缀匹配时才列模糊匹配。
        let order: Vec<&str> = rank(&list, "chk").into_iter().map(|i| list[i].value.as_str()).collect();
        assert_eq!(order, ["checkout", "Check", "check-ignore", "cherry-pick", "xcheck"]);
        let mut hidden = candidate("secret", Priority::Default);
        hidden.hidden = true;
        let list = vec![hidden];
        assert!(rank(&list, "sec").is_empty());
        assert_eq!(rank(&list, "secret"), [0]);
        // 带控制字符的候选不列。
        let list = vec![candidate("a\rrm -rf ~\r", Priority::Default), candidate("ab", Priority::Default)];
        assert_eq!(rank(&list, "a"), [1]);
        let mut labelled = candidate("ok", Priority::Default);
        labelled.label = "o\x1bk".into();
        assert!(rank(&[labelled], "").is_empty());
    }

    #[test]
    fn ties_go_to_usage_then_shorter_commands_then_names() {
        let mut list = vec![
            Candidate::new("gif2webp", Kind::Command),
            Candidate::new("gi-compile-repository", Kind::Command),
            Candidate::new("git", Kind::Command),
            Candidate::new("gist", Kind::Command),
        ];
        let order = |list: &[Candidate]| -> Vec<String> {
            rank(list, "gi").into_iter().map(|i| list[i].value.clone()).collect()
        };
        // 都没用过：命令名短的在前，再按字母序。
        assert_eq!(order(&list), ["git", "gist", "gif2webp", "gi-compile-repository"]);
        // 用过的在前。
        list[3].usage = 2;
        list[0].usage = 1;
        assert_eq!(order(&list), ["gist", "gif2webp", "git", "gi-compile-repository"]);
        // 不是命令名时不看长短，按字母序；生成器要求保持顺序的排在前面、照原样。
        let mut list = vec![
            Candidate::new("zeta", Kind::Subcommand),
            Candidate::new("alpha-long", Kind::Subcommand),
            Candidate { fixed: Some(1), ..Candidate::new("main", Kind::Generated) },
            Candidate { fixed: Some(0), ..Candidate::new("fix", Kind::Generated) },
        ];
        assert_eq!(order_all(&list), ["fix", "main", "alpha-long", "zeta"]);
        list[0].usage = 3;
        assert_eq!(order_all(&list)[0], "zeta");
    }

    #[test]
    fn options_sort_by_name_without_dashes() {
        let list = vec![
            Candidate { label: "--conflict".into(), ..Candidate::new("--conflict", Kind::Option) },
            Candidate { label: "-f, --force".into(), ..Candidate::new("-f", Kind::Option) },
            Candidate { label: "-b".into(), ..Candidate::new("-b", Kind::Option) },
        ];
        assert_eq!(order_all(&list), ["-b", "--conflict", "-f"]);
    }

    fn order_all(list: &[Candidate]) -> Vec<String> {
        rank(list, "").into_iter().map(|i| list[i].value.clone()).collect()
    }

    #[test]
    fn decisions_look_at_prefix_matches_first() {
        let list = vec![
            candidate("checkout", Priority::Default),
            candidate("xcheckout", Priority::Default),
            candidate("cherry-pick", Priority::Default),
        ];
        // 有前缀匹配时只列它们。
        let ranked = rank(&list, "checko");
        assert_eq!(ranked, [0]);
        assert_eq!(decisive(&list, &ranked, "checko"), [0]);
        // 只有按顺序含有的：都算。
        let ranked = rank(&list, "chpk");
        assert_eq!(decisive(&list, &ranked, "chpk"), ranked.as_slice());
    }

    #[test]
    fn common_prefix_uses_the_candidates_own_case() {
        let list = vec![candidate("Checkout", Priority::Default), candidate("Cherry-pick", Priority::Default)];
        let ranked = rank(&list, "c");
        assert_eq!(common_prefix(&list, &ranked, "c"), Some((0, "Che".into())));
        assert_eq!(common_prefix(&list, &ranked, "che"), None);
        // 只按顺序匹配上的 `xabd` 不列，也就不影响公共开头；拿它算公共开头时不成立。
        let list = vec![candidate("abc", Priority::Default), candidate("xabd", Priority::Default)];
        assert_eq!(common_prefix(&list, &rank(&list, "ab"), "ab"), Some((0, "abc".into())));
        assert_eq!(common_prefix(&list, &[0, 1], "ab"), None);
    }

    fn edit_at(input: &str, from: usize, value: &str, finish: bool) -> Edit {
        let cursor = input.find('^').unwrap();
        let text = input.replacen('^', "", 1);
        let segment = parse(&text, cursor);
        edit(&text, cursor, &segment.words[segment.current], from, value, finish)
    }

    fn keys(backspace: usize, text: &str) -> Edit {
        Edit { backspace, text: text.into() }
    }

    #[test]
    fn edits_only_touch_the_text_before_the_cursor() {
        assert_eq!(edit_at("git ch^", 0, "checkout", true), keys(2, "checkout "));
        // 光标在词中间（后面可能是灰字建议）：只换光标前的部分，不往右走，也不补空格。
        assert_eq!(edit_at("git ch^eck x", 0, "checkout", true), keys(2, "checkout"));
        // 空词。
        assert_eq!(edit_at("git ^", 0, "status", true), keys(0, "status "));
        // 光标后面已经是空白：不再补空格。
        assert_eq!(edit_at("git ch^ x", 0, "checkout", true), keys(2, "checkout"));
    }

    #[test]
    fn edits_escape_and_close_quotes() {
        // 文件名只换最后一段，空格加反斜杠；目录不补空格。
        assert_eq!(edit_at("ls ~/My^", 2, "My Dir/", false), keys(2, r"My\ Dir/"));
        // 在双引号里：不加反斜杠，写完时闭合引号。
        assert_eq!(edit_at(r#"ls "My D^"#, 0, "My Doc.txt", true), keys(4, r#"My Doc.txt" "#));
        // 光标在闭合的引号之后：引号在删掉的部分里，写完的词再闭合一次。
        assert_eq!(edit_at(r#"ls "ab"^"#, 0, "abc", true), keys(3, r#"abc" "#));
        // 目录也一样补回删掉的闭合引号，之后接着输入时引号外的字照样连在这个词上。
        assert_eq!(edit_at(r#"ls "My D"^"#, 0, "My Dir/", false), keys(5, r#"My Dir/""#));
        // 引号没闭合、还没写完：引号留着开着。
        assert_eq!(edit_at(r#"ls "My D^"#, 0, "My Dir/", false), keys(4, "My Dir/"));
        // 光标在引号里的词中间：只换光标前的部分，引号状态和光标处一样，不用补。
        assert_eq!(edit_at(r#"ls "a^b" x"#, 0, "abc", true), keys(1, "abc"));
        // 已经输入了反斜杠转义：原文里的反斜杠也一起删掉。
        assert_eq!(edit_at(r"ls My\ D^", 0, "My Dir/", false), keys(5, r"My\ Dir/"));
        // 宽字符算一个字。
        assert_eq!(edit_at("cat 文^", 0, "文件", true), keys(1, "文件 "));
        // 要接 `=` 值的选项不补空格。
        assert_eq!(edit_at("x --fo^", 0, "--format=", false).text, "--format=");
        // 开头的 `~` 是字面的文件名时要转义。
        assert_eq!(edit_at("ls ^", 0, "~x", true), keys(0, r"\~x "));
    }

}
