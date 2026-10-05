//! 语法高亮的公开接口：认得出要求覆盖的语言、按第一行认语言，认不出或取消时不给结果。

use std::{path::Path, sync::atomic::AtomicBool};

use runode_preview::{Color, highlight, syntax_name};

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

fn colors(path: &str, text: &str) -> Vec<Color> {
    let spans = highlight(Path::new(path), &lines(text), &AtomicBool::new(false))
        .unwrap_or_else(|| panic!("{path}: no syntax"));
    spans.iter().flatten().map(|span| span.style.color).collect()
}

/// 要求覆盖的语言都认得出，而且高亮出了不止一种颜色。
#[test]
fn highlights_the_main_languages() {
    let samples = [
        ("main.rs", "Rust", "// hi\nfn main() { let x = \"s\"; }"),
        ("app.ts", "TypeScript", "// hi\nconst x: number = 1;\nfunction f() { return 'a'; }"),
        ("App.tsx", "TypeScriptReact", "const a = <div className=\"x\">hi</div>;\nexport default function A() {}"),
        ("index.js", "JavaScript", "// hi\nconst x = 'a';\nfunction f() { return 1; }"),
        ("main.py", "Python", "# hi\ndef f():\n    return 'a'"),
        ("main.go", "Go", "// hi\npackage main\nfunc main() { x := \"a\" }"),
        ("data.json", "JSON", "{\"key\": \"value\", \"n\": 1}"),
        ("conf.yaml", "YAML", "# hi\nkey: value\nn: 1"),
        ("Cargo.toml", "TOML", "# hi\n[package]\nname = \"x\""),
        ("README.md", "Markdown", "# Title\n\nSome `code` and **bold**."),
        ("run.sh", "Bourne Again Shell (bash)", "# hi\necho \"$HOME\"\nif true; then exit 1; fi"),
        ("Dockerfile", "Dockerfile", "# hi\nFROM alpine:3\nRUN echo \"a\""),
        ("main.c", "C", "// hi\n#include <stdio.h>\nint main(void) { return 0; }"),
        ("main.cpp", "C++", "// hi\n#include <vector>\nclass A { public: int x; };"),
        ("main.swift", "Swift", "// hi\nlet x = \"a\"\nfunc f() -> Int { return 1 }"),
        ("main.zig", "Zig", "// hi\nconst std = @import(\"std\");\npub fn main() void {}"),
    ];
    for (path, name, text) in samples {
        assert_eq!(syntax_name(Path::new(path), ""), Some(name), "{path}");
        let colors = colors(path, text);
        assert!(!colors.is_empty(), "{path}: nothing highlighted");
        let distinct: std::collections::HashSet<_> = colors.iter().collect();
        assert!(distinct.len() >= 2, "{path}: only {distinct:?}");
    }
}

#[test]
fn finds_syntax_by_first_line() {
    assert_eq!(syntax_name(Path::new("script"), "#!/bin/bash"), Some("Bourne Again Shell (bash)"));
    assert_eq!(syntax_name(Path::new("notes.unknownext"), "plain words"), None);
}

#[test]
fn unknown_language_or_cancel_gives_nothing() {
    assert!(highlight(Path::new("notes.unknownext"), &lines("hello"), &AtomicBool::new(false)).is_none());
    assert!(highlight(Path::new("a.rs"), &lines("fn main() {}"), &AtomicBool::new(true)).is_none());
}
