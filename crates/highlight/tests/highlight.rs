//! 提示符上输入的语法高亮：命令名、关键字、选项、字符串、变量、路径和子命令各上什么颜色。

use std::{
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
};

use runode_highlight::{Kind, Shell, highlight};
use runode_shared_types::shell::ShellNames;

/// 测试用的临时目录：`bin` 里放几个可执行文件当 PATH，另有一个文件和一个子目录。用完删掉。
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "runode-syntax-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("file.txt"), "").unwrap();
        for name in ["ls", "cat", "git", "tool"] {
            let path = dir.join("bin").join(name);
            std::fs::write(&path, "").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn shell(&self) -> Shell {
        let words = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect();
        Shell {
            path: Some(self.0.join("bin").into_os_string()),
            names: ShellNames {
                aliases: words(&["g"]),
                alias_values: vec![("g".into(), "git --no-pager".into())],
                functions: words(&["myfn"]),
                builtins: words(&["cd", "echo", "[", "export"]),
                keywords: words(&[
                    "if", "then", "else", "fi", "for", "in", "do", "done", "case", "esac", "[[", "]]", "{", "}", "!",
                    "function", "while",
                ]),
            },
            usage: Default::default(),
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 按叠好的颜色把输入切成段：每段是一串颜色相同的字，没上色的不列。
fn painted(text: &str, shell: &Shell, cwd: Option<&Path>) -> Vec<(String, Kind)> {
    let mut kinds: Vec<Option<Kind>> = vec![None; text.len()];
    for span in highlight(text, shell, cwd) {
        for kind in &mut kinds[span.range] {
            *kind = Some(span.kind);
        }
    }
    let mut out: Vec<(String, Kind)> = Vec::new();
    let mut last = None;
    for (i, c) in text.char_indices() {
        let kind = kinds[i];
        match (kind, out.last_mut()) {
            (Some(kind), Some((s, prev))) if last == Some(kind) && *prev == kind => s.push(c),
            (Some(kind), _) => out.push((c.to_string(), kind)),
            (None, _) => {}
        }
        last = kind;
    }
    out
}

fn check(text: &str, expected: &[(&str, Kind)]) {
    let scratch = Scratch::new();
    let got = painted(text, &scratch.shell(), Some(scratch.path()));
    let expected: Vec<(String, Kind)> = expected.iter().map(|(s, k)| ((*s).to_owned(), *k)).collect();
    assert_eq!(got, expected, "highlighting {text:?}");
}

#[test]
fn commands_options_and_strings() {
    check(
        r#"ls -la --color "a $HOME b" 'c' $'d\n'"#,
        &[
            ("ls", Kind::Command),
            ("-la", Kind::SingleHyphenOption),
            ("--color", Kind::DoubleHyphenOption),
            ("\"a ", Kind::DoubleQuotedArgument),
            ("$HOME", Kind::BackOrDollarDoubleQuotedArgument),
            (" b\"", Kind::DoubleQuotedArgument),
            ("'c'", Kind::SingleQuotedArgument),
            ("$'d", Kind::DollarQuotedArgument),
            ("\\n", Kind::BackDollarQuotedArgument),
            ("'", Kind::DollarQuotedArgument),
        ],
    );
}

#[test]
fn every_command_in_a_pipeline_is_looked_up() {
    check(
        "FOO=1 ls | tool && nope x; echo $x; myfn",
        &[
            ("ls", Kind::Command),
            ("tool", Kind::Command),
            ("nope", Kind::UnknownToken),
            ("echo", Kind::Builtin),
            ("$x", Kind::Variable),
            ("myfn", Kind::Function),
        ],
    );
}

#[test]
fn unknown_commands_are_not_marked_before_the_shell_reports_its_names() {
    let scratch = Scratch::new();
    let shell = Shell { names: ShellNames::default(), ..scratch.shell() };
    assert_eq!(painted("nope", &shell, Some(scratch.path())), []);
}

#[test]
fn subcommands_come_from_the_command_spec_and_aliases() {
    check(
        "git stash push -m x",
        &[
            ("git", Kind::Command),
            ("stash", Kind::Subcommand),
            ("push", Kind::Subcommand),
            ("-m", Kind::SingleHyphenOption),
        ],
    );
    check("g status", &[("g", Kind::Alias), ("status", Kind::Subcommand)]);
    // 第一个参数不是子命令时，后面的也不是。
    check("git nothing status", &[("git", Kind::Command)]);
}

#[test]
fn precommands_take_options_before_the_command() {
    check("sudo -u root ls", &[("sudo", Kind::Precommand), ("-u", Kind::SingleHyphenOption), ("ls", Kind::Command)]);
}

#[test]
fn keywords_and_test_brackets() {
    check(
        "if [[ -f x ]]; then echo; fi",
        &[
            ("if", Kind::ReservedWord),
            ("[[", Kind::DoubleSqBracket),
            ("-f", Kind::SingleHyphenOption),
            ("]]", Kind::DoubleSqBracket),
            ("then", Kind::ReservedWord),
            ("echo", Kind::Builtin),
            ("fi", Kind::ReservedWord),
        ],
    );
    check(
        "for i in 1 2; do echo $i; done",
        &[
            ("for", Kind::ReservedWord),
            ("in", Kind::ReservedWord),
            ("do", Kind::ReservedWord),
            ("echo", Kind::Builtin),
            ("$i", Kind::Variable),
            ("done", Kind::ReservedWord),
        ],
    );
    check(
        "case $x in a|b) echo;; esac",
        &[
            ("case", Kind::ReservedWord),
            ("$x", Kind::Variable),
            ("in", Kind::ReservedWord),
            ("echo", Kind::Builtin),
            ("esac", Kind::ReservedWord),
        ],
    );
}

#[test]
fn paths_and_globs() {
    check(
        "cat file.txt sub missing *.rs > file.txt",
        &[
            ("cat", Kind::Command),
            ("file.txt", Kind::Path),
            ("sub", Kind::PathToDir),
            ("*.rs", Kind::Globbing),
            ("file.txt", Kind::Path),
        ],
    );
    // 写成路径的命令名：可执行文件是命令，不存在的找不到。
    check("bin/tool; ./nope", &[("bin/tool", Kind::Command), ("./nope", Kind::UnknownToken)]);
}

#[test]
fn substitutions_are_highlighted_inside() {
    check(
        "echo $(ls $(nope)) `tool`",
        &[
            ("echo", Kind::Builtin),
            ("$(", Kind::BracketLevel(1)),
            ("ls", Kind::Command),
            ("$(", Kind::BracketLevel(2)),
            ("nope", Kind::UnknownToken),
            (")", Kind::BracketLevel(2)),
            (")", Kind::BracketLevel(1)),
            ("tool", Kind::Command),
        ],
    );
    check(
        "echo \"$(ls)\"",
        &[
            ("echo", Kind::Builtin),
            ("\"", Kind::DoubleQuotedArgument),
            ("$(", Kind::BracketLevel(1)),
            ("ls", Kind::Command),
            (")", Kind::BracketLevel(1)),
            ("\"", Kind::DoubleQuotedArgument),
        ],
    );
}

#[test]
fn history_comments_here_strings_and_math() {
    check("echo !! # note", &[("echo", Kind::Builtin), ("!!", Kind::HistoryExpansion), ("# note", Kind::Comment)]);
    check(
        "cat <<< 'x' 2>&1",
        &[("cat", Kind::Command), ("<<<", Kind::HereStringTri), ("'x'", Kind::SingleQuotedArgument)],
    );
    check(
        "((i += 2))",
        &[("((", Kind::ReservedWord), ("i", Kind::MathVar), ("2", Kind::MathNum), ("))", Kind::ReservedWord)],
    );
    check(
        "a=(x 'y') ls",
        &[
            ("(", Kind::AssignArrayBracket),
            ("'y'", Kind::SingleQuotedArgument),
            (")", Kind::AssignArrayBracket),
            ("ls", Kind::Command),
        ],
    );
}

#[test]
fn unfinished_input_does_not_panic() {
    let scratch = Scratch::new();
    let shell = scratch.shell();
    for text in ["echo \"abc", "echo 'abc", "echo $(ls", "echo ${x", "((1 +", "echo `ls", "a=(1", "ls >", "echo \\"] {
        highlight(text, &shell, Some(scratch.path()));
    }
}

#[test]
fn styles_follow_the_default_theme() {
    let style = Kind::UnknownToken.style();
    assert_eq!((style.color, style.bold), (1, true));
    assert_eq!(Kind::Command.style().color, 2);
    assert!(Kind::PathToDir.style().underline);
    assert_eq!(Kind::Variable.style().color, 113);
    assert_eq!(Kind::BracketLevel(4).style(), Kind::BracketLevel(1).style());
}
