//! 把提示符上的一行输入切成词：处理引号、反斜杠转义和 `|`、`&&`、`;` 这类分隔，只留光标
//! 所在的那一段命令。每个词记得自己在原文里的位置，补全时才知道要删掉哪些字、在什么引号里
//! 写新的字。

use std::ops::Range;

/// 一处文字所在的引号。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quote {
    None,
    Single,
    Double,
}

/// 词里的一个字符：去掉引号和转义后的字符，以及它在原文里占的字节区间（含转义它的反斜杠）。
#[derive(Clone, Debug)]
struct Char {
    c: char,
    raw: Range<usize>,
    quote: Quote,
}

/// 一个词。
#[derive(Clone, Debug)]
pub struct Word {
    /// 在原文里的字节区间，含引号和反斜杠。
    pub raw: Range<usize>,
    chars: Vec<Char>,
    /// 到词尾时还没闭合的引号。
    pub open_quote: Quote,
    /// 紧跟在 `>`、`<` 后面，是重定向的目标，不算命令的参数。
    pub redirect: bool,
}

impl Word {
    fn empty(at: usize, redirect: bool) -> Self {
        Self { raw: at..at, chars: Vec::new(), open_quote: Quote::None, redirect }
    }

    /// 去掉引号和转义之后的文字。
    pub fn value(&self) -> String {
        self.chars.iter().map(|c| c.c).collect()
    }

    /// 原文里 `cursor`（字节位置）之前的那部分，去掉引号和转义。
    pub fn value_before(&self, cursor: usize) -> String {
        self.chars.iter().take_while(|c| c.raw.start < cursor).map(|c| c.c).collect()
    }

    /// 原文里 `cursor`（字节位置）之前有几个字（去掉引号和转义后）。
    pub fn chars_before(&self, cursor: usize) -> usize {
        self.chars.iter().take_while(|c| c.raw.start < cursor).count()
    }

    /// 去掉引号后的第 `i` 个字在原文里从哪个字节开始（含转义它的反斜杠），在什么引号里。
    pub fn char_at(&self, i: usize) -> Option<(usize, Quote)> {
        self.chars.get(i).map(|c| (c.raw.start, c.quote))
    }
}

/// 光标所在的那一段命令。
#[derive(Clone, Debug)]
pub struct Segment {
    /// 这一段的词，按顺序；光标落在空白处时其中有一个为它插进来的空词。
    pub words: Vec<Word>,
    /// 光标所在的词在 `words` 里的下标。
    pub current: usize,
}

/// 把 `text` 切成词，返回光标（`cursor` 是字节位置）所在的那一段。
///
/// 光标在一个词的中间或末尾时就是这个词；在空白处，或者正好在一个词的开头时，当作在那里
/// 新起一个空词，补全插入新词而不是改后面那个词。
pub fn parse(text: &str, cursor: usize) -> Segment {
    let mut segment: Vec<Word> = Vec::new();
    let mut word: Option<Word> = None;
    let mut redirect = false;
    // 光标所在的那一段已经切完，后面的不用看了。
    let mut done = false;
    let mut chars = text.char_indices().peekable();
    let mut quote = Quote::None;

    // 词结束时收进当前段。
    let finish = |segment: &mut Vec<Word>, word: &mut Option<Word>, redirect: &mut bool| {
        if let Some(word) = word.take() {
            segment.push(word);
            *redirect = false;
        }
    };

    while let Some((i, c)) = chars.next() {
        if quote == Quote::None {
            let separator = matches!(c, '|' | '&' | ';' | '(' | ')' | '\n');
            if separator || c == ' ' || c == '\t' || c == '<' || c == '>' {
                finish(&mut segment, &mut word, &mut redirect);
                if separator {
                    if i >= cursor {
                        done = true;
                        break;
                    }
                    // 一段命令结束，光标在后面，换下一段重新开始。
                    segment.clear();
                    redirect = false;
                } else if c == '<' || c == '>' {
                    redirect = true;
                }
                continue;
            }
        }
        let word = word.get_or_insert_with(|| Word::empty(i, redirect));
        let (value, end, in_quote) = match (quote, c) {
            (Quote::None, '\'') => {
                quote = Quote::Single;
                word.raw.end = i + 1;
                continue;
            }
            (Quote::None, '"') => {
                quote = Quote::Double;
                word.raw.end = i + 1;
                continue;
            }
            (Quote::Single, '\'') | (Quote::Double, '"') => {
                quote = Quote::None;
                word.raw.end = i + 1;
                continue;
            }
            // 反斜杠在单引号外转义下一个字；双引号里只转义这几个字，别的时候原样留着。
            (Quote::None, '\\') | (Quote::Double, '\\')
                if chars
                    .peek()
                    .is_some_and(|&(_, next)| quote == Quote::None || matches!(next, '"' | '\\' | '$' | '`')) =>
            {
                let (j, next) = chars.next().unwrap();
                (next, j + next.len_utf8(), quote)
            }
            _ => (c, i + c.len_utf8(), quote),
        };
        word.chars.push(Char { c: value, raw: i..end, quote: in_quote });
        word.raw.end = end;
    }
    if let Some(word) = &mut word {
        word.open_quote = quote;
    }
    if !done {
        finish(&mut segment, &mut word, &mut redirect);
    }

    // 光标所在的词：光标落在词的开头之后、末尾之前或正好在末尾。
    let current = segment.iter().position(|w| w.raw.start < cursor && cursor <= w.raw.end);
    let current = match current {
        Some(current) => current,
        None => {
            let at = segment.iter().position(|w| w.raw.start >= cursor).unwrap_or(segment.len());
            // 紧跟在重定向符号后面的空白处：补全的是重定向的目标。
            let redirect = text[..cursor].trim_end_matches([' ', '\t']).ends_with(['<', '>']);
            segment.insert(at, Word::empty(cursor, redirect));
            at
        }
    };
    Segment { words: segment, current }
}

/// 从不在引号里的 `start` 读到 `end`（都是 `text` 里的字节位置）之后处在什么引号里。
pub fn quote_at(text: &str, start: usize, end: usize) -> Quote {
    let mut quote = Quote::None;
    let mut chars = text[start..end].chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            // 引号外和双引号里的反斜杠转义下一个字（双引号里是 `"` 之类）；转义了别的字时反斜杠原样留着，
            // 也不影响引号。
            (Quote::None | Quote::Double, '\\') => {
                chars.next();
            }
            (Quote::None, '\'') => quote = Quote::Single,
            (Quote::None, '"') => quote = Quote::Double,
            (Quote::Single, '\'') | (Quote::Double, '"') => quote = Quote::None,
            _ => {}
        }
    }
    quote
}

/// 形如 `NAME=value` 的变量赋值：写在命令前面时是给这条命令的环境变量。
pub fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 在引号外写进 shell 时要加反斜杠的字符。
fn needs_escape(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t'
            | '\\'
            | '\''
            | '"'
            | '$'
            | '`'
            | '&'
            | '|'
            | ';'
            | '<'
            | '>'
            | '('
            | ')'
            | '{'
            | '}'
            | '['
            | ']'
            | '*'
            | '?'
            | '!'
            | '#'
    )
}

/// 把 `text` 原样写进 shell 时的写法：在引号外给特殊字符加反斜杠，`at_word_start` 时开头的
/// `~` 也要转义；在双引号里只转义 `"`、`\`、`$`、`` ` ``；在单引号里遇到单引号先关上引号、
/// 转义它再重新打开。
pub fn escape(text: &str, quote: Quote, at_word_start: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, c) in text.chars().enumerate() {
        match quote {
            Quote::None if needs_escape(c) || (c == '~' && i == 0 && at_word_start) => {
                out.push('\\');
                out.push(c);
            }
            Quote::Double if matches!(c, '"' | '\\' | '$' | '`') => {
                out.push('\\');
                out.push(c);
            }
            Quote::Single if c == '\'' => out.push_str("'\\''"),
            _ => out.push(c),
        }
    }
    out
}

/// shell 行编辑里这段文字算几个字符，方向键和退格按它走：零宽的组合字符跟着前一个字，
/// 宽字符也只算一个。
pub fn edit_chars(text: &str) -> usize {
    text.chars().filter(|&c| runode_terminal::cell_width(c) > 0).count()
}

/// 这段文字在终端里占几格。
pub fn cells(text: &str) -> usize {
    text.chars().map(|c| usize::from(runode_terminal::cell_width(c))).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `^` 标出光标位置。
    fn at(input: &str) -> (String, Segment) {
        let cursor = input.find('^').unwrap();
        let text = input.replacen('^', "", 1);
        let segment = parse(&text, cursor);
        (text, segment)
    }

    fn values(segment: &Segment) -> Vec<String> {
        segment.words.iter().map(Word::value).collect()
    }

    #[test]
    fn splits_words_and_finds_the_cursor() {
        let (_, s) = at("git ch^");
        assert_eq!(values(&s), ["git", "ch"]);
        assert_eq!(s.current, 1);
        let (_, s) = at("git ^");
        assert_eq!(values(&s), ["git", ""]);
        assert_eq!(s.current, 1);
        // 光标在词中间：还是这个词。
        let (text, s) = at("git che^ckout x");
        assert_eq!(s.current, 1);
        assert_eq!(s.words[1].value_before(text.find("ckout").unwrap()), "che");
        // 正好在一个词的开头：在它前面新起一个空词。
        let (_, s) = at("git ^checkout");
        assert_eq!(values(&s), ["git", "", "checkout"]);
        assert_eq!(s.current, 1);
        let (_, s) = at("^git");
        assert_eq!(s.current, 0);
    }

    #[test]
    fn keeps_only_the_segment_under_the_cursor() {
        let (_, s) = at("cd x && git ch^ | less; ls");
        assert_eq!(values(&s), ["git", "ch"]);
        let (_, s) = at("a | b ||c^");
        assert_eq!(values(&s), ["c"]);
        let (_, s) = at("echo $(git br^");
        assert_eq!(values(&s), ["git", "br"]);
        let (_, s) = at("ls;^");
        assert_eq!(values(&s), [""]);
    }

    #[test]
    fn handles_quotes_and_escapes() {
        let (text, s) = at(r#"ls "My Dir/fi^"#);
        assert_eq!(values(&s), ["ls", "My Dir/fi"]);
        let word = &s.words[1];
        assert_eq!(word.open_quote, Quote::Double);
        // `fi` 从原文的哪里开始，在引号里。
        assert_eq!(word.char_at(7), Some((text.find("fi").unwrap(), Quote::Double)));
        assert_eq!(word.char_at(9), None);
        assert_eq!(word.chars_before(text.len()), 9);
        assert_eq!(quote_at(&text, word.raw.start, text.len()), Quote::Double);
        assert_eq!(quote_at(r#"a"b"c"#, 0, 4), Quote::None);
        assert_eq!(quote_at(r#"'a\'"#, 0, 3), Quote::Single);
        assert_eq!(quote_at(r#""a\"b"#, 0, 5), Quote::Double);

        let (_, s) = at(r"ls My\ Dir/a\^");
        assert_eq!(values(&s), ["ls", r"My Dir/a\"]);
        let (_, s) = at(r"echo 'a b'c^");
        assert_eq!(values(&s), ["echo", "a bc"]);
        assert_eq!(s.words[1].open_quote, Quote::None);
        // 双引号里的反斜杠只转义少数几个字。
        let (_, s) = at(r#"echo "a\b\"c^"#);
        assert_eq!(values(&s), ["echo", r#"a\b"c"#]);
        // 引号里的分隔符不算。
        let (_, s) = at("echo 'a | b' x^");
        assert_eq!(values(&s), ["echo", "a | b", "x"]);
    }

    #[test]
    fn marks_redirect_targets() {
        let (_, s) = at("sort < in.txt -r^");
        assert_eq!(values(&s), ["sort", "in.txt", "-r"]);
        assert!(s.words[1].redirect);
        assert!(!s.words[2].redirect);
        let (_, s) = at("cat >^");
        assert!(s.words[s.current].redirect);
        let (_, s) = at("cat > ^");
        assert!(s.words[s.current].redirect);
    }

    #[test]
    fn recognizes_assignments() {
        assert!(is_assignment("FOO=bar"));
        assert!(is_assignment("_X1="));
        assert!(!is_assignment("--opt=x"));
        assert!(!is_assignment("1A=x"));
        assert!(!is_assignment("ls"));
    }

    #[test]
    fn escapes_for_each_quote() {
        assert_eq!(escape("a b&c", Quote::None, false), r"a\ b\&c");
        assert_eq!(escape("~x", Quote::None, true), r"\~x");
        assert_eq!(escape("~x", Quote::None, false), "~x");
        assert_eq!(escape(r#"a "b" $c"#, Quote::Double, false), r#"a \"b\" \$c"#);
        assert_eq!(escape("it's", Quote::Single, false), r"it'\''s");
    }

    #[test]
    fn counts_edit_steps_and_cells() {
        assert_eq!(edit_chars("中文ab"), 4);
        assert_eq!(cells("中文ab"), 6);
        // e 加上组合重音符号：一个字。
        assert_eq!(edit_chars("e\u{301}"), 1);
    }
}
