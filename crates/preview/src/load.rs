//! 读要预览的文件，判断它是文本、图片还是二进制。

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read as _},
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

/// 读 `path`：图片读出全部字节，其余当文本读，最多读 `MAX_TEXT_BYTES`。
pub fn load(path: &Path) -> Content {
    if let Some(format) = image_format(path) {
        return match fs::metadata(path) {
            Err(err) => Content::Unreadable(err.to_string()),
            // 设备文件、管道的大小是 0，读起来却没有尽头（指向 /dev/zero 的 logo.png），只读普通文件。
            Ok(meta) if !meta.is_file() => Content::Unreadable("not a regular file".to_owned()),
            Ok(meta) if meta.len() > MAX_IMAGE_BYTES => Content::TooLarge,
            Ok(_) => {
                // 看过大小后文件还可能变大，读的时候照样设上限，多读一个字节看超没超。
                let mut bytes = Vec::new();
                match open_regular(path).and_then(|file| file.take(MAX_IMAGE_BYTES + 1).read_to_end(&mut bytes)) {
                    Err(err) => Content::Unreadable(err.to_string()),
                    Ok(_) if bytes.len() as u64 > MAX_IMAGE_BYTES => Content::TooLarge,
                    Ok(_) => Content::Image { format, bytes },
                }
            }
        };
    }
    // 文本同样只读普通文件：打开没有写端的命名管道会一直阻塞，读的上限救不了。
    match fs::metadata(path) {
        Err(err) => return Content::Unreadable(err.to_string()),
        Ok(meta) if !meta.is_file() => return Content::Unreadable("not a regular file".to_owned()),
        Ok(_) => {}
    }
    let mut bytes = Vec::new();
    // 多读一个字节，读满了就说明后面还有。
    let read = open_regular(path).and_then(|file| file.take(MAX_TEXT_BYTES as u64 + 1).read_to_end(&mut bytes));
    if let Err(err) = read {
        return Content::Unreadable(err.to_string());
    }
    let cut = bytes.len() > MAX_TEXT_BYTES;
    bytes.truncate(MAX_TEXT_BYTES);
    decode(bytes, cut)
}

/// `O_NONBLOCK` 的值，和 libc 里的一样（标准库没有这个常量）；没列出的平台为空，照旧阻塞地打开。
#[cfg(unix)]
const O_NONBLOCK: Option<i32> = if cfg!(target_vendor = "apple") {
    Some(0x0004)
} else if cfg!(all(
    target_os = "linux",
    any(
        target_arch = "x86",
        target_arch = "x86_64",
        target_arch = "arm",
        target_arch = "aarch64",
        target_arch = "riscv64",
        target_arch = "powerpc64",
        target_arch = "s390x",
        target_arch = "loongarch64"
    )
)) {
    Some(0o4000)
} else {
    None
};

/// 打开 `path`，打开的不是普通文件时报错。调用方先看过路径是普通文件，可看过以后路径还可能被换成
/// 别的（指向设备的符号链接、命名管道），所以打开以后对着打开的这个再看一次，读的才一定是普通文件。
/// 不阻塞地打开：换成了没有写端的命名管道时阻塞式的打开一直等，走不到这次检查；对普通文件这个标志
/// 不起作用。
fn open_regular(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    if let Some(flag) = O_NONBLOCK {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(flag);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    Ok(file)
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

    /// 行数正好到上限、以换行结尾的不算截断，多一行（哪怕没有换行）就算。
    #[test]
    fn line_limit() {
        let full = "a\n".repeat(MAX_LINES);
        let Content::Text(read) = decode(full.clone().into_bytes(), false) else { panic!("not text") };
        assert_eq!((read.lines.len(), read.truncated), (MAX_LINES, false));
        let Content::Text(read) = decode(format!("{full}b").into_bytes(), false) else { panic!("not text") };
        assert_eq!((read.lines.len(), read.truncated), (MAX_LINES, true));
        let Content::Text(read) = decode(format!("{full}\n").into_bytes(), false) else { panic!("not text") };
        assert_eq!((read.lines.len(), read.truncated), (MAX_LINES, true));
        // 截在字节上限处时最后的空串也算后面还有。
        let Content::Text(read) = decode(full.into_bytes(), true) else { panic!("not text") };
        assert_eq!((read.lines.len(), read.truncated), (MAX_LINES, true));
    }

    /// 看过是普通文件以后路径被换成没有写端的命名管道：打开不阻塞，对着打开的这个看出不是普通文件。
    #[cfg(unix)]
    #[test]
    fn fifo_swapped_in_after_the_check_does_not_block() {
        use std::{process::Command, sync::mpsc, thread, time::Duration};

        let dir = std::env::temp_dir().join(format!("runode-preview-fifo-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("notes.txt");
        fs::remove_file(&fifo).ok();
        assert!(Command::new("mkfifo").arg(&fifo).status().unwrap().success());
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || tx.send(open_regular(&fifo).map(drop)).ok());
        let opened = rx.recv_timeout(Duration::from_secs(5)).expect("打开命名管道阻塞了");
        assert!(opened.is_err());
        fs::remove_dir_all(&dir).ok();
    }

    /// 截断处切开的半个字符去掉，不当二进制；没截断时同样的字节是坏的 UTF-8。
    #[test]
    fn cut_inside_a_character() {
        let bytes = "ab\n中".as_bytes()[..4].to_vec();
        assert_eq!(decode(bytes.clone(), true), text(&["ab", ""], true));
        assert_eq!(decode(bytes, false), Content::Binary);
    }
}
