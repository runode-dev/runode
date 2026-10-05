//! 候选的高亮、排序和取舍：前缀和分散匹配的高亮，先完全匹配、再前缀、再按顺序含有，同分时
//! 按用过的次数、命令名长短和名字排，选项按去掉横线的名字排，以及决定性的候选和公共开头。

use runode_completion::{Candidate, Kind, common_prefix, decisive, highlight, rank};
use warp_command_signatures::Priority;

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
