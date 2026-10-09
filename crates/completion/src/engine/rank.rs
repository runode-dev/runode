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
pub(crate) fn tier(name: &str, query: &str) -> Option<u8> {
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
