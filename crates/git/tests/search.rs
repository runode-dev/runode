mod common;

use std::{path::PathBuf, sync::atomic::AtomicBool};

use common::TestRepo;
use runode_git::{GrepMatch, GrepQuery, grep, list_files};

fn literal(pattern: &str, ignore_case: bool) -> GrepQuery<'_> {
    GrepQuery { pattern, ignore_case, ..GrepQuery::default() }
}

#[test]
fn lists_tracked_and_untracked_files_but_not_ignored_ones() {
    let repo = TestRepo::new("search-list");
    repo.commit_file(".gitignore", "build/\n", "init");
    repo.commit_file("gone.txt", "x", "add");
    std::fs::remove_file(repo.path().join("gone.txt")).unwrap();
    repo.write("src/main.rs", "fn main() {}\n");
    repo.write("build/out.o", "x");
    let mut files = list_files(repo.path()).unwrap();
    files.sort();
    assert_eq!(files, [PathBuf::from(".gitignore"), PathBuf::from("src/main.rs")]);
    assert!(list_files(&std::env::temp_dir()).is_none());
}

#[test]
fn greps_literal_text_and_stops_at_the_limit() {
    let repo = TestRepo::new("search-grep");
    repo.commit_file(".gitignore", "build/\n", "init");
    repo.write("a.txt", "Hello (world)\r\nbye\nhello again\n");
    repo.write("build/b.txt", "hello\n");
    let hit = |path: &str, line, text: &str| GrepMatch { path: path.into(), line, text: text.into() };
    // 括号按字面找，不当正则；被忽略的目录不找；CRLF 行尾的 `\r` 去掉。
    assert_eq!(
        grep(repo.path(), &literal("(world)", false), 10, &AtomicBool::new(false)),
        [hit("a.txt", 1, "Hello (world)")]
    );
    assert_eq!(
        grep(repo.path(), &literal("hello", false), 10, &AtomicBool::new(false)),
        [hit("a.txt", 3, "hello again")]
    );
    assert_eq!(grep(repo.path(), &literal("hello", true), 10, &AtomicBool::new(false)).len(), 2);
    assert_eq!(grep(repo.path(), &literal("hello", true), 1, &AtomicBool::new(false)).len(), 1);
    assert!(grep(repo.path(), &literal("", true), 10, &AtomicBool::new(false)).is_empty());
    // 取消了的立刻返回。
    assert!(grep(repo.path(), &literal("hello", true), 10, &AtomicBool::new(true)).len() <= 2);
}

#[test]
fn greps_a_directory_outside_any_repo() {
    let repo = TestRepo::new("search-plain");
    repo.write("note.md", "needle\n");
    std::fs::remove_dir_all(repo.path().join(".git")).unwrap();
    assert_eq!(grep(repo.path(), &literal("needle", false), 10, &AtomicBool::new(false)).len(), 1);
}

#[test]
fn greps_whole_words_regexes_and_pathspecs() {
    let repo = TestRepo::new("search-options");
    repo.write("src/a.ts", "foo foobar\nfoo1\n");
    repo.write("dist/b.min.js", "foo\n");
    let never = AtomicBool::new(false);
    let lines = |query: &GrepQuery| -> Vec<_> {
        grep(repo.path(), query, 10, &never)
            .into_iter()
            .map(|hit| format!("{}:{}", hit.path.display(), hit.line))
            .collect()
    };
    let word = GrepQuery { pattern: "foo", whole_word: true, ..GrepQuery::default() };
    assert_eq!(lines(&word), ["dist/b.min.js:1", "src/a.ts:1"]);
    let regex = GrepQuery { pattern: "foo[0-9]", regex: true, ..GrepQuery::default() };
    assert_eq!(lines(&regex), ["src/a.ts:2"]);
    let specs = [":(glob)**/*.ts".to_owned()];
    assert_eq!(lines(&GrepQuery { pathspecs: &specs, ..word }), ["src/a.ts:1"]);
    let specs = [":(exclude,glob)**/*.min.js".to_owned()];
    assert_eq!(lines(&GrepQuery { pathspecs: &specs, ..word }), ["src/a.ts:1"]);
}

#[test]
fn caps_very_long_lines() {
    let repo = TestRepo::new("search-long");
    // 压缩过的 js 那样一行很长；读的时候截短，后面的行照常对得上。
    repo.write("app.min.js", &format!("needle{}\nneedle short\n", "x".repeat(1024 * 1024)));
    let hits = grep(repo.path(), &literal("needle", false), 10, &AtomicBool::new(false));
    assert_eq!(hits.len(), 2);
    assert!(hits[0].text.starts_with("needlexxx") && hits[0].text.len() <= 64 * 1024, "{}", hits[0].text.len());
    assert_eq!((hits[1].line, hits[1].text.as_str()), (2, "needle short"));
}
