//! 不属于哪个具体功能、几处界面都用的 GPUI 部件和小工具：单行输入框（`text_field`）、多行输入框
//! （`text_area`）、滚动条（`scrollbar`）、悬停提示（`tooltip`）、界面各档字号（`font_size`）、系统声音（`sound`）、按文件类型区分的图标（`file_icons`）、
//! 让 Option 组合键先走快捷键的输入处理（`input_handler`）、
//! 复制粘贴这类共用的编辑动作（`actions`），把调色板的颜色换成 GPUI 颜色的 `hsla`，以及把家目录写成
//! `~` 显示路径的 `display_dir`，放掉图片在图集里那份的 `drop_render_image`、`release_bitmap`，以及让辅助工具认得界面的
//! 共用写法（`a11y`）。
//!
//! 不依赖桌面 app 里的功能模块，它们要的东西由调用方经参数、事件或 `font_size::UiFontSize` 这类
//! 全局值交过来；文字一律由调用方翻译好再给，这里不带翻译。

pub mod a11y;
pub mod actions;
pub mod file_icons;
pub mod font_size;
pub mod icons;
pub mod inline_edit;
pub mod input_handler;
pub mod menu;
pub mod panel;
pub mod row_buttons;
pub mod scrollbar;
pub mod sound;
pub mod text_area;
pub mod text_field;
pub mod tooltip;

use std::{path::Path, sync::Arc};

use gpui::{App, Hsla, Image, ImageSource, RenderImage, rgb};
use runode_shared_types::color::Rgb;

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
        return format!("~/{}", rest.display());
    }
    dir.display().to_string()
}

/// 把画过的图从各窗口的图集里删掉。`App::drop_image` 不传窗口时只删留在 `App` 里的窗口，而处理某个窗口的
/// 事件时 GPUI 把那个窗口取了出来，它那份图集里的纹理就一直留着；等这轮更新结束、窗口都放回去了再删。
pub fn drop_render_image(image: Arc<RenderImage>, cx: &mut App) {
    cx.defer(move |cx| cx.drop_image(image, None));
}

/// 放掉 `img()` 画过的位图：GPUI 缓存的解码结果和它在图集里的纹理。`remove_asset` 只清前者，
/// 纹理要拿解码结果另外删，解码结果只能经窗口取，所以和 `drop_render_image` 一样等窗口都放回去。
pub fn release_bitmap(image: Arc<Image>, cx: &mut App) {
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
