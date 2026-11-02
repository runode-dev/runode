//! 一对改过的行里具体改了哪些字。

use runode_git::changed_words;

fn changed<'a>(text: &'a str, ranges: &[std::ops::Range<usize>]) -> Vec<&'a str> {
    ranges.iter().map(|range| &text[range.clone()]).collect()
}

#[test]
fn marks_only_the_changed_words() {
    let (old, new) = ("let total = count + 1;", "let total = count * 2;");
    let (a, b) = changed_words(old, new);
    assert_eq!(changed(old, &a), ["+ 1"]);
    assert_eq!(changed(new, &b), ["* 2"]);
}

#[test]
fn chinese_is_compared_char_by_char() {
    let (old, new) = ("改动现在只有一处：实现。", "改动现在有两处：实现，再关掉。");
    let (a, b) = changed_words(old, new);
    assert_eq!(changed(old, &a), ["只", "一"]);
    assert_eq!(changed(new, &b), ["两", "，再关掉"]);
}

#[test]
fn insertion_marks_only_the_new_side() {
    let (a, b) = changed_words("foo(bar)", "foo(bar, baz)");
    assert!(a.is_empty());
    assert_eq!(changed("foo(bar, baz)", &b), [", baz"]);
}

#[test]
fn tabs_are_compared_as_written() {
    let (a, b) = changed_words("\tx = 1", "\tx = 2");
    assert_eq!(changed("\tx = 1", &a), ["1"]);
    assert_eq!(changed("\tx = 2", &b), ["2"]);
    // 缩进从空格换成制表符算改了。
    let (a, b) = changed_words("    let x = 1;", "\tlet x = 1;");
    assert_eq!(changed("    let x = 1;", &a), ["    "]);
    assert_eq!(changed("\tlet x = 1;", &b), ["\t"]);
}

#[test]
fn rewritten_lines_are_not_marked() {
    assert_eq!(changed_words("fn alpha() {}", "let beta = gamma;"), (Vec::new(), Vec::new()));
}
