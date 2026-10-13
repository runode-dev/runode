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
fn expanded_tabs_match_raw_tabs() {
    let (a, b) = changed_words("    x = 1", "\tx = 2");
    assert_eq!(changed("    x = 1", &a), ["1"]);
    assert_eq!(changed("\tx = 2", &b), ["2"]);
}

#[test]
fn tabs_count_as_expanded_for_the_rewrite_cutoff() {
    // 相同的缩进按四格算，占了长的那行的四分之一以上，两种写法、两个方向都照常标。
    for (old, new) in [("\tfoo", "    bar"), ("    bar", "\tfoo"), ("    foo", "    bar")] {
        let (a, b) = changed_words(old, new);
        assert_eq!((a.len(), b.len()), (1, 1), "{old:?} -> {new:?}");
    }
}

#[test]
fn rewritten_lines_are_not_marked() {
    assert_eq!(changed_words("fn alpha() {}", "let beta = gamma;"), (Vec::new(), Vec::new()));
}
