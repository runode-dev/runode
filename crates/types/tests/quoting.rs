//! 往终端里打路径时照各种 shell 的写法加引号、转义。

use runode_types::{session::SessionMeta, shell::Quoting};

#[test]
fn quotes_paths_for_posix_shells() {
    let quote = |text| Quoting::Posix.quote(text);
    assert_eq!(quote("src/main.rs:12"), "src/main.rs:12");
    assert_eq!(quote("my file.txt"), "'my file.txt'");
    assert_eq!(quote("it's"), r"'it'\''s'");
    assert_eq!(quote("文件.md"), "文件.md");
    assert_eq!(quote("=foo"), "'=foo'");
    assert_eq!(quote("a=b"), "a=b");
    assert_eq!(quote(r"C:\src\a.rs"), r"'C:\src\a.rs'");
}

#[test]
fn quotes_paths_for_windows_shells() {
    assert_eq!(Quoting::PowerShell.quote(r"C:\src\main.rs:12"), r"C:\src\main.rs:12");
    assert_eq!(Quoting::PowerShell.quote(r"C:\me\$(whoami).txt"), r"'C:\me\$(whoami).txt'");
    assert_eq!(Quoting::PowerShell.quote("Bob's ’file’"), "'Bob''s ’’file’’'");
    assert_eq!(Quoting::PowerShell.quote("a,b"), "'a,b'");
    assert_eq!(Quoting::Elvish.quote("it's"), "'it''s'");
    assert_eq!(Quoting::Cmd.quote(r"C:\Users\me\Screen Shot.png"), r#""C:\Users\me\Screen Shot.png""#);
    assert_eq!(Quoting::Cmd.quote("a=b"), r#""a=b""#);
    assert_eq!(Quoting::Nushell.quote(r"C:\my dir"), r"'C:\my dir'");
    assert_eq!(Quoting::Nushell.quote("it's"), "r#'it's'#");
    assert_eq!(Quoting::Nushell.quote("a'#b"), "r##'a'#b'##");
}

#[test]
fn picks_the_quoting_of_the_foreground_shell() {
    let meta = |foreground: &str, foreground_is_shell| SessionMeta {
        foreground: Some(foreground.to_owned()),
        foreground_is_shell,
        ..SessionMeta::default()
    };
    let platform = if cfg!(windows) { Quoting::Cmd } else { Quoting::Posix };
    assert_eq!(Quoting::of_session(&meta("pwsh.exe", true)), Quoting::PowerShell);
    assert_eq!(Quoting::of_session(&meta("bash.exe", true)), Quoting::Posix);
    assert_eq!(Quoting::of_session(&meta("nu", true)), Quoting::Nushell);
    assert_eq!(Quoting::of_session(&meta("cmd.exe", true)), Quoting::Cmd);
    assert_eq!(Quoting::of_session(&meta("pwsh.exe", false)), platform);
    assert_eq!(Quoting::of_program("powershell.exe"), Quoting::PowerShell);
}

#[test]
fn escapes_image_paths_with_backslashes() {
    let escape = |text| Quoting::Posix.escape(text);
    assert_eq!(escape("/tmp/shot.png"), "/tmp/shot.png");
    assert_eq!(escape("/tmp/Screen Shot (1).png"), r"/tmp/Screen\ Shot\ \(1\).png");
    assert_eq!(escape("/tmp/it's.png"), r"/tmp/it\'s.png");
    assert_eq!(escape("/tmp/截图.png"), "/tmp/截图.png");
    assert_eq!(escape("=a=b"), r"\=a=b");
    assert_eq!(Quoting::PowerShell.escape(r"C:\tmp\shot.png"), r"C:\tmp\shot.png");
    assert_eq!(Quoting::Cmd.escape(r"C:\Users\me\it's.png"), r#""C:\Users\me\it's.png""#);
}
