//! 读文件判断能怎么显示：按扩展名认图片，大文件和行数过多的文件截断，二进制和读不了的文件。

use std::{fs, path::Path};

use runode_preview::{Content, ImageFormat, MAX_LINES, MAX_TEXT_BYTES, Text, image_format, load};

fn text(lines: &[&str], truncated: bool) -> Content {
    Content::Text(Text { lines: lines.iter().map(|line| (*line).to_owned()).collect(), truncated })
}

#[test]
fn recognizes_images_by_extension() {
    assert_eq!(image_format(Path::new("a/logo.PNG")), Some(ImageFormat::Png));
    assert_eq!(image_format(Path::new("photo.jpeg")), Some(ImageFormat::Jpeg));
    assert_eq!(image_format(Path::new("photo.jpg")), Some(ImageFormat::Jpeg));
    assert_eq!(image_format(Path::new("icon.svg")), Some(ImageFormat::Svg));
    assert_eq!(image_format(Path::new("favicon.ico")), Some(ImageFormat::Ico));
    assert_eq!(image_format(Path::new("anim.gif")), Some(ImageFormat::Gif));
    assert_eq!(image_format(Path::new("x.webp")), Some(ImageFormat::Webp));
    assert_eq!(image_format(Path::new("x.bmp")), Some(ImageFormat::Bmp));
    assert_eq!(image_format(Path::new("main.rs")), None);
    assert_eq!(image_format(Path::new("png")), None);
}

#[test]
fn truncates_large_files() {
    let dir = std::env::temp_dir().join(format!("runode-preview-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();

    let big = dir.join("big.txt");
    let line = "x".repeat(99) + "\n";
    fs::write(&big, line.repeat(MAX_TEXT_BYTES / 100 + 10)).unwrap();
    let Content::Text(read) = load(&big) else { panic!("not text") };
    assert!(read.truncated);
    assert!(read.lines.len() <= MAX_TEXT_BYTES / 100 + 1);
    assert_eq!(read.lines[0].len(), 99);

    let many = dir.join("many.txt");
    fs::write(&many, "a\n".repeat(MAX_LINES + 5)).unwrap();
    let Content::Text(read) = load(&many) else { panic!("not text") };
    assert!(read.truncated);
    assert_eq!(read.lines.len(), MAX_LINES);

    let small = dir.join("small.rs");
    fs::write(&small, "fn main() {}\n").unwrap();
    assert_eq!(load(&small), text(&["fn main() {}"], false));

    let image = dir.join("dot.png");
    fs::write(&image, b"\x89PNG\r\n\x1a\n").unwrap();
    assert!(matches!(load(&image), Content::Image { format: ImageFormat::Png, .. }));

    let binary = dir.join("blob.bin");
    fs::write(&binary, [1, 2, 0, 3]).unwrap();
    assert_eq!(load(&binary), Content::Binary);

    assert!(matches!(load(&dir.join("missing.txt")), Content::Unreadable(_)));
    fs::remove_dir_all(&dir).ok();
}

/// 指向 /dev/zero 的图片：大小是 0 却读不完，不能一直读下去。
#[cfg(unix)]
#[test]
fn image_symlink_to_a_device_is_not_read() {
    let dir = std::env::temp_dir().join(format!("runode-preview-dev-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let logo = dir.join("logo.png");
    fs::remove_file(&logo).ok();
    std::os::unix::fs::symlink("/dev/zero", &logo).unwrap();
    assert!(matches!(load(&logo), Content::Unreadable(_)));
    fs::remove_dir_all(&dir).ok();
}
