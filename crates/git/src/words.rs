//! 一对改过的行里具体改了哪些字：两边切成词，求最长公共子序列，没对上的就是改了的。

use std::{borrow::Cow, ops::Range};

/// 两边中间不同的那段按词数相乘超过这么多时不细比，整段算改了。
const MAX_CELLS: usize = 250_000;

/// `old` 改成 `new` 时两边各自改了的字节范围，按先后排、不重叠。连续的字母数字和 `_`
/// 算一个词，一串空白算一个，别的字（标点、汉字）一个字算一个；制表符当四个空格比，diff 里展开过的
/// 一边和原样的一边也认得出一样。两边相同的部分不到长的那行的四分之一时当整行重写，两边都不标。
pub fn changed_words(old: &str, new: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let (a, b) = (tokens(old), tokens(new));
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x.1 == y.1).count();
    let suffix = a[prefix..].iter().rev().zip(b[prefix..].iter().rev()).take_while(|(x, y)| x.1 == y.1).count();
    let (mid_a, mid_b) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    let (mut keep_a, mut keep_b) = (vec![false; mid_a.len()], vec![false; mid_b.len()]);
    let (n, m) = (mid_a.len(), mid_b.len());
    if n * m <= MAX_CELLS {
        // lcs[i * (m + 1) + j] 是 mid_a[i..] 和 mid_b[j..] 的最长公共子序列有多长。
        let mut lcs = vec![0u32; (n + 1) * (m + 1)];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                lcs[i * (m + 1) + j] = if mid_a[i].1 == mid_b[j].1 {
                    lcs[(i + 1) * (m + 1) + j + 1] + 1
                } else {
                    lcs[(i + 1) * (m + 1) + j].max(lcs[i * (m + 1) + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n && j < m {
            if mid_a[i].1 == mid_b[j].1 {
                (keep_a[i], keep_b[j]) = (true, true);
                (i, j) = (i + 1, j + 1);
            } else if lcs[(i + 1) * (m + 1) + j] >= lcs[i * (m + 1) + j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }
    }
    // 长度按展开制表符后的样子算，一边展开过、一边原样时结果才一样。
    let len = |tokens: &[(Range<usize>, Cow<str>)]| tokens.iter().map(|t| t.1.len()).sum::<usize>();
    let kept = len(&a[..prefix])
        + len(&a[a.len() - suffix..])
        + mid_a.iter().zip(&keep_a).filter(|(_, keep)| **keep).map(|(t, _)| t.1.len()).sum::<usize>();
    if kept * 4 < len(&a).max(len(&b)) {
        return (Vec::new(), Vec::new());
    }
    (ranges(old, mid_a, &keep_a), ranges(new, mid_b, &keep_b))
}

/// 没留下的词并成连续的范围，中间只隔着空白的也并起来，免得一处改动被空格切成几截。
fn ranges(text: &str, tokens: &[(Range<usize>, Cow<str>)], keep: &[bool]) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for ((range, _), _) in tokens.iter().zip(keep).filter(|(_, keep)| !**keep) {
        match out.last_mut() {
            Some(last) if text[last.end..range.start].trim().is_empty() => last.end = range.end,
            _ => out.push(range.clone()),
        }
    }
    out
}

/// 切成词：每个词的字节范围和拿来比的样子（制表符展开成四个空格）。
fn tokens(text: &str) -> Vec<(Range<usize>, Cow<'_, str>)> {
    // 汉字这类三字节以上的字虽算字母，一个字算一个词，不然一句话只是一个词。
    let class = |ch: char| match ch {
        _ if ch.is_whitespace() => 1,
        _ if ch == '_' || (ch.is_alphanumeric() && ch.len_utf8() < 3) => 2,
        _ => 0,
    };
    let mut out = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((start, ch)) = chars.next() {
        let kind = class(ch);
        let mut end = start + ch.len_utf8();
        while kind != 0
            && let Some(&(at, next)) = chars.peek().filter(|&&(_, next)| class(next) == kind)
        {
            end = at + next.len_utf8();
            chars.next();
        }
        let token = &text[start..end];
        let key = if token.contains('\t') { Cow::Owned(token.replace('\t', "    ")) } else { Cow::Borrowed(token) };
        out.push((start..end, key));
    }
    out
}
