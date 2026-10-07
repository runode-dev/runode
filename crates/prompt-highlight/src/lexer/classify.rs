//! 命令名和参数位置上的词是什么：查 shell 报告的名字、PATH、补全用的命令规格和文件系统。

use std::path::PathBuf;

use runode_completion::{is_command, is_executable, resolve_path};

use super::{Expect, Lexer, Word};
use crate::Kind;

/// 前置命令：后面接着另一条命令。和 fast-syntax-highlighting 认的一样，外加 `-`（zsh 的
/// `- cmd`）。
const PRECOMMANDS: &[&str] =
    &["-", "builtin", "command", "exec", "nocorrect", "noglob", "pkexec", "sudo", "doas", "nohup", "xargs"];

/// 前置命令后面跟着值的选项。
fn precommand_values(name: &str) -> &'static [&'static str] {
    match name {
        "sudo" => &["-u", "-g", "-C", "-D", "-p", "-R", "-r", "-t", "-T", "-U"],
        "doas" => &["-u", "-C"],
        "exec" => &["-a"],
        "xargs" => &["-I", "-n", "-P", "-L", "-s", "-E", "-d", "-a", "-J", "-R", "-S"],
        _ => &[],
    }
}

/// shell 没报告关键字时（fish）用的关键字。
const FALLBACK_KEYWORDS: &[&str] =
    &["if", "else", "end", "for", "in", "while", "begin", "function", "switch", "case", "and", "or", "not"];

impl Lexer<'_> {
    /// 命令名位置上的词是什么，以及之后的词在什么位置上。
    pub(super) fn command_name(&self, literal: Option<&str>) -> (Option<Kind>, Expect) {
        // 带变量的命令名不知道会展开成什么。
        let Some(name) = literal else {
            return (None, Expect::args(None));
        };
        let names = &self.shell.names;
        let has = |list: &[String]| list.iter().any(|n| n == name);
        if PRECOMMANDS.contains(&name) {
            return (
                Some(Kind::Precommand),
                Expect::PrecommandOptions { values: precommand_values(name), value_next: false },
            );
        }
        let keyword = if names.keywords.is_empty() { FALLBACK_KEYWORDS.contains(&name) } else { has(&names.keywords) };
        if keyword {
            let next = match name {
                "for" | "select" | "foreach" => Expect::ForVar,
                "case" | "switch" => Expect::CaseSubject,
                "function" => Expect::FunctionName,
                "[[" => Expect::args(Some("[[".to_owned())),
                "fi" | "done" | "esac" | "}" | "end" => Expect::args(None),
                _ => Expect::Command,
            };
            let kind = if name == "[[" { Kind::DoubleSqBracket } else { Kind::ReservedWord };
            return (Some(kind), next);
        }
        if has(&names.aliases) {
            // 子命令按别名展开后的命令查，比如 `g=git` 时 `g status`。
            let target = names
                .alias_values
                .iter()
                .find(|(n, _)| n == name)
                .and_then(|(_, value)| value.split_whitespace().next())
                .map(str::to_owned);
            return (Some(Kind::Alias), Expect::args(target));
        }
        if has(&names.builtins) {
            let kind = if name == "[" { Kind::SingleSqBracket } else { Kind::Builtin };
            return (Some(kind), Expect::args(Some(name.to_owned())));
        }
        if has(&names.functions) {
            return (Some(Kind::Function), Expect::args(Some(name.to_owned())));
        }
        let args = Expect::args(Some(name.to_owned()));
        if name.contains('/') {
            let kind = match self.resolve(name) {
                Some(path) if path.is_dir() => Some(Kind::PathToDir),
                Some(path) if is_executable(&path) => Some(Kind::Command),
                Some(_) => Some(Kind::UnknownToken),
                // 没有当前目录，相对路径查不了。
                None => None,
            };
            return (kind, args);
        }
        let path = self.shell.path.clone().or_else(|| std::env::var_os("PATH")).unwrap_or_default();
        if is_command(&path, name) {
            return (Some(Kind::Command), args);
        }
        // shell 没报告过名字时分不清是不是别名或函数，不标成找不到。
        let reported = !names.builtins.is_empty();
        (reported.then_some(Kind::UnknownToken), args)
    }

    /// 参数位置上的词：通配、存在的文件或目录，其余不上色。
    pub(super) fn argument_kind(&self, word: &Word) -> Option<Kind> {
        if word.glob {
            return Some(Kind::Globbing);
        }
        self.path_kind(word)
    }

    /// 词写的是存在的文件或目录时是哪一种。
    pub(super) fn path_kind(&self, word: &Word) -> Option<Kind> {
        let literal = word.literal.as_deref().filter(|literal| !literal.is_empty())?;
        let path = self.resolve(literal)?;
        let meta = std::fs::metadata(path).ok()?;
        Some(if meta.is_dir() { Kind::PathToDir } else { Kind::Path })
    }

    /// 词里写的路径对应的实际路径：相对路径从 shell 所在目录算，`~` 开头的从主目录算。
    fn resolve(&self, literal: &str) -> Option<PathBuf> {
        resolve_path(literal, self.cwd, self.home.as_deref())
    }
}
