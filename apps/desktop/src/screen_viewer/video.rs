//! 远端的画面：在读连接的线程上把 HEVC 解成能直接画的一帧（`Picture`）。macOS 上
//! 解成 IOSurface 支持的像素缓冲，交给 GPUI 的 `surface` 元素、不拷像素；Windows 上解成 BGRA，包成
//! `RenderImage`。别的系统没有解码器，远程桌面不可用。

use std::io;

use gpui::{AnyElement, IntoElement as _, ObjectFit, Styled as _};
use runode_types::screen::VideoFrame;

/// 解出来、能直接画的一帧。
pub struct Picture {
    pub width: u32,
    pub height: u32,
    #[cfg(target_os = "macos")]
    image: runode_screen::PixelBuffer,
    #[cfg(windows)]
    pub(super) image: std::sync::Arc<gpui::RenderImage>,
}

// SAFETY: 像素缓冲建好以后只读，CoreVideo 的对象可以跨线程交出去。
#[cfg(target_os = "macos")]
unsafe impl Send for Picture {}

impl Picture {
    /// 铺满父元素、等比缩放居中。
    pub fn render(&self) -> AnyElement {
        #[cfg(target_os = "macos")]
        {
            gpui::surface(self.image.clone()).object_fit(ObjectFit::Contain).size_full().into_any_element()
        }
        #[cfg(windows)]
        {
            use gpui::StyledImage as _;
            gpui::img(gpui::ImageSource::Render(self.image.clone()))
                .object_fit(ObjectFit::Contain)
                .size_full()
                .into_any_element()
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            gpui::div().into_any_element()
        }
    }
}

/// 这个系统能不能当远程桌面的观看端。
pub const SUPPORTED: bool = cfg!(any(target_os = "macos", windows));

/// 一路画面的解码器，只在读连接的线程上用。
pub struct VideoDecoder {
    #[cfg(any(target_os = "macos", windows))]
    inner: runode_screen::Decoder,
}

impl VideoDecoder {
    pub fn new() -> io::Result<Self> {
        #[cfg(any(target_os = "macos", windows))]
        {
            #[cfg(target_os = "macos")]
            let inner = runode_screen::Decoder::new();
            #[cfg(windows)]
            let inner = runode_screen::Decoder::new()?;
            Ok(Self { inner })
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        Err(io::Error::new(io::ErrorKind::Unsupported, "remote desktop is not available on this system"))
    }

    /// 解一帧；还在等关键帧或解码器还没出帧时为 `Ok(None)`，解错时报错。
    pub fn decode(&mut self, frame: &VideoFrame) -> io::Result<Option<Picture>> {
        #[cfg(target_os = "macos")]
        {
            Ok(self.inner.decode(frame)?.map(|decoded| Picture {
                width: decoded.width,
                height: decoded.height,
                image: decoded.image,
            }))
        }
        #[cfg(windows)]
        {
            let Some(decoded) = self.inner.decode(frame)? else { return Ok(None) };
            let buffer = image::RgbaImage::from_raw(decoded.width, decoded.height, decoded.bgra)
                .ok_or_else(|| io::Error::other("a decoded frame of the wrong size"))?;
            let image = std::sync::Arc::new(gpui::RenderImage::new([image::Frame::new(buffer)]));
            Ok(Some(Picture { width: decoded.width, height: decoded.height, image }))
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            let _ = frame;
            Ok(None)
        }
    }

    /// 丢掉解到一半的状态，从下一个关键帧重新开始。
    pub fn reset(&mut self) {
        #[cfg(any(target_os = "macos", windows))]
        self.inner.reset();
    }

    /// 在等关键帧，该向远端要一个。
    pub fn needs_keyframe(&self) -> bool {
        #[cfg(any(target_os = "macos", windows))]
        {
            self.inner.needs_keyframe()
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        true
    }
}
