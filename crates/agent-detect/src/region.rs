//! 规则的 `region`：从屏幕底部的文字（或标题、进度报告）里切出一段给规则去匹配。
//!
//! 屏幕文字是一行一行的，行尾空白已经去掉，行与行之间用 `\n` 连接，见
//! `crate::Signals::screen`。切出来的都是原文的一个子串，不复制。
//!
//! 有几种区域认得特定的界面结构：
//! - 输入提示行：以「›」开头的行（`›` 单独一行或后面跟空格）。
//! - 回答块标记行：以「•」「■」「✗」「✓」开头的行。提示行之后又出现了标记行，说明这个提示行
//!   已经是历史，当前没有提示行。
//! - 横线：去掉首尾空白后以「─」开头，要么整行都是「─」，要么至少连着三个「─」再跟别的字
//!   （带标题的分隔线）。最后两条横线围起来的是输入框。

/// 规则里写的区域。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Region {
    /// 整段屏幕文字。
    WholeRecent,
    /// 最后一个提示行之后；没有提示行时是整段。
    AfterLastPromptMarker,
    /// 当前提示行之前；没有当前提示行时是整段。
    BeforeCurrentPromptMarker,
    /// 有当前提示行时为空，没有时是整段。
    WholeRecentWithoutCurrentPromptMarker,
    /// 当前提示行之前最近的那个标记行，只有那一行。
    CurrentPromptBlockMarker,
    /// 从当前提示行之前最近的标记行开始到末尾。
    AfterCurrentPromptBlockMarker,
    /// 输入框里面：最后两条横线中靠上那条之后，到下一条横线之前。
    PromptBoxBody,
    /// 输入框上面的全部内容；没有输入框时是整段。
    AbovePromptBox,
    /// 输入框上面最后一个不空的行。
    LastNonEmptyAbovePromptBox,
    /// 最后一条横线之后；没有横线时是整段。
    AfterLastHorizontalRule,
    /// 程序用 OSC 0/2 设的标题。
    OscTitle,
    /// 程序最近一次 OSC 9 报告里 `9;` 后面的部分，比如 `4;3`。
    OscProgress,
    /// 最后 n 行，空行也算。
    BottomLines(usize),
    /// 从倒数第 n 个不空的行开始到末尾。
    BottomNonEmptyLines(usize),
    /// 从开头到第 n 个不空的行为止。
    TopNonEmptyLines(usize),
}

/// `TopNonEmptyLines` 能写的最大行数。
const MAX_TOP_LINES: usize = u16::MAX as usize;

impl Region {
    /// 解析规则里的写法，前后空白不算；不认得时为 `None`。
    pub(crate) fn parse(spec: &str) -> Option<Self> {
        let spec = spec.trim();
        let region = match spec {
            "whole_recent" => Self::WholeRecent,
            "after_last_prompt_marker" => Self::AfterLastPromptMarker,
            "before_current_prompt_marker" => Self::BeforeCurrentPromptMarker,
            "whole_recent_without_current_prompt_marker" => Self::WholeRecentWithoutCurrentPromptMarker,
            "current_prompt_block_marker" => Self::CurrentPromptBlockMarker,
            "after_current_prompt_block_marker" => Self::AfterCurrentPromptBlockMarker,
            "prompt_box_body" => Self::PromptBoxBody,
            "above_prompt_box" => Self::AbovePromptBox,
            "last_non_empty_above_prompt_box" => Self::LastNonEmptyAbovePromptBox,
            "after_last_horizontal_rule" => Self::AfterLastHorizontalRule,
            "osc_title" => Self::OscTitle,
            "osc_progress" => Self::OscProgress,
            _ => {
                if let Some(n) = counted(spec, "bottom_lines") {
                    return n.parse().ok().map(Self::BottomLines);
                }
                if let Some(n) = counted(spec, "bottom_non_empty_lines") {
                    return n.parse().ok().map(Self::BottomNonEmptyLines);
                }
                let n = counted(spec, "top_non_empty_lines")?;
                // 只认不带前导零的十进制正整数。
                if n.starts_with('0') || !n.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                return n.parse().ok().filter(|&n| n <= MAX_TOP_LINES).map(Self::TopNonEmptyLines);
            }
        };
        Some(region)
    }

    /// 从 `screen`（标题、进度报告另给）里切出这块区域。
    pub(crate) fn slice<'a>(self, screen: &'a str, title: &'a str, progress: &'a str) -> &'a str {
        let lines = Lines::new(screen);
        match self {
            Self::OscTitle => title,
            Self::OscProgress => progress,
            Self::WholeRecent => screen,
            Self::AfterLastPromptMarker => match lines.rposition(is_prompt_line) {
                Some(i) => lines.from(i + 1),
                None => screen,
            },
            Self::BeforeCurrentPromptMarker => match current_prompt(&lines) {
                Some(i) => lines.before(i),
                None => screen,
            },
            Self::WholeRecentWithoutCurrentPromptMarker => match current_prompt(&lines) {
                Some(_) => "",
                None => screen,
            },
            Self::CurrentPromptBlockMarker => {
                current_block_marker(&lines).map_or("", |i| lines.line(i))
            }
            Self::AfterCurrentPromptBlockMarker => current_block_marker(&lines).map_or("", |i| lines.from(i)),
            Self::PromptBoxBody => match prompt_box_top(&lines) {
                Some(top) => {
                    let end = (top + 1..lines.len()).find(|&i| is_horizontal_rule(lines.line(i))).unwrap_or(lines.len());
                    lines.between(top + 1, end)
                }
                None => "",
            },
            Self::AbovePromptBox => above_prompt_box(&lines),
            Self::LastNonEmptyAbovePromptBox => {
                above_prompt_box(&lines).lines().rev().find(|line| !line.trim().is_empty()).unwrap_or("")
            }
            Self::AfterLastHorizontalRule => match lines.rposition(is_horizontal_rule) {
                Some(i) => lines.from(i + 1),
                None => screen,
            },
            Self::BottomLines(n) => lines.from(lines.len().saturating_sub(n)),
            Self::BottomNonEmptyLines(n) => {
                let start = (0..lines.len()).rev().filter(|&i| !lines.line(i).trim().is_empty()).take(n).last();
                start.map_or("", |i| lines.from(i))
            }
            Self::TopNonEmptyLines(n) => {
                let last = (0..lines.len()).filter(|&i| !lines.line(i).trim().is_empty()).take(n).last();
                last.map_or("", |i| lines.before(i + 1))
            }
        }
    }
}

/// `name(n)` 里的 `n`。
fn counted<'a>(spec: &'a str, name: &str) -> Option<&'a str> {
    spec.strip_prefix(name)?.strip_prefix('(')?.strip_suffix(')')
}

/// 一段文字按行切开，记下每行在原文里的起点，好把几行原样切回原文的子串。
struct Lines<'a> {
    text: &'a str,
    /// 每行的起止字节位置（不含换行符）。
    spans: Vec<(usize, usize)>,
}

impl<'a> Lines<'a> {
    fn new(text: &'a str) -> Self {
        let mut spans = Vec::new();
        let mut start = 0;
        for line in text.split_inclusive('\n') {
            let content = line.strip_suffix('\n').unwrap_or(line);
            let content = content.strip_suffix('\r').unwrap_or(content);
            spans.push((start, start + content.len()));
            start += line.len();
        }
        Self { text, spans }
    }

    fn len(&self) -> usize {
        self.spans.len()
    }

    fn line(&self, i: usize) -> &'a str {
        let (start, end) = self.spans[i];
        &self.text[start..end]
    }

    /// 第 `i` 行开头在原文里的位置；`i` 超出行数时是原文末尾。
    fn start(&self, i: usize) -> usize {
        self.spans.get(i).map_or(self.text.len(), |&(start, _)| start)
    }

    /// 从第 `i` 行开始到末尾。
    fn from(&self, i: usize) -> &'a str {
        &self.text[self.start(i)..]
    }

    /// 第 `i` 行之前的全部内容，含最后的换行符。
    fn before(&self, i: usize) -> &'a str {
        &self.text[..self.start(i)]
    }

    /// 第 `start` 行到第 `end` 行之前。
    fn between(&self, start: usize, end: usize) -> &'a str {
        &self.text[self.start(start)..self.start(end).max(self.start(start))]
    }

    fn rposition(&self, mut pred: impl FnMut(&str) -> bool) -> Option<usize> {
        (0..self.len()).rev().find(|&i| pred(self.line(i)))
    }
}

fn is_prompt_line(line: &str) -> bool {
    line == "›" || line.starts_with("› ")
}

fn is_block_marker(line: &str) -> bool {
    line.starts_with(['•', '■', '✗', '✓'])
}

/// 当前的提示行：最后一个提示行，并且它后面没有标记行。
fn current_prompt(lines: &Lines<'_>) -> Option<usize> {
    let prompt = lines.rposition(is_prompt_line)?;
    (prompt + 1..lines.len()).all(|i| !is_block_marker(lines.line(i))).then_some(prompt)
}

/// 当前提示行之前最近的标记行。
fn current_block_marker(lines: &Lines<'_>) -> Option<usize> {
    let prompt = current_prompt(lines)?;
    (0..prompt).rev().find(|&i| is_block_marker(lines.line(i)))
}

/// 输入框上边那条横线：从下往上数第二条横线。
fn prompt_box_top(lines: &Lines<'_>) -> Option<usize> {
    (0..lines.len()).rev().filter(|&i| is_horizontal_rule(lines.line(i))).nth(1)
}

fn above_prompt_box<'a>(lines: &Lines<'a>) -> &'a str {
    match prompt_box_top(lines) {
        Some(top) => lines.before(top),
        None => lines.text,
    }
}

fn is_horizontal_rule(line: &str) -> bool {
    let line = line.trim();
    let dashes = line.chars().take_while(|&c| c == '─').count();
    if dashes == 0 {
        return false;
    }
    let rest = line['─'.len_utf8() * dashes..].trim_start();
    rest.is_empty() || dashes >= 3
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slice<'a>(screen: &'a str, spec: &str) -> &'a str {
        Region::parse(spec).unwrap_or_else(|| panic!("bad region {spec}")).slice(screen, "", "")
    }

    #[test]
    fn regions_cut_the_screen_by_its_structure() {
        for (screen, spec, expected) in [
            ("old\n\nnew\n", "bottom_lines(2)", "\nnew\n"),
            ("a\nb\n\nc\n\n", "bottom_non_empty_lines(2)", "b\n\nc\n\n"),
            ("\n", "bottom_non_empty_lines(2)", ""),
            ("\na\n\nb\nc\n", "top_non_empty_lines(2)", "\na\n\nb\n"),
            ("before\n› input\nafter\n", "after_last_prompt_marker", "after\n"),
            ("before\n› input\nafter\n", "before_current_prompt_marker", "before\n"),
            ("before\n› input\nafter\n", "whole_recent_without_current_prompt_marker", ""),
            ("no marker\n", "whole_recent_without_current_prompt_marker", "no marker\n"),
            ("› old\n• new\n", "whole_recent_without_current_prompt_marker", "› old\n• new\n"),
            ("• old\n■ latest\n› input\n", "current_prompt_block_marker", "■ latest"),
            ("• old\n■ latest\n› input\n", "after_current_prompt_block_marker", "■ latest\n› input\n"),
            ("› old\n• new\n", "current_prompt_block_marker", ""),
            ("above\n\n───\nbody\n───\nfooter\n", "above_prompt_box", "above\n\n"),
            ("above\n\n───\nbody\n───\nfooter\n", "last_non_empty_above_prompt_box", "above"),
            ("above\n───\nbody\n───\nfooter\n", "prompt_box_body", "body\n"),
            ("above\n───\nbody\n", "prompt_box_body", ""),
            ("above\n───\nbody\n───\nfooter\n", "after_last_horizontal_rule", "footer\n"),
            ("above\n── title ──\nfooter\n", "after_last_horizontal_rule", "above\n── title ──\nfooter\n"),
            ("above\n─── title ───\nfooter\n", "after_last_horizontal_rule", "footer\n"),
        ] {
            assert_eq!(slice(screen, spec), expected, "region={spec} screen={screen:?}");
        }
    }

    #[test]
    fn osc_regions_read_their_own_inputs() {
        assert_eq!(Region::OscTitle.slice("screen", "title", "4;3"), "title");
        assert_eq!(Region::OscProgress.slice("screen", "title", "4;3"), "4;3");
    }

    #[test]
    fn region_names_are_strict() {
        assert_eq!(Region::parse(" whole_recent "), Some(Region::WholeRecent));
        assert_eq!(Region::parse("bottom_non_empty_lines(12)"), Some(Region::BottomNonEmptyLines(12)));
        assert_eq!(Region::parse("top_non_empty_lines(20)"), Some(Region::TopNonEmptyLines(20)));
        for bad in ["after_last_promt_marker", "bottom_lines(x)", "top_non_empty_lines(0)", "top_non_empty_lines(07)", "top_non_empty_lines(70000)", "bottom_lines(3"] {
            assert_eq!(Region::parse(bad), None, "{bad}");
        }
    }
}
