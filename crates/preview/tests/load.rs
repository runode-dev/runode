//! 判断读到的文件能怎么显示：按扩展名认图片，截断的文本和太大的图片，行数过多的文件，二进制。

use std::path::Path;

use runode_preview::{Content, ImageFormat, MAX_LINES, MAX_TEXT_BYTES, Text, decode, image_format, limit};

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
fn reads_images_whole_and_text_up_to_the_limit() {
    assert_eq!(limit(Path::new("a/logo.png")), (runode_preview::MAX_IMAGE_BYTES, true));
    assert_eq!(limit(Path::new("main.rs")), (MAX_TEXT_BYTES as u64, false));
}

#[test]
fn truncates_large_files() {
    let line = "x".repeat(99) + "\n";
    let mut big = line.repeat(MAX_TEXT_BYTES / 100 + 10).into_bytes();
    big.truncate(MAX_TEXT_BYTES);
    let Content::Text(read) = decode(Path::new("big.txt"), big, true) else { panic!("not text") };
    assert!(read.truncated);
    assert!(read.lines.len() <= MAX_TEXT_BYTES / 100 + 1);
    assert_eq!(read.lines[0].len(), 99);

    let many = "a\n".repeat(MAX_LINES + 5).into_bytes();
    let Content::Text(read) = decode(Path::new("many.txt"), many, false) else { panic!("not text") };
    assert!(read.truncated);
    assert_eq!(read.lines.len(), MAX_LINES);

    assert_eq!(decode(Path::new("small.rs"), b"fn main() {}\n".to_vec(), false), text(&["fn main() {}"], false));

    let png = b"\x89PNG\r\n\x1a\n".to_vec();
    assert!(matches!(decode(Path::new("dot.png"), png, false), Content::Image { format: ImageFormat::Png, .. }));
    assert_eq!(decode(Path::new("huge.png"), Vec::new(), true), Content::TooLarge);

    assert_eq!(decode(Path::new("blob.bin"), vec![1, 2, 0, 3], false), Content::Binary);
}
