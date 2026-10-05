//! 接受候选时要发给 shell 的按键：只改光标前的部分，补齐引号和空格。

use crate::line::{self, Quote, Word};

/// 改写当前词时要发给 shell 的按键：先退格删掉光标前要换的部分，再写新的字。光标不动、
/// 光标后面的字一概不碰：那里可能是 shell 或插件画的灰字建议，屏幕上和真的输入分不出来，
/// 往右走或删掉它都会把建议当成输入接受下来。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub backspace: usize,
    pub text: String,
}

/// 把输入 `text`（光标在字节位置 `cursor`）里当前词 `word` 从第 `from` 个字到光标的部分换成
/// `value`，光标后面的部分不动。
///
/// 写完后要回到光标处原来的引号状态，光标后面的字才照旧解析：删掉的部分里有引号开关时
/// 补上相应的引号。`finish` 且光标在词尾时这个词算写完了：闭合引号，后面不是空白就补一个
/// 空格；光标在词中间时不补空格，免得把后面的字拆成另一个词。
pub fn edit(text: &str, cursor: usize, word: &Word, from: usize, value: &str, finish: bool) -> Edit {
    let cursor = cursor.clamp(word.raw.start, word.raw.end);
    let at_cursor = line::quote_at(text, word.raw.start, cursor);
    let (start, quote) = match word.char_at(from) {
        Some(found) if from < word.chars_before(cursor) => found,
        // 光标前没有要换的字：就在光标处写。
        _ => (cursor, at_cursor),
    };
    let mut out = line::escape(value, quote, start == word.raw.start);
    let close = |out: &mut String, quote: Quote| match quote {
        Quote::None => {}
        Quote::Single => out.push('\''),
        Quote::Double => out.push('"'),
    };
    if finish && cursor == word.raw.end {
        close(&mut out, quote);
        if !text[cursor..].starts_with([' ', '\t']) {
            out.push(' ');
        }
    } else if quote != at_cursor {
        close(&mut out, quote);
        close(&mut out, at_cursor);
    }
    Edit { backspace: line::edit_chars(&text[start..cursor]), text: out }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::line::parse;

    fn edit_at(input: &str, from: usize, value: &str, finish: bool) -> Edit {
        let cursor = input.find('^').unwrap();
        let text = input.replacen('^', "", 1);
        let segment = parse(&text, cursor);
        edit(&text, cursor, &segment.words[segment.current], from, value, finish)
    }

    fn keys(backspace: usize, text: &str) -> Edit {
        Edit { backspace, text: text.into() }
    }

    #[test]
    fn edits_only_touch_the_text_before_the_cursor() {
        assert_eq!(edit_at("git ch^", 0, "checkout", true), keys(2, "checkout "));
        // 光标在词中间（后面可能是灰字建议）：只换光标前的部分，不往右走，也不补空格。
        assert_eq!(edit_at("git ch^eck x", 0, "checkout", true), keys(2, "checkout"));
        // 空词。
        assert_eq!(edit_at("git ^", 0, "status", true), keys(0, "status "));
        // 光标后面已经是空白：不再补空格。
        assert_eq!(edit_at("git ch^ x", 0, "checkout", true), keys(2, "checkout"));
    }

    #[test]
    fn edits_escape_and_close_quotes() {
        // 文件名只换最后一段，空格加反斜杠；目录不补空格。
        assert_eq!(edit_at("ls ~/My^", 2, "My Dir/", false), keys(2, r"My\ Dir/"));
        // 在双引号里：不加反斜杠，写完时闭合引号。
        assert_eq!(edit_at(r#"ls "My D^"#, 0, "My Doc.txt", true), keys(4, r#"My Doc.txt" "#));
        // 光标在闭合的引号之后：引号在删掉的部分里，写完的词再闭合一次。
        assert_eq!(edit_at(r#"ls "ab"^"#, 0, "abc", true), keys(3, r#"abc" "#));
        // 目录也一样补回删掉的闭合引号，之后接着输入时引号外的字照样连在这个词上。
        assert_eq!(edit_at(r#"ls "My D"^"#, 0, "My Dir/", false), keys(5, r#"My Dir/""#));
        // 引号没闭合、还没写完：引号留着开着。
        assert_eq!(edit_at(r#"ls "My D^"#, 0, "My Dir/", false), keys(4, "My Dir/"));
        // 光标在引号里的词中间：只换光标前的部分，引号状态和光标处一样，不用补。
        assert_eq!(edit_at(r#"ls "a^b" x"#, 0, "abc", true), keys(1, "abc"));
        // 已经输入了反斜杠转义：原文里的反斜杠也一起删掉。
        assert_eq!(edit_at(r"ls My\ D^", 0, "My Dir/", false), keys(5, r"My\ Dir/"));
        // 宽字符算一个字。
        assert_eq!(edit_at("cat 文^", 0, "文件", true), keys(1, "文件 "));
        // 要接 `=` 值的选项不补空格。
        assert_eq!(edit_at("x --fo^", 0, "--format=", false).text, "--format=");
        // 开头的 `~` 是字面的文件名时要转义。
        assert_eq!(edit_at("ls ^", 0, "~x", true), keys(0, r"\~x "));
    }
}
