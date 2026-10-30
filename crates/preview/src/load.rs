//! 判断要预览的文件是文本、图片还是二进制。读文件不在这里，调用方按 `limit` 读好交给 `decode`。

use std::path::Path;

/// 文本最多读这么多字节，再多的截掉，`Text::truncated` 记下来。
pub const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
/// 文本最多保留这么多行。
pub const MAX_LINES: usize = 50_000;
/// 图片比这大时不读，按太大处理。
pub const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

/// 按扩展名认出的图片格式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
    Bmp,
    Tiff,
    Ico,
    Svg,
}

/// 读成一行一行的文本，行尾的 `\n`、`\r\n` 都去掉了。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text {
    /// 至少一行；空文件是一个空行。
    pub lines: Vec<String>,
    /// 超过 `MAX_TEXT_BYTES` 或 `MAX_LINES`，后面没读。
    pub truncated: bool,
}

/// 文件能怎么预览。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    Text(Text),
    Image {
        format: ImageFormat,
        bytes: Vec<u8>,
    },
    /// 含 NUL 字节或者不是合法的 UTF-8。
    Binary,
    /// 图片比 `MAX_IMAGE_BYTES` 大。
    TooLarge,
    /// 读不了，附上原因。
    Unreadable(String),
}

/// 按扩展名判断是不是图片，不分大小写。
pub fn image_format(path: &Path) -> Option<ImageFormat> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::Webp,
        "bmp" => ImageFormat::Bmp,
        "tif" | "tiff" => ImageFormat::Tiff,
        "ico" => ImageFormat::Ico,
        "svg" => ImageFormat::Svg,
        _ => return None,
    })
}

/// 读 `path` 来预览时最多读多少字节，以及是不是只要整个文件：图片要整个读出来，比 `MAX_IMAGE_BYTES`
/// 大就不读；其余当文本读前 `MAX_TEXT_BYTES`。
pub fn limit(path: &Path) -> (u64, bool) {
    if image_format(path).is_some() { (MAX_IMAGE_BYTES, true) } else { (MAX_TEXT_BYTES as u64, false) }
}

/// 把按 `limit` 读到的 `path` 的字节判断成能怎么预览；`cut` 表示文件在这之后还有内容。
pub fn decode(path: &Path, bytes: Vec<u8>, cut: bool) -> Content {
    match image_format(path) {
        Some(_) if cut => Content::TooLarge,
        Some(format) => Content::Image { format, bytes },
        None => decode_text(bytes, cut),
    }
}

/// 把读到的字节解成文本；`cut` 表示文件在这之后还有内容。带 UTF-16 的 BOM 的按 UTF-16 解（Windows
/// PowerShell 5.1 的 `>` 默认这样写），其余的含 NUL 或者不是 UTF-8 的当二进制。截断处正好切开一个
/// 多字节字符时只去掉那半个字符。
fn decode_text(mut bytes: Vec<u8>, cut: bool) -> Content {
    let text = if let Some(big_endian) = utf16_bom(&bytes) {
        match decode_utf16(&bytes[2..], big_endian, cut) {
            Some(text) if !text.contains('\0') => text,
            _ => return Content::Binary,
        }
    } else {
        if bytes.contains(&0) {
            return Content::Binary;
        }
        if bytes.starts_with(b"\xEF\xBB\xBF") {
            bytes.drain(..3);
        }
        match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(err) if cut && err.utf8_error().error_len().is_none() => {
                let valid = err.utf8_error().valid_up_to();
                let mut bytes = err.into_bytes();
                bytes.truncate(valid);
                String::from_utf8(bytes).unwrap_or_default()
            }
            Err(_) => return Content::Binary,
        }
    };
    // 只切出要留的 `MAX_LINES` 行：2MB 全是换行时全切出来是两百万个 `String`，绝大部分随即丢掉。
    let mut segments = text.split('\n');
    let mut lines: Vec<String> =
        segments.by_ref().take(MAX_LINES).map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned()).collect();
    let truncated = match segments.next() {
        // 以换行结尾时最后切出一个空串，那不是一行。
        None => {
            if lines.len() > 1 && lines.last().is_some_and(String::is_empty) && !cut {
                lines.pop();
            }
            cut
        }
        // 正好 `MAX_LINES` 行、以换行结尾的不算截断。
        Some("") if !cut && segments.next().is_none() => false,
        Some(_) => true,
    };
    Content::Text(Text { lines, truncated })
}

/// 开头是 UTF-16 的 BOM 时给出是不是大端。
fn utf16_bom(bytes: &[u8]) -> Option<bool> {
    match bytes {
        [0xFF, 0xFE, ..] => Some(false),
        [0xFE, 0xFF, ..] => Some(true),
        _ => None,
    }
}

/// 解 BOM 之后的 UTF-16。截断时末尾落单的字节和切开的代理对去掉；别处有坏的代理对不是文本，为 `None`。
fn decode_utf16(bytes: &[u8], big_endian: bool, cut: bool) -> Option<String> {
    let units = bytes.as_chunks::<2>().0.iter();
    let mut units: Vec<u16> =
        units.map(|&pair| if big_endian { u16::from_be_bytes(pair) } else { u16::from_le_bytes(pair) }).collect();
    if !cut && !bytes.len().is_multiple_of(2) {
        return None;
    }
    if cut && units.last().is_some_and(|unit| (0xD800..0xDC00).contains(unit)) {
        units.pop();
    }
    String::from_utf16(&units).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[&str], truncated: bool) -> Content {
        Content::Text(Text { lines: lines.iter().map(|line| (*line).to_owned()).collect(), truncated })
    }

    #[test]
    fn splits_lines() {
        assert_eq!(decode_text(b"a\nb\n".to_vec(), false), text(&["a", "b"], false));
        assert_eq!(decode_text(b"a\r\nb".to_vec(), false), text(&["a", "b"], false));
        assert_eq!(decode_text(b"a\n\n".to_vec(), false), text(&["a", ""], false));
        assert_eq!(decode_text(Vec::new(), false), text(&[""], false));
        assert_eq!(decode_text(b"\xEF\xBB\xBFbom".to_vec(), false), text(&["bom"], false));
    }

    fn utf16(bom: [u8; 2], text: &str, big_endian: bool) -> Vec<u8> {
        let mut bytes = bom.to_vec();
        for unit in text.encode_utf16() {
            bytes.extend(if big_endian { unit.to_be_bytes() } else { unit.to_le_bytes() });
        }
        bytes
    }

    #[test]
    fn reads_utf16_with_a_bom() {
        assert_eq!(decode_text(utf16([0xFF, 0xFE], "a\r\n中文\n", false), false), text(&["a", "中文"], false));
        assert_eq!(decode_text(utf16([0xFE, 0xFF], "a\nb", true), false), text(&["a", "b"], false));
        // 截断处切开代理对和一个码元时只去掉那半个字符。
        let mut bytes = utf16([0xFF, 0xFE], "a😀", false);
        bytes.truncate(bytes.len() - 3);
        assert_eq!(decode_text(bytes.clone(), true), text(&["a"], true));
        assert_eq!(decode_text(bytes, false), Content::Binary);
        assert_eq!(decode_text(utf16([0xFF, 0xFE], "a\0b", false), false), Content::Binary);
    }

    #[test]
    fn nul_or_invalid_utf8_is_binary() {
        assert_eq!(decode_text(b"abc\0def".to_vec(), false), Content::Binary);
        assert_eq!(decode_text(b"caf\xE9".to_vec(), false), Content::Binary);
        assert_eq!(decode_text(vec![0x89, b'P', b'N', b'G'], false), Content::Binary);
    }

    /// 行数正好到上限、以换行结尾的不算截断，多一行（哪怕没有换行）就算。
    #[test]
    fn line_limit() {
        let full = "a\n".repeat(MAX_LINES);
        let Content::Text(read) = decode_text(full.clone().into_bytes(), false) else { panic!("not text") };
        assert_eq!((read.lines.len(), read.truncated), (MAX_LINES, false));
        let Content::Text(read) = decode_text(format!("{full}b").into_bytes(), false) else { panic!("not text") };
        assert_eq!((read.lines.len(), read.truncated), (MAX_LINES, true));
        let Content::Text(read) = decode_text(format!("{full}\n").into_bytes(), false) else { panic!("not text") };
        assert_eq!((read.lines.len(), read.truncated), (MAX_LINES, true));
        // 截在字节上限处时最后的空串也算后面还有。
        let Content::Text(read) = decode_text(full.into_bytes(), true) else { panic!("not text") };
        assert_eq!((read.lines.len(), read.truncated), (MAX_LINES, true));
    }

    /// 截断处切开的半个字符去掉，不当二进制；没截断时同样的字节是坏的 UTF-8。
    #[test]
    fn cut_inside_a_character() {
        let bytes = "ab\n中".as_bytes()[..4].to_vec();
        assert_eq!(decode_text(bytes.clone(), true), text(&["ab", ""], true));
        assert_eq!(decode_text(bytes, false), Content::Binary);
    }
}
