//! 词里的引号、`$`（变量、命令替换和算术）、反引号和历史展开，就地记下颜色。

use super::{Lexer, ends_word, is_name_char};
use crate::Kind;

impl Lexer<'_> {
    /// `"…"`：整段是字符串的颜色，里面的变量和转义另上色，命令替换照样分析。
    pub(super) fn double_quoted(&mut self, literal: &mut Option<String>) {
        let start = self.pos;
        let index = self.spans.len();
        self.span(start..start, Kind::DoubleQuotedArgument);
        self.pos += 1;
        while let Some(c) = self.peek() {
            let at = self.pos;
            match c {
                '"' => {
                    self.pos += 1;
                    break;
                }
                '\\' => {
                    self.pos += 1;
                    match self.bump() {
                        Some(next @ ('$' | '`' | '"' | '\\' | '\n')) => {
                            self.span(at..self.pos, Kind::BackOrDollarDoubleQuotedArgument);
                            if let Some(literal) = literal {
                                literal.push(next);
                            }
                        }
                        Some(next) => {
                            if let Some(literal) = literal {
                                literal.push('\\');
                                literal.push(next);
                            }
                        }
                        None => {}
                    }
                }
                '$' => {
                    self.dollar();
                    if self.pos == at + 1 {
                        if let Some(literal) = literal {
                            literal.push('$');
                        }
                    } else {
                        if !matches!(self.text[at + 1..].chars().next(), Some('(')) {
                            // 变量在字符串里是转义的颜色。
                            if let Some(span) = self.spans.iter_mut().rev().find(|s| s.range.start == at) {
                                span.kind = Kind::BackOrDollarDoubleQuotedArgument;
                            }
                        }
                        *literal = None;
                    }
                }
                '`' => {
                    self.backquoted();
                    *literal = None;
                }
                _ => {
                    self.pos += c.len_utf8();
                    if let Some(literal) = literal {
                        literal.push(c);
                    }
                }
            }
        }
        self.spans[index].range.end = self.pos;
    }

    /// `$'…'`：反斜杠转义另上色。
    pub(super) fn dollar_quoted(&mut self) {
        let start = self.pos;
        let index = self.spans.len();
        self.span(start..start, Kind::DollarQuotedArgument);
        self.pos += 2;
        while let Some(c) = self.peek() {
            let at = self.pos;
            self.pos += c.len_utf8();
            match c {
                '\'' => break,
                '\\' => {
                    self.bump();
                    self.span(at..self.pos, Kind::BackDollarQuotedArgument);
                }
                _ => {}
            }
        }
        self.spans[index].range.end = self.pos;
    }

    /// 光标处的 `$`：变量、`${…}`、命令替换 `$(…)` 或算术 `$((…))`。后面什么都不是时只吃掉
    /// `$` 本身。
    pub(super) fn dollar(&mut self) {
        let start = self.pos;
        self.pos += 1;
        match self.peek() {
            Some('(') if self.peek_at(1) == Some('(') => {
                self.pos += 2;
                self.math();
            }
            Some('(') => {
                self.pos += 1;
                let level = self.open(start);
                self.list(Some(')'));
                self.close(level);
            }
            Some('{') => {
                let mut depth = 0usize;
                while let Some(c) = self.bump() {
                    match c {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                self.span(start..self.pos, Kind::Variable);
            }
            Some(c) if is_name_char(c) && !c.is_ascii_digit() => {
                while self.peek().is_some_and(is_name_char) {
                    self.pos += 1;
                }
                self.span(start..self.pos, Kind::Variable);
            }
            Some(c) if c.is_ascii_digit() || matches!(c, '?' | '@' | '#' | '$' | '!' | '*' | '-') => {
                self.pos += 1;
                self.span(start..self.pos, Kind::Variable);
            }
            _ => {}
        }
    }

    /// `` `…` ``：里面照命令分析，反引号本身不上色。
    pub(super) fn backquoted(&mut self) {
        self.pos += 1;
        self.list(Some('`'));
        if self.peek() == Some('`') {
            self.pos += 1;
        }
    }

    /// 光标处的 `!` 是历史展开时上色并吃掉它，返回 `true`。`!` 后面是空白、`=`、`(` 或者
    /// 什么都没有时不是。
    pub(super) fn history_expansion(&mut self) -> bool {
        let start = self.pos;
        let Some(next) = self.peek_at(1) else {
            return false;
        };
        if next.is_whitespace() || matches!(next, '=' | '(' | '"' | '\'') {
            return false;
        }
        self.pos += 1;
        match next {
            '!' | '$' | '^' | '*' | '#' => self.pos += 1,
            '-' | '0'..='9' => {
                self.pos += 1;
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1;
                }
            }
            _ => {
                while self.peek().is_some_and(|c| !ends_word(c) && !matches!(c, '\'' | '"' | '$' | '`')) {
                    self.bump();
                }
            }
        }
        self.span(start..self.pos, Kind::HistoryExpansion);
        true
    }
}
