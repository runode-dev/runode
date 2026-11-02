//! 推送通知正文里的几行：从屏幕文字里取 agent 问的问题。

/// 等回答时卡片上的几行：agent 问的问题和它上面的内容（比如要执行的命令），不要选项和按键提示。
/// 屏幕上有编号的选项（「❯ 1. Yes」「2. No」这类，连同缩进到选项文字下面的续行）时去掉它们，最后一个
/// 选项下面的（「Esc to cancel · Tab to amend」这类）也不要；没有选项时就是屏幕最底下的几行。去掉
/// 行尾空白，跳过空行和只有空白、制表符（U+2500–U+257F，分隔线和边框）的行，取最后 `limit` 行。
pub(super) fn preview_lines(text: &str, limit: usize) -> Vec<String> {
    let mut kept: Vec<&str> = Vec::new();
    // 正在看的选项的文字从第几列起：缩进到这一列或更深的行是它的续行。
    let mut option: Option<usize> = None;
    // 最后一个选项出现时 `kept` 有多长：之后的都是选项下面的提示。
    let mut before_footer = None;
    let meaningful = text
        .split('\n')
        .map(str::trim_end)
        .filter(|line| line.chars().any(|c| !c.is_whitespace() && !('\u{2500}'..='\u{257f}').contains(&c)));
    for line in meaningful {
        if let Some(column) = option_column(line) {
            option = Some(column);
            before_footer = Some(kept.len());
        } else if option.is_some_and(|column| indent(line) >= column) {
        } else {
            option = None;
            kept.push(line);
        }
    }
    kept.truncate(before_footer.unwrap_or(kept.len()));
    kept[kept.len().saturating_sub(limit)..].iter().map(|&line| line.to_owned()).collect()
}

/// `line` 是编号选项（可以带「❯」这类选中标记，编号一两位，后面跟「.」或「)」和空格）时，选项文字
/// 从第几列（按字符数）起。
fn option_column(line: &str) -> Option<usize> {
    let rest = line.trim_start();
    let rest = rest.strip_prefix(['❯', '›', '>', '▶', '→']).map_or(rest, str::trim_start);
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if !(1..=2).contains(&digits) {
        return None;
    }
    let after = rest[digits..].strip_prefix(['.', ')'])?;
    if !after.starts_with(' ') {
        return None;
    }
    Some(line.chars().count() - after.trim_start().chars().count())
}

/// 行首空白有几个字符。
fn indent(line: &str) -> usize {
    line.chars().take_while(|c| c.is_whitespace()).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> &'static str {
        "╭──────────╮\n│ Bash     │\n╰──────────╯\n\nDo you want to proceed?   \n❯ 1. Yes\n  \
         2. No, and tell Claude what to do differently (esc)\n\n\t \n"
    }

    /// 和手机端 `Presentation.previewLines` 一样：去掉行尾空白，跳过空行、只有空白和制表符的行，取最后几行。
    #[test]
    fn preview_lines_keep_the_question_without_options_and_hints() {
        assert_eq!(preview_lines(screen(), 3), ["│ Bash     │", "Do you want to proceed?"]);
        assert_eq!(preview_lines(screen(), 1), ["Do you want to proceed?"]);
        assert_eq!(preview_lines(screen(), 0), Vec::<String>::new());
        assert_eq!(preview_lines("", 3), Vec::<String>::new());
        // 窄屏下 Claude 的权限确认：选项折到下一行的续行和底下的按键提示都不要。
        let narrow = " Bash command\n╌╌╌╌\n touch hello.txt\n╌╌╌╌\n Do you want to proceed?\n ❯ 1. Yes\n   2. Yes, and \
                      always allow access to\n      /private/tmp/e2e from this project\n   3. No\n\n Esc to cancel · Tab to \
                      amend\n";
        assert_eq!(preview_lines(narrow, 3), [" Bash command", " touch hello.txt", " Do you want to proceed?"]);
        // 提问：选项下面的说明、分隔线下的选项和折成两行的提示都不要。
        let question = " ☐ 午饭\n\n午饭吃什么？\n\n❯ 1. 面条\n     吃面条\n  2. 米饭\n     吃米饭\n────\n  3. Chat about \
                        this\n\nEnter to select · Esc to\ncancel\n";
        assert_eq!(preview_lines(question, 3), [" ☐ 午饭", "午饭吃什么？"]);
        // 没有选项时是最底下的几行。
        assert_eq!(preview_lines("a\n─┼─\n\u{3000}\nb\nContinue? (y/n)", 2), ["b", "Continue? (y/n)"]);
    }
}
