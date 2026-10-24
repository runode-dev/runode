//! 不属于哪个具体功能、几处界面都用的 GPUI 部件和小工具：各部件见同名子模块，路径、调色板颜色和位图缓存
//! 的几个小函数在本文件。
//!
//! 不依赖桌面 app 里的功能模块，它们要的东西由调用方经参数、事件或 `font_size::UiFontSize` 这类
//! 全局值交过来；文字一律由调用方翻译好再给，这里不带翻译。

pub mod a11y;
pub mod actions;
pub mod badge;
pub mod button;
pub mod card;
pub mod dialog;
pub mod file_icons;
pub mod font_size;
pub mod icons;
pub mod inline_edit;
pub mod input;
pub mod input_handler;
pub mod menu;
pub mod panel;
pub mod path_tree;
pub mod popover;
pub mod row_buttons;
pub mod scrollbar;
pub mod sound;
pub mod spinner;
pub mod switch;
pub mod tabs;
pub mod textarea;
pub mod theme;
pub mod tooltip;

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
};

use gpui::{App, Hsla, Image, ImageSource, MouseDownEvent, RenderImage, Window, rgb};
use runode_types::color::Rgb;

/// 调色板里的颜色换成 GPUI 画图用的颜色。
pub fn hsla(color: Rgb) -> Hsla {
    rgb(color.to_u32()).into()
}

/// 给人看的路径，家目录写成 `~`。
pub fn display_dir(dir: &Path) -> String {
    if let Some(home) = runode_paths::Dirs::from_env().home
        && let Ok(rest) = dir.strip_prefix(&home)
    {
        if rest.as_os_str().is_empty() {
            return "~".into();
        }
        return format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display());
    }
    dir.display().to_string()
}

/// 路径的最后一段，用在标签、提示和列表里；没有最后一段（比如 `/`）时写整个路径。
pub fn base_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

/// `from` 改名或挪到了 `to` 之后 `path` 的新路径；`path` 不是 `from` 也不在它下面时为 `None`。
/// `path` 就是 `from` 时直接是 `to`：`to.join("")` 会多出结尾的 `/`，把文件当成目录去读。
pub fn follow_move(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    let rest = path.strip_prefix(from).ok()?;
    Some(if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) })
}

/// 在标题栏这类能拖动窗口的地方按下鼠标：双击缩放窗口，否则开始拖动窗口。
pub fn drag_window(event: &MouseDownEvent, window: &mut Window, _: &mut App) {
    if event.click_count >= 2 {
        window.titlebar_double_click();
    } else {
        window.start_window_move();
    }
}

/// 把画过的图从各窗口的图集里删掉。`App::drop_image` 不传窗口时只删留在 `App` 里的窗口，而处理某个窗口的
/// 事件时 GPUI 把那个窗口取了出来，它那份图集里的纹理就一直留着；等这轮更新结束、窗口都放回去了再删。
pub fn drop_render_image(image: Arc<RenderImage>, cx: &mut App) {
    cx.defer(move |cx| cx.drop_image(image, None));
}

/// 画过的位图（`Image::id`），见 `draw_bitmap`。
static DRAWN_BITMAPS: Mutex<BTreeSet<u64>> = Mutex::new(BTreeSet::new());

/// 要交给 `img()` 或 `Image::use_render_image` 画的位图先经过它：记下画过，`release_bitmap` 才去放它。
pub fn draw_bitmap(image: &Arc<Image>) -> Arc<Image> {
    DRAWN_BITMAPS.lock().unwrap_or_else(PoisonError::into_inner).insert(image.id());
    image.clone()
}

/// 画过的位图 `id` 要放了：画过且还没放过时为真，同时忘掉它。
fn forget_drawn(id: u64) -> bool {
    DRAWN_BITMAPS.lock().unwrap_or_else(PoisonError::into_inner).remove(&id)
}

/// 放掉 `img()` 画过的位图：GPUI 缓存的解码结果和它在图集里的纹理。`remove_asset` 只清前者，
/// 纹理要拿解码结果另外删，解码结果只能经窗口取，所以和 `drop_render_image` 一样等窗口都放回去。
/// 没经 `draw_bitmap` 画过（或者已经放过）的不动：取解码结果时缓存里没有，`get_render_image` 会白起一次解码。
pub fn release_bitmap(image: Arc<Image>, cx: &mut App) {
    if !forget_drawn(image.id()) {
        return;
    }
    cx.defer(move |cx| {
        let decoded = cx
            .windows()
            .first()
            .and_then(|handle| handle.update(cx, |_, window, cx| image.clone().get_render_image(window, cx)).ok())
            .flatten();
        if let Some(decoded) = decoded {
            cx.drop_image(decoded, None);
        }
        ImageSource::Image(image).remove_asset(cx);
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{Image, ImageFormat};

    use super::{draw_bitmap, forget_drawn};

    #[test]
    fn only_a_drawn_bitmap_is_released_and_only_once() {
        let image = Arc::new(Image::from_bytes(ImageFormat::Png, b"release-test".to_vec()));
        assert!(!forget_drawn(image.id()), "never drawn");
        draw_bitmap(&image);
        assert!(forget_drawn(image.id()));
        assert!(!forget_drawn(image.id()), "already released");
    }
}
