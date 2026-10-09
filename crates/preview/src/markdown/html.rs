//! README 里常用的那点 HTML：把 HTML 块和行内 HTML 拆成开标签、闭标签和文字，由 `Builder` 照 GitHub
//! 的样子解释。不是完整的 HTML 解析器：不补缺的闭标签，注释、`<!DOCTYPE>` 这类声明和 `<script>`、
//! `<style>` 的内容丢掉，实体只认常用的几个和数字的。

pub(super) enum Token<'a> {
    /// 标签名和属性名是小写的，属性值的实体已解开。
    Open {
        name: String,
        attrs: Vec<(String, String)>,
    },
    Close(String),
    /// 原文，空白和实体都没处理。
    Text(&'a str),
}

/// 开标签里属性 `key`（小写）的值。
pub(super) fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs.iter().find(|(name, _)| name == key).map(|(_, value)| value.as_str())
}

enum Scan<'a> {
    /// 一个标签和它占的字节数。
    Tag(Token<'a>, usize),
    /// `<` 后面不像标签，`<` 当文字。
    NotTag,
    /// 到结尾也没等到 `>` 或者引号的另一半，剩下的都当文字。
    Unterminated,
}

pub(super) fn tokens(html: &str) -> Vec<Token<'_>> {
    let mut out = Vec::new();
    let mut rest = html;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            out.push(Token::Text(rest));
            break;
        };
        if lt > 0 {
            out.push(Token::Text(&rest[..lt]));
        }
        rest = &rest[lt..];
        if let Some(body) = rest.strip_prefix("<!--") {
            rest = body.find("-->").map_or("", |end| &body[end + 3..]);
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            rest = rest.find('>').map_or("", |end| &rest[end + 1..]);
            continue;
        }
        match tag(rest) {
            Scan::Tag(token, len) => {
                rest = &rest[len..];
                if let Token::Open { name, .. } = &token
                    && matches!(name.as_str(), "script" | "style")
                {
                    // 内容连同标签一起丢掉；闭标签下一轮当普通的闭标签读掉。
                    let close = format!("</{name}");
                    rest = rest.to_ascii_lowercase().find(&close).map_or("", |at| &rest[at..]);
                    continue;
                }
                out.push(token);
            }
            Scan::NotTag => {
                out.push(Token::Text("<"));
                rest = &rest[1..];
            }
            Scan::Unterminated => {
                out.push(Token::Text(rest));
                break;
            }
        }
    }
    out
}

/// 读 `text` 开头（`<`）的一个标签。引号外遇到 `<` 就不算标签，引号里的到结尾才算没结束，所以整段
/// 只扫一遍。
fn tag(text: &str) -> Scan<'_> {
    let bytes = text.as_bytes();
    let close = bytes.get(1) == Some(&b'/');
    let mut ix = if close { 2 } else { 1 };
    let name_start = ix;
    if !bytes.get(ix).is_some_and(u8::is_ascii_alphabetic) {
        return Scan::NotTag;
    }
    while bytes.get(ix).is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'-') {
        ix += 1;
    }
    let name = text[name_start..ix].to_ascii_lowercase();
    let mut attrs = Vec::new();
    let skip_space = |ix: &mut usize| {
        while bytes.get(*ix).is_some_and(u8::is_ascii_whitespace) {
            *ix += 1;
        }
    };
    loop {
        skip_space(&mut ix);
        match bytes.get(ix) {
            None => return Scan::Unterminated,
            Some(b'>') => break,
            Some(b'/') => ix += 1,
            Some(b'<') => return Scan::NotTag,
            Some(_) => {
                let start = ix;
                while bytes.get(ix).is_some_and(|byte| {
                    !byte.is_ascii_whitespace() && !matches!(byte, b'=' | b'>' | b'/' | b'<' | b'"' | b'\'')
                }) {
                    ix += 1;
                }
                if ix == start {
                    return Scan::NotTag;
                }
                let key = text[start..ix].to_ascii_lowercase();
                skip_space(&mut ix);
                let mut value = "";
                if bytes.get(ix) == Some(&b'=') {
                    ix += 1;
                    skip_space(&mut ix);
                    match bytes.get(ix) {
                        None => return Scan::Unterminated,
                        Some(&quote @ (b'"' | b'\'')) => {
                            let Some(len) = text[ix + 1..].find(char::from(quote)) else {
                                return Scan::Unterminated;
                            };
                            value = &text[ix + 1..ix + 1 + len];
                            ix += len + 2;
                        }
                        Some(_) => {
                            let start = ix;
                            while bytes
                                .get(ix)
                                .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(byte, b'>' | b'<'))
                            {
                                ix += 1;
                            }
                            value = &text[start..ix];
                        }
                    }
                }
                attrs.push((key, decode_entities(value)));
            }
        }
    }
    let token = if close { Token::Close(name) } else { Token::Open { name, attrs } };
    Scan::Tag(token, ix + 1)
}

/// 解开 `&amp;`、`&#169;`、`&#xA9;` 这类实体；认不得的照原样留着。
pub(super) fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp + 1..];
        // 实体名不长，分号只往后找几个字节，一长串 `&` 也是线性的。
        let decoded =
            rest.bytes().take(32).position(|byte| byte == b';').and_then(|end| Some((entity(&rest[..end])?, end)));
        match decoded {
            Some((ch, end)) => {
                out.push(ch);
                rest = &rest[end + 1..];
            }
            None => out.push('&'),
        }
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<char> {
    if let Some(number) = name.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok(),
            None => number.parse().ok(),
        }?;
        return char::from_u32(code).filter(|&ch| ch != '\0');
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "middot" => '·',
        "bull" => '•',
        "ndash" => '–',
        "mdash" => '—',
        "hellip" => '…',
        "larr" => '←',
        "rarr" => '→',
        _ => return None,
    })
}

/// HTML 文字里连着的空白折成一个空格（`&nbsp;` 解开前做，所以它不折）。
pub(super) fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for ch in text.chars() {
        if ch.is_ascii_whitespace() {
            space = true;
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(ch);
    }
    if space {
        out.push(' ');
    }
    out
}
