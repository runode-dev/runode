//! 读要预览的文件，判断它是文本、图片还是二进制。

use std::{
    fs::{self, File},
    io::Read as _,
    path::Path,
};

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
    Image { format: ImageFormat, bytes: Vec<u8> },
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

/// 读 `path`：图片读出全部字节，其余当文本读，最多读 `MAX_TEXT_BYTES`。
pub fn load(path: &Path) -> Content {
    if let Some(format) = image_format(path) {
        return match fs::metadata(path) {
            Err(err) => Content::Unreadable(err.to_string()),
            Ok(meta) if meta.len() > MAX_IMAGE_BYTES => Content::TooLarge,
            Ok(_) => match fs::read(path) {
                Ok(bytes) => Content::Image { format, bytes },
                Err(err) => Content::Unreadable(err.to_string()),
            },
        };
    }
    let mut bytes = Vec::new();
    // 多读一个字节，读满了就说明后面还有。
    let read = File::open(path).and_then(|file| file.take(MAX_TEXT_BYTES as u64 + 1).read_to_end(&mut bytes));
    if let Err(err) = read {
        return Content::Unreadable(err.to_string());
    }
    let cut = bytes.len() > MAX_TEXT_BYTES;
    bytes.truncate(MAX_TEXT_BYTES);
    decode(bytes, cut)
}

/// 把读到的字节解成文本；`cut` 表示文件在这之后还有内容。含 NUL 或者不是 UTF-8 的当二进制，
/// 截断处正好切开一个多字节字符时只去掉那半个字符。
fn decode(mut bytes: Vec<u8>, cut: bool) -> Content {
    if bytes.contains(&0) {
        return Content::Binary;
    }
    if bytes.starts_with(b"\xEF\xBB\xBF") {
        bytes.drain(..3);
    }
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) if cut && err.utf8_error().error_len().is_none() => {
            let valid = err.utf8_error().valid_up_to();
            let mut bytes = err.into_bytes();
            bytes.truncate(valid);
            String::from_utf8(bytes).unwrap_or_default()
        }
        Err(_) => return Content::Binary,
    };
    let mut lines: Vec<String> =
        text.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned()).collect();
    // 以换行结尾时最后切出一个空串，那不是一行。
    if lines.len() > 1 && lines.last().is_some_and(String::is_empty) && !cut {
        lines.pop();
    }
    let mut truncated = cut;
    if lines.len() > MAX_LINES {
        lines.truncate(MAX_LINES);
        truncated = true;
    }
    Content::Text(Text { lines, truncated })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[&str], truncated: bool) -> Content {
        Content::Text(Text { lines: lines.iter().map(|line| (*line).to_owned()).collect(), truncated })
    }

    #[test]
    fn splits_lines() {
        assert_eq!(decode(b"a\nb\n".to_vec(), false), text(&["a", "b"], false));
        assert_eq!(decode(b"a\r\nb".to_vec(), false), text(&["a", "b"], false));
        assert_eq!(decode(b"a\n\n".to_vec(), false), text(&["a", ""], false));
        assert_eq!(decode(Vec::new(), false), text(&[""], false));
        assert_eq!(decode(b"\xEF\xBB\xBFbom".to_vec(), false), text(&["bom"], false));
    }

    #[test]
    fn nul_or_invalid_utf8_is_binary() {
        assert_eq!(decode(b"abc\0def".to_vec(), false), Content::Binary);
        assert_eq!(decode(b"caf\xE9".to_vec(), false), Content::Binary);
        assert_eq!(decode(vec![0x89, b'P', b'N', b'G'], false), Content::Binary);
    }

    /// 截断处切开的半个字符去掉，不当二进制；没截断时同样的字节是坏的 UTF-8。
    #[test]
    fn cut_inside_a_character() {
        let bytes = "ab\n中".as_bytes()[..4].to_vec();
        assert_eq!(decode(bytes.clone(), true), text(&["ab", ""], true));
        assert_eq!(decode(bytes, false), Content::Binary);
    }
}
