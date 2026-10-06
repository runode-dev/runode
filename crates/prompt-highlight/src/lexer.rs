//! 把一行输入扫成要上色的各段：按命令名、参数、重定向目标、括号这些位置认词。命令名、参数
//! 和路径怎么认见 `classify`，引号、`$`、反引号和历史展开见 `quote`。

mod classify;
mod quote;

use std::{
    ops::Range,
    path::{Path, PathBuf},
};

use runode_completion::is_subcommand;

use crate::{Kind, Shell};

/// 一段要上色的文字：在输入里的字节区间和它的类别。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    pub kind: Kind,
}

/// 给一行输入上色，`cwd` 是 shell 所在的目录，相对路径从它算。返回的各段按顺序叠上去：
/// 后面的盖过前面的，比如整个词是路径，里面带引号的部分仍是字符串的颜色。
pub fn highlight(text: &str, shell: &Shell, cwd: Option<&Path>) -> Vec<Span> {
    let home = runode_paths::Dirs::from_env().home;
    let mut lexer = Lexer { text, pos: 0, spans: Vec::new(), shell, cwd, home, depth: 0, case_depth: 0, closing: None };
    lexer.list(None);
    lexer.spans.retain(|span| !span.range.is_empty());
    lexer.spans
}

/// 下一个词出现在什么位置上。
#[derive(Clone, Debug)]
enum Expect {
    /// 命令名。
    Command,
    /// 前置命令（`sudo` 等）的选项，选项之后是命令名。`values` 是后面跟着值的选项。
    PrecommandOptions { values: &'static [&'static str], value_next: bool },
    /// 命令的参数。`command` 是查规格用的命令名，`subcommands` 是已经认出的子命令；
    /// 只有第一个不是选项的参数可能是子命令。
    Args { command: Option<String>, subcommands: Vec<String>, positional: usize, after_dashes: bool },
    /// `for` 后面的变量名。
    ForVar,
    /// `for x` 后面，等 `in`。
    ForIn,
    /// `case` 后面的词。
    CaseSubject,
    /// `case x` 后面，等 `in`。
    CaseIn,
    /// `case … in` 里的模式，到 `)` 为止。
    CasePattern,
    /// `function` 后面的函数名。
    FunctionName,
}

impl Expect {
    fn args(command: Option<String>) -> Self {
        Expect::Args { command, subcommands: Vec::new(), positional: 0, after_dashes: false }
    }
}

/// 扫过的一个词。
struct Word {
    range: Range<usize>,
    /// 去掉引号和转义后的文字；带变量、命令替换这类要展开的东西时为 `None`。
    literal: Option<String>,
    /// 有不在引号里的通配符。
    glob: bool,
    /// 第一个字在引号里或者被转义了，比如 `"-x"` 不算选项。
    quoted_start: bool,
}

struct Lexer<'a> {
    text: &'a str,
    pos: usize,
    spans: Vec<Span>,
    shell: &'a Shell,
    cwd: Option<&'a Path>,
    home: Option<PathBuf>,
    /// 括号嵌套了几层，决定括号的颜色。
    depth: u8,
    /// 在几层 `case … esac` 里，`;;` 之后是模式还是命令要看它。
    case_depth: usize,
    /// 正在分析的这串命令到哪个字为止，见 `list`；反引号里的词遇到反引号就结束。
    closing: Option<char>,
}

/// 词在这些字前面结束。
fn ends_word(c: char) -> bool {
    c.is_whitespace() || matches!(c, ';' | '&' | '|' | '<' | '>' | '(' | ')')
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `NAME=`、`NAME+=`、`NAME[i]=` 开头的赋值，返回 `=` 之后的字节位置。
fn assignment(raw: &str) -> Option<usize> {
    let name = raw.find(|c: char| !is_name_char(c)).unwrap_or(raw.len());
    if name == 0 || raw.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    let mut rest = &raw[name..];
    if rest.starts_with('[') {
        rest = &rest[rest.find(']')? + 1..];
    }
    let rest = rest.strip_prefix('+').unwrap_or(rest);
    rest.starts_with('=').then(|| raw.len() - rest.len() + 1)
}

impl Lexer<'_> {
    fn peek(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.text[self.pos..].chars().nth(offset)
    }

    fn rest(&self) -> &str {
        &self.text[self.pos..]
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn span(&mut self, range: Range<usize>, kind: Kind) {
        self.spans.push(Span { range, kind });
    }

    /// 一串命令，到 `end` 为止（不吃掉它）；`end` 为 `None` 时到输入末尾。
    fn list(&mut self, end: Option<char>) {
        let outer = std::mem::replace(&mut self.closing, end);
        self.commands(end);
        self.closing = outer;
    }

    fn commands(&mut self, end: Option<char>) {
        let mut expect = Expect::Command;
        // 下一个词是重定向的目标；`true` 时它是 here-doc 的结束标记，不当路径。
        let mut redirect: Option<bool> = None;
        loop {
            while let Some(c) = self.peek() {
                if c == ' ' || c == '\t' {
                    self.pos += 1;
                } else if c == '\\' && self.peek_at(1) == Some('\n') {
                    self.pos += 2;
                } else {
                    break;
                }
            }
            let Some(c) = self.peek() else {
                return;
            };
            let start = self.pos;
            if c == ')' && matches!(expect, Expect::CasePattern) {
                self.pos += 1;
                expect = Expect::Command;
                continue;
            }
            if Some(c) == end {
                return;
            }
            match c {
                '\n' => {
                    self.pos += 1;
                    expect = Expect::Command;
                }
                '#' => {
                    let len = self.rest().find('\n').unwrap_or(self.rest().len());
                    self.pos += len;
                    self.span(start..self.pos, Kind::Comment);
                }
                ';' => {
                    self.pos += 1;
                    if matches!(self.peek(), Some(';' | '&' | '|')) {
                        self.pos += 1;
                        expect = if self.case_depth > 0 { Expect::CasePattern } else { Expect::Command };
                    } else {
                        expect = Expect::Command;
                    }
                }
                '&' if matches!(self.peek_at(1), Some('>')) => redirect = Some(self.redirection()),
                '&' | '|' => {
                    self.pos += 1;
                    if matches!(self.peek(), Some('&' | '|' | '!')) {
                        self.pos += 1;
                    }
                    if !(c == '|' && matches!(expect, Expect::CasePattern)) {
                        expect = Expect::Command;
                    }
                }
                '<' | '>' => redirect = Some(self.redirection()),
                '0'..='9' if self.fd_redirection() => redirect = Some(self.redirection()),
                '(' => self.group(&mut expect),
                ')' => {
                    // 没配对的右括号。
                    self.pos += 1;
                    expect = Expect::args(None);
                }
                _ => {
                    if let Some(heredoc) = redirect.take() {
                        let first = self.spans.len();
                        let word = self.word(false);
                        if !heredoc && let Some(kind) = self.path_kind(&word) {
                            self.spans.insert(first, Span { range: word.range, kind });
                        }
                        continue;
                    }
                    self.command_word(&mut expect);
                }
            }
        }
    }

    /// 光标处是 `2>` 这样带文件描述符的重定向。
    fn fd_redirection(&self) -> bool {
        let digits = self.rest().find(|c: char| !c.is_ascii_digit()).unwrap_or(self.rest().len());
        matches!(self.rest()[digits..].chars().next(), Some('<' | '>'))
    }

    /// 吃掉一个重定向运算符，返回后面的词是不是 here-doc 的结束标记。`>&2` 这样直接接着
    /// 文件描述符的连它一起吃掉，后面就没有目标了，这时也返回 `true`。
    fn redirection(&mut self) -> bool {
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        const OPERATORS: &[&str] =
            &["<<<", "<<-", "<<", "<>", "<&", "<", "&>>", "&>", ">>|", ">>!", ">>&", ">>", ">|", ">!", ">&", ">"];
        let Some(op) = OPERATORS.iter().find(|op| self.rest().starts_with(**op)) else {
            self.pos += 1;
            return true;
        };
        self.pos += op.len();
        match *op {
            "<<<" => {
                self.span(self.pos - 3..self.pos, Kind::HereStringTri);
                false
            }
            "<<" | "<<-" => true,
            "<&" | ">&" => {
                let fd = self.rest().find(|c: char| !c.is_ascii_digit() && c != '-').unwrap_or(self.rest().len());
                if fd > 0 && self.rest()[fd..].chars().next().is_none_or(ends_word) {
                    self.pos += fd;
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    /// `(`：子 shell、算术命令 `((`，或者 `case` 模式前可写可不写的左括号。
    fn group(&mut self, expect: &mut Expect) {
        let start = self.pos;
        self.pos += 1;
        if matches!(expect, Expect::CasePattern) {
            return;
        }
        if self.peek() == Some('(') && matches!(expect, Expect::Command | Expect::ForVar) {
            self.pos += 1;
            self.span(start..self.pos, Kind::ReservedWord);
            if self.math() {
                self.span(self.pos - 2..self.pos, Kind::ReservedWord);
            }
            *expect = Expect::args(None);
            return;
        }
        let level = self.open(start);
        let inner = self.pos;
        self.list(Some(')'));
        let empty = self.text[inner..self.pos].trim().is_empty();
        self.close(level);
        // `foo () {` 定义函数：空括号后面是函数体。
        *expect = if empty { Expect::Command } else { Expect::args(None) };
    }

    /// 从 `start` 到光标是一个左括号，记下它的层数。
    fn open(&mut self, start: usize) -> u8 {
        self.depth = self.depth.saturating_add(1);
        let level = self.depth;
        self.span(start..self.pos, Kind::BracketLevel(level));
        level
    }

    /// 光标处是配对的右括号时吃掉它。
    fn close(&mut self, level: u8) {
        if self.peek() == Some(')') {
            self.span(self.pos..self.pos + 1, Kind::BracketLevel(level));
            self.pos += 1;
        }
        self.depth = self.depth.saturating_sub(1);
    }

    /// 算术：数字和变量名上色，到配对的 `))` 为止，吃掉它时返回 `true`。
    fn math(&mut self) -> bool {
        let mut depth = 0usize;
        while let Some(c) = self.peek() {
            let start = self.pos;
            match c {
                ')' if depth == 0 && self.peek_at(1) == Some(')') => {
                    self.pos += 2;
                    return true;
                }
                '(' => {
                    depth += 1;
                    self.pos += 1;
                }
                ')' => {
                    depth = depth.saturating_sub(1);
                    self.pos += 1;
                }
                '$' => self.dollar(),
                c if c.is_ascii_digit() => {
                    while self.peek().is_some_and(|c| c.is_ascii_alphanumeric() || c == '#' || c == '.') {
                        self.pos += 1;
                    }
                    self.span(start..self.pos, Kind::MathNum);
                }
                c if is_name_char(c) => {
                    while self.peek().is_some_and(is_name_char) {
                        self.pos += 1;
                    }
                    self.span(start..self.pos, Kind::MathVar);
                }
                _ => {
                    self.bump();
                }
            }
        }
        false
    }

    /// 命令名、参数等普通的词，按 `expect` 认它是什么，再定下一个词的位置。
    fn command_word(&mut self, expect: &mut Expect) {
        // 词里的引号、变量等先记下，整个词的颜色要插在它们前面。
        let first = self.spans.len();
        let word = self.word(matches!(expect, Expect::Command | Expect::PrecommandOptions { .. }));
        let text = self.text;
        let raw = &text[word.range.clone()];
        let literal = word.literal.as_deref();
        let option = raw.len() > 1 && raw.starts_with('-') && !word.quoted_start;
        let option_kind = || if raw.starts_with("--") { Kind::DoubleHyphenOption } else { Kind::SingleHyphenOption };
        let mut outer = None;
        match expect {
            Expect::PrecommandOptions { value_next, .. } if *value_next => *value_next = false,
            Expect::PrecommandOptions { values, value_next } if option => {
                outer = Some(option_kind());
                *value_next = values.contains(&raw);
            }
            Expect::Command | Expect::PrecommandOptions { .. } => {
                if assignment(raw).is_some() {
                    // 赋值本身不上色；`a=(…)` 接着是数组。
                    if self.peek() == Some('(') && raw.ends_with('=') {
                        self.array();
                    }
                } else {
                    let (kind, next) = self.command_name(literal);
                    outer = kind;
                    *expect = next;
                }
            }
            Expect::Args { command, subcommands, positional, after_dashes } => {
                if literal == Some("]]") && command.as_deref() == Some("[[") {
                    outer = Some(Kind::DoubleSqBracket);
                } else if literal == Some("]") && command.as_deref() == Some("[") {
                    outer = Some(Kind::SingleSqBracket);
                } else if option && !*after_dashes {
                    *after_dashes = raw == "--";
                    outer = Some(option_kind());
                } else if *positional == 0
                    && let (Some(command), Some(literal)) = (command.as_deref(), literal)
                    && is_subcommand(command, subcommands, literal)
                {
                    subcommands.push(literal.to_owned());
                    outer = Some(Kind::Subcommand);
                } else {
                    *positional += 1;
                    outer = self.argument_kind(&word);
                }
            }
            Expect::ForVar => *expect = Expect::ForIn,
            Expect::ForIn | Expect::CaseIn if literal == Some("in") => {
                outer = Some(Kind::ReservedWord);
                if matches!(expect, Expect::CaseIn) {
                    self.case_depth += 1;
                    *expect = Expect::CasePattern;
                } else {
                    *expect = Expect::args(None);
                }
            }
            Expect::ForIn | Expect::CaseIn => {
                outer = self.argument_kind(&word);
                *expect = Expect::args(None);
            }
            Expect::CaseSubject => *expect = Expect::CaseIn,
            Expect::CasePattern if literal == Some("esac") => {
                outer = Some(Kind::ReservedWord);
                self.case_depth = self.case_depth.saturating_sub(1);
                *expect = Expect::args(None);
            }
            Expect::CasePattern => {}
            Expect::FunctionName => *expect = Expect::Command,
        }
        if let Some(kind) = outer {
            self.spans.insert(first, Span { range: word.range, kind });
        }
    }

    /// `a=(…)` 的数组：括号是绿色，元素当参数。
    fn array(&mut self) {
        let start = self.pos;
        self.pos += 1;
        self.span(start..self.pos, Kind::AssignArrayBracket);
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.pos += 1;
            }
            match self.peek() {
                None => return,
                Some(')') => {
                    self.span(self.pos..self.pos + 1, Kind::AssignArrayBracket);
                    self.pos += 1;
                    return;
                }
                Some(c) if ends_word(c) => {
                    self.pos += c.len_utf8();
                }
                Some(_) => {
                    let first = self.spans.len();
                    let word = self.word(false);
                    if let Some(kind) = self.argument_kind(&word) {
                        self.spans.insert(first, Span { range: word.range, kind });
                    }
                }
            }
        }
    }

    /// 扫一个词：引号、转义、变量、命令替换和历史展开就地记下颜色。`command` 为真时词在命令名
    /// 的位置上，`NAME=` 后面紧跟的 `(` 留给数组。
    fn word(&mut self, command: bool) -> Word {
        let start = self.pos;
        let mut literal = Some(String::new());
        let mut glob = false;
        let mut quoted_start = false;
        let push = |literal: &mut Option<String>, s: &str| {
            if let Some(literal) = literal {
                literal.push_str(s);
            }
        };
        while let Some(c) = self.peek() {
            let at = self.pos;
            if c == '`' && self.closing == Some('`') {
                break;
            }
            if ends_word(c) {
                // zsh 的 `*(.)`、`foo(|bar)`：紧贴在参数后面的括号是通配的一部分。命令名后面的是
                // 函数定义 `foo() {`，赋值后面的是数组，都不算。
                if c == '(' && at > start && !command {
                    self.balanced_parens();
                    glob = true;
                    continue;
                }
                break;
            }
            match c {
                '\\' => {
                    quoted_start |= at == start;
                    self.pos += 1;
                    if let Some(next) = self.bump() {
                        push(&mut literal, &next.to_string());
                    }
                }
                '\'' => {
                    quoted_start |= at == start;
                    self.pos += 1;
                    let len = self.rest().find('\'').map_or(self.rest().len(), |i| i + 1);
                    let content = &self.text[self.pos..self.pos + len];
                    push(&mut literal, content.strip_suffix('\'').unwrap_or(content));
                    self.pos += len;
                    self.span(at..self.pos, Kind::SingleQuotedArgument);
                }
                '"' => {
                    quoted_start |= at == start;
                    self.double_quoted(&mut literal);
                }
                '$' if self.peek_at(1) == Some('\'') => {
                    quoted_start |= at == start;
                    self.dollar_quoted();
                    literal = None;
                }
                '$' => {
                    let before = self.pos;
                    self.dollar();
                    if self.pos == before + 1 {
                        push(&mut literal, "$");
                    } else {
                        literal = None;
                    }
                }
                '`' => {
                    self.backquoted();
                    literal = None;
                }
                '!' if self.history_expansion() => literal = None,
                '*' | '?' | '[' => {
                    // `[`、`[[` 单独成词时是命令，不算通配。
                    glob |= c != '[' || self.rest()[1..].contains(']');
                    self.pos += 1;
                    push(&mut literal, &c.to_string());
                }
                _ => {
                    self.pos += c.len_utf8();
                    push(&mut literal, &c.to_string());
                }
            }
        }
        let raw = &self.text[start..self.pos];
        if raw == "[[" || raw == "]]" || raw == "[" || raw == "]" {
            glob = false;
        }
        Word { range: start..self.pos, literal, glob, quoted_start }
    }

    /// 吃掉从 `(` 起配对的一组括号，不上色。
    fn balanced_parens(&mut self) {
        let mut depth = 0usize;
        while let Some(c) = self.bump() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                '\\' => {
                    self.bump();
                }
                _ => {}
            }
        }
    }
}
