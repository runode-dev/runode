//! 预览栏里画的图片：读到的图片换成 GPUI 能画的样子（`Picture`），解码前先按尺寸估解开后占多少内存，
//! 太大的不交给 GPUI。

use std::{io::Cursor, path::Path, sync::Arc};

use gpui::{App, Image, RenderImage, SvgRenderer};
use runode_preview::Content;

use super::gpui_format;
use crate::{project_host::ProjectHost, ui};

/// 一张位图解码后各帧合起来最多占这么多字节，超出的只显示替代文字：几百帧的 GIF、尺寸写得很大的图
/// 文件不大，GPUI 解开却要几百 MB 到几 GB。SVG 由 GPUI 画成不超过 8192×8192 的位图，最多也是这么多。
const MAX_DECODED_BYTES: usize = 256 << 20;

#[derive(Clone)]
pub(super) enum Picture {
    /// 还没解码的位图和解码后各帧合起来占的字节（`decoded_bytes`）：GPUI 画的时候才解码。
    Bitmap(Arc<Image>, usize),
    /// SVG 在后台画好的位图。
    Svg(Arc<RenderImage>),
}

/// 认同一张图：位图按字节的哈希（GPUI 也按它缓存解码结果），SVG 画好的位图按是不是同一份。
#[derive(PartialEq, Eq, Hash)]
pub(super) enum PictureKey {
    Bitmap(u64),
    Svg(usize),
}

impl Picture {
    pub(super) fn key(&self) -> PictureKey {
        match self {
            Picture::Bitmap(image, _) => PictureKey::Bitmap(image.id()),
            Picture::Svg(image) => PictureKey::Svg(Arc::as_ptr(image) as usize),
        }
    }

    /// 留着这张图占的字节：位图按解码后的各帧（GPUI 解码后留着，直到 `release`），SVG 按画好的各帧。
    pub(super) fn bytes(&self) -> usize {
        match self {
            Picture::Bitmap(_, decoded) => *decoded,
            Picture::Svg(image) => {
                (0..image.frame_count()).filter_map(|frame| image.as_bytes(frame)).map(<[u8]>::len).sum()
            }
        }
    }

    /// 关掉或换掉时放掉解码结果，见 `Preview::release_image`。
    pub(super) fn release(&self, cx: &mut App) {
        match self {
            Picture::Bitmap(image, _) => ui::release_bitmap(image.clone(), cx),
            Picture::Svg(image) => ui::drop_render_image(image.clone(), cx),
        }
    }
}

/// 经 `project` 读出 Markdown 引用的图片文件 `path`，和预览栏打开图片文件一样读、画 SVG；读不了、画不了时
/// 为空。在后台调用。
pub(super) fn picture(project: &ProjectHost, path: &Path, svg: &SvgRenderer) -> Option<Picture> {
    picture_of(super::load_from(project, path), svg)
}

/// 读到的图片换成能画的样子；不是图片、画不了、解码后超过 `MAX_DECODED_BYTES` 时为空。SVG 按自身尺寸画
/// （预览栏打开 SVG 文件时会把小图放大重画，这里不要，徽章这类小图按原大小排）。
pub(super) fn picture_of(content: Content, svg: &SvgRenderer) -> Option<Picture> {
    if let Content::Image { format: runode_preview::ImageFormat::Svg, bytes } = &content {
        return svg.render_single_frame(bytes, 1.).ok().map(Picture::Svg);
    }
    match content {
        Content::Image { format: runode_preview::ImageFormat::Gif, bytes } => first_frame(&bytes),
        Content::Image { format, bytes } => {
            let decoded = decoded_bytes(format, &bytes, MAX_DECODED_BYTES).filter(|&n| n <= MAX_DECODED_BYTES)?;
            Some(Picture::Bitmap(Arc::new(Image::from_bytes(gpui_format(format), bytes)), decoded))
        }
        _ => None,
    }
}

/// Markdown 里的 GIF 只留第一帧：几百帧的录屏每帧都解成整张画布，合起来动辄上 GB，放不进
/// `MAX_LOCAL_IMAGE_BYTES`。重编成 PNG 交给 GPUI，画布超过 `MAX_DECODED_BYTES` 或解不了时为空。
fn first_frame(bytes: &[u8]) -> Option<Picture> {
    // 只读头和第一帧的宽高，不解像素：帧可以比画布写得大得多，解码前先按较大的算。
    let mut options = gif::DecodeOptions::new();
    options.skip_frame_decoding(true);
    options.set_memory_limit(gif::MemoryLimit::Unlimited);
    let mut header = options.read_info(Cursor::new(bytes)).ok()?;
    let canvas = usize::from(header.width()).saturating_mul(usize::from(header.height()));
    let first = header.read_next_frame().ok()??;
    let pixels = canvas.max(usize::from(first.width).saturating_mul(usize::from(first.height)));
    let decoded = pixels.saturating_mul(4);
    if decoded > MAX_DECODED_BYTES {
        return None;
    }
    let frame = image::load_from_memory_with_format(bytes, image::ImageFormat::Gif).ok()?;
    let mut png = Vec::new();
    frame.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).ok()?;
    Some(Picture::Bitmap(Arc::new(Image::from_bytes(gpui::ImageFormat::Png, png)), decoded))
}

/// 位图 `bytes` 解码后超过 `MAX_DECODED_BYTES`：预览栏直接打开图片文件、看图片的 diff 时也按太大处理，不交给
/// GPUI 解码。读不出尺寸时不算，照旧交给 GPUI，解不了由它报。
pub(super) fn too_large_to_decode(format: runode_preview::ImageFormat, bytes: &[u8]) -> bool {
    decoded_bytes(format, bytes, MAX_DECODED_BYTES).is_some_and(|n| n > MAX_DECODED_BYTES)
}

/// 位图 `bytes` 照 GPUI 解码（`Image::to_image_data`）那样解开后各帧合起来占的字节：GIF 每一帧都解成整张
/// 画布大小的 RGBA，别的只解第一帧。GIF 只数帧、不解像素（解一帧就要按画布大小分配，写得很大的图在这里
/// 就撑爆了），数过 `budget` 就停，返回的比 `budget` 大；解不了的帧 GPUI 会跳过，这里照样算上，宁可多算。
/// 读不出尺寸（GPUI 也解不了）时为空。
fn decoded_bytes(format: runode_preview::ImageFormat, bytes: &[u8], budget: usize) -> Option<usize> {
    let rgba = |width: usize, height: usize| width.saturating_mul(height).saturating_mul(4);
    if format == runode_preview::ImageFormat::Gif {
        let mut options = gif::DecodeOptions::new();
        options.skip_frame_decoding(true);
        // 不解像素，用不着防它分配；GPUI 那边是 image 的 `GifDecoder`，本来就不设上限。
        options.set_memory_limit(gif::MemoryLimit::Unlimited);
        let mut decoder = options.read_info(Cursor::new(bytes)).ok()?;
        let frame = rgba(decoder.width().into(), decoder.height().into());
        if frame > budget {
            return Some(frame);
        }
        let mut total = 0usize;
        // 每一帧解码时先按它自己写的宽高分配，再画到画布上；帧可以比画布写得大得多。
        while let Ok(Some(next)) = decoder.read_next_frame() {
            total = total.saturating_add(frame.max(rgba(next.width.into(), next.height.into())));
            if total > budget {
                break;
            }
        }
        return (total > 0).then_some(total);
    }
    let reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    let (width, height) = reader.into_dimensions().ok()?;
    Some(rgba(width as usize, height as usize))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 位图解码后的大小按尺寸算；GIF 按帧数乘每帧的整张 RGBA，数过预算就停。
    #[test]
    fn decoded_size_counts_every_gif_frame() {
        use runode_preview::ImageFormat;
        let mut png = Vec::new();
        image::RgbaImage::new(100, 50).write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        assert_eq!(decoded_bytes(ImageFormat::Png, &png, usize::MAX), Some(100 * 50 * 4));
        let mut gif = Vec::new();
        let frames = (0..3).map(|_| image::Frame::new(image::RgbaImage::new(10, 10)));
        image::codecs::gif::GifEncoder::new(&mut gif).encode_frames(frames).unwrap();
        assert_eq!(decoded_bytes(ImageFormat::Gif, &gif, usize::MAX), Some(3 * 400));
        // 数过预算就停：第二帧时已经超了。
        assert_eq!(decoded_bytes(ImageFormat::Gif, &gif, 500), Some(800));
        assert_eq!(decoded_bytes(ImageFormat::Png, b"not an image", usize::MAX), None);
        // 一帧写得比画布大：照帧的宽高算。
        let mut big = Vec::new();
        let frame = image::Frame::new(image::RgbaImage::new(30, 20));
        image::codecs::gif::GifEncoder::new(&mut big).encode_frames([frame]).unwrap();
        big[6..10].copy_from_slice(&[1, 0, 1, 0]);
        assert_eq!(decoded_bytes(ImageFormat::Gif, &big, usize::MAX), Some(30 * 20 * 4));
        // 头里把画布写成 65535×65535：不照它分配，直接算出来就是太大。
        gif[6..10].copy_from_slice(&[0xff; 4]);
        assert_eq!(decoded_bytes(ImageFormat::Gif, &gif, 500), Some(65535 * 65535 * 4));
    }

    /// Markdown 里的 GIF 只按第一帧算占用，几百帧的录屏也放得进预算。
    #[test]
    fn markdown_gif_keeps_only_first_frame() {
        let mut gif = Vec::new();
        let frames = (0..50).map(|_| image::Frame::new(image::RgbaImage::new(10, 10)));
        image::codecs::gif::GifEncoder::new(&mut gif).encode_frames(frames).unwrap();
        let Some(Picture::Bitmap(_, decoded)) = first_frame(&gif) else { panic!() };
        assert_eq!(decoded, 10 * 10 * 4);
        assert!(first_frame(b"not a gif").is_none());
    }
}
