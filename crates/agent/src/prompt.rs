//! agent 在屏幕底部问的带编号的问题：上面几行问的是什么，下面一列选项。选一项发的是它的编号键，
//! 认错了的后果和用户自己按一个数字键一样。屏幕文字由调用方读好了交进来，一行一个，用 `\n` 分开。

use std::ops::Range;

/// 认出来的提问。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Prompt {
    /// 选项上面有字的几行，从上往下，去掉了行首的空白。可能为空。
    pub question: Vec<String>,
    /// 从 1 起连续编号，至少两个。
    pub options: Vec<PromptOption>,
}

/// 提问里的一个选项。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PromptOption {
    /// 选这一项要按的数字键。
    pub number: u8,
    /// 屏幕上的原文：折到下一行的接上了，末尾的按键提示（`(esc)` 这种短括号）去掉了。
    pub text: String,
    /// 带着光标记号（`MARKERS`）的那一项，回车选的就是它。
    pub selected: bool,
}

/// 每个选项都只要按一个数字键。
pub const MAX_OPTIONS: u8 = 9;
/// 提问的选项或 agent 的输入框下面最多容几行别的字（按键提示、agent 的状态栏）；再多就不算贴着屏幕
/// 底部，多半是 agent 输出里的一段普通编号列表或画在输出里的框。
pub const MAX_TRAILING_LINES: usize = 2;
/// 问题最多取几行：要带得出 Claude 权限框里标题下面的命令本身。
pub const MAX_QUESTION_LINES: usize = 4;
/// 选项末尾的括号里不超过这么多字时当作按键提示去掉。
pub const MAX_HINT_LENGTH: usize = 6;

/// 选项行开头表示光标停在这一项的记号。
const MARKERS: [char; 5] = ['❯', '›', '>', '▶', '●'];
/// 框两边的竖线。
const BORDERS: [char; 3] = ['│', '┃', '║'];

/// 从屏幕文字里认出提问。认法：从下往上，跳过空行、制表线和至多 `MAX_TRAILING_LINES` 行别的字，
/// 找到一块编号从 1 起连续、至少两项的选项；选项之间可以夹空行和制表线，缩进到选项正文那一列的下一行
/// 算折行，接到上一项后面。选项块上面至多 `MAX_QUESTION_LINES` 行有字的行是问题，遇到制表线或别的
/// 选项为止。行首行尾的框线（`BORDERS`）先去掉。选项超过 `MAX_OPTIONS` 个时认不出。
pub fn parse(screen: &str) -> Option<Prompt> {
    let entries = merge(screen.split('\n').map(Entry::new));
    let mut index = entries.len();
    let mut trailing = 0;
    let last = loop {
        index = index.checked_sub(1)?;
        match &entries[index] {
            Entry::Option(line) => break line.number,
            Entry::Text { .. } => {
                trailing += 1;
                if trailing > MAX_TRAILING_LINES {
                    return None;
                }
            }
            Entry::Blank | Entry::Rule => {}
        }
    };
    if !(2..=MAX_OPTIONS).contains(&last) {
        return None;
    }

    // `index` 指着还没看的那一行的下一行：走到 0 就是看完了。
    let mut index = index + 1;
    let mut options = Vec::new();
    let mut expected = last;
    while expected > 0 {
        index = index.checked_sub(1)?;
        match &entries[index] {
            Entry::Blank | Entry::Rule => {}
            Entry::Text { .. } => return None,
            Entry::Option(line) => {
                if line.number != expected {
                    return None;
                }
                options.push(PromptOption {
                    number: line.number,
                    text: strip_hint(&line.text),
                    selected: line.selected,
                });
                expected -= 1;
            }
        }
    }
    options.reverse();

    let mut question = Vec::new();
    while index > 0 && question.len() < MAX_QUESTION_LINES {
        index -= 1;
        match &entries[index] {
            Entry::Blank => {}
            Entry::Rule | Entry::Option(_) => break,
            Entry::Text { text, .. } => question.push(text.clone()),
        }
    }
    question.reverse();
    Some(Prompt { question, options })
}

/// 屏幕底部最后 `lines` 行有字的输出，从上往下，去掉了框线和两头的空白：跳过空行和制表线；底部有
/// agent 的输入框（`╭` 起、`╰` 止，下面至多 `MAX_TRAILING_LINES` 行别的字）时只取框上面的行。
/// 认框靠的是字形，agent 换了画法就可能取到输入框或状态栏。
pub fn excerpt(screen: &str, lines: usize) -> Vec<String> {
    let rows: Vec<&str> = screen.split('\n').collect();
    let end = input_box_top(&rows).unwrap_or(rows.len());
    let mut picked: Vec<String> = rows[..end]
        .iter()
        .rev()
        .filter_map(|row| match Entry::new(row) {
            Entry::Blank | Entry::Rule => None,
            Entry::Text { .. } | Entry::Option(_) => Some(unboxed(row).trim().to_owned()),
        })
        .take(lines)
        .collect();
    picked.reverse();
    picked
}

/// 底部输入框上沿那一行的下标。
fn input_box_top(rows: &[&str]) -> Option<usize> {
    let mut trailing = 0;
    let mut bottom = None;
    for (index, row) in rows.iter().enumerate().rev() {
        match Entry::new(row) {
            Entry::Blank => {}
            Entry::Rule if edge(row) == Some('╰') => {
                bottom = Some(index);
                break;
            }
            Entry::Rule | Entry::Text { .. } | Entry::Option(_) => {
                trailing += 1;
                if trailing > MAX_TRAILING_LINES {
                    return None;
                }
            }
        }
    }
    rows[..bottom?].iter().rposition(|row| edge(row) == Some('╭'))
}

/// 制表线去掉行首空白后的第一个字。
fn edge(row: &str) -> Option<char> {
    row.trim_start().chars().next()
}

/// 屏幕上的一行。
enum Entry {
    Blank,
    /// 只有制表符（分隔线、框的上下沿）。
    Rule,
    /// 别的字，带着去掉的行首空白有几个字。
    Text {
        text: String,
        indent: usize,
    },
    Option(OptionLine),
}

impl Entry {
    fn new(raw: &str) -> Self {
        let line = unboxed(raw);
        if line.chars().all(char::is_whitespace) {
            Entry::Blank
        } else if line.chars().all(|c| c.is_whitespace() || is_box_drawing(c)) {
            Entry::Rule
        } else if let Some(option) = OptionLine::new(line) {
            Entry::Option(option)
        } else {
            let rest = line.trim_start();
            let indent = line[..line.len() - rest.len()].chars().count();
            Entry::Text { text: rest.to_owned(), indent }
        }
    }
}

fn is_box_drawing(c: char) -> bool {
    ('\u{2500}'..='\u{257F}').contains(&c)
}

fn unboxed(raw: &str) -> &str {
    &raw[unboxed_range(raw)]
}

/// 去掉行尾空白和框两边的竖线后剩下的那一段；竖线后面的缩进留着，折行靠它认。
fn unboxed_range(raw: &str) -> Range<usize> {
    let mut line = raw.trim_end();
    if let Some(stripped) = line.strip_suffix(BORDERS) {
        line = stripped.trim_end();
    }
    let rest = line.trim_start();
    let start = match rest.strip_prefix(BORDERS) {
        Some(inner) => line.len() - inner.len(),
        None => 0,
    };
    start..line.len()
}

/// 一行选项：行首空白，可选的光标记号，一两位数字，`.` 或 `)`，空白，正文。
struct OptionLine {
    number: u8,
    text: String,
    selected: bool,
    /// 正文从第几个字开始，折行的缩进要到这里。
    text_column: usize,
}

impl OptionLine {
    fn new(line: &str) -> Option<Self> {
        let mut rest = line.trim_start();
        let marker = rest.chars().next().filter(|c| MARKERS.contains(c));
        if let Some(marker) = marker {
            rest = rest[marker.len_utf8()..].trim_start();
        }
        let selected = marker.is_some();
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if !(1..=2).contains(&digits) {
            return None;
        }
        let number = rest[..digits].parse().ok()?;
        rest = rest[digits..].strip_prefix(['.', ')'])?;
        let text = rest.trim_start();
        if text.len() == rest.len() || text.is_empty() {
            return None;
        }
        let text_column = line.chars().count() - text.chars().count();
        Some(OptionLine { number, text: text.to_owned(), selected, text_column })
    }
}

/// 选项下面紧跟着、缩进到正文那一列的字是这一项折下来的，接上去。
fn merge(entries: impl Iterator<Item = Entry>) -> Vec<Entry> {
    let mut merged: Vec<Entry> = Vec::new();
    for entry in entries {
        if let (Entry::Text { text, indent }, Some(Entry::Option(option))) = (&entry, merged.last_mut())
            && *indent >= option.text_column
        {
            option.text.push(' ');
            option.text.push_str(text);
            continue;
        }
        merged.push(entry);
    }
    merged
}

/// 去掉末尾 `(esc)`、`(y)` 这种短括号；去掉后没字了就留着。
fn strip_hint(text: &str) -> String {
    let hint = text.strip_suffix(')').and_then(|body| body.rfind('(').map(|open| (open, &body[open + 1..])));
    match hint {
        Some((open, inside)) if inside.chars().count() <= MAX_HINT_LENGTH => {
            let kept = text[..open].trim();
            if kept.is_empty() { text.to_owned() } else { kept.to_owned() }
        }
        _ => text.to_owned(),
    }
}
