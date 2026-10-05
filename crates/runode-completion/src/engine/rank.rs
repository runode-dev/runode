//! 候选的匹配和排序：按当前词筛出、排好候选，算出要高亮的字和公共开头。

use std::collections::HashSet;

use super::{Candidate, Kind};

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

#[cfg(test)]
mod tests {
    use super::*;
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
}
