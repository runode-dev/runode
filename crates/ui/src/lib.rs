//! 不属于哪个具体功能、几处界面都用的 GPUI 部件和小工具：按钮和开关按钮（`button`）、开关（`switch`）、卡片（`card`）、
//! 徽标（`badge`）、对话框的面板（`dialog`）、单行输入框
//! （`input`）、多行输入框（`textarea`，两样都分成画边框的组件和管编辑的 entity）、滚动条（`scrollbar`）、悬停提示（`tooltip`）、界面各档字号（`font_size`）、系统声音（`sound`）、按文件类型区分的图标（`file_icons`）、
//! 让 Option 组合键先走快捷键的输入处理（`input_handler`）、
//! 复制粘贴这类共用的编辑动作（`actions`），标签条共用的拖动卡片和落点提示（`tabs`），把调色板的颜色换成
//! GPUI 颜色的 `hsla`，以及把家目录写成 `~` 显示路径的 `display_dir`、取路径最后一段的 `base_name`、跟着改名挪路径的
//! `follow_move`、在标题栏这类地方按下鼠标拖动窗口的 `drag_window`，放掉图片在图集里那份的 `drop_render_image`、
//! `release_bitmap`，让辅助工具认得界面的共用写法（`a11y`），以及各部件取颜色的主题 token（`theme`）。
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
pub mod row_buttons;
pub mod scrollbar;
pub mod sound;
pub mod switch;
pub mod tabs;
pub mod textarea;
pub mod theme;
pub mod tooltip;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use gpui::{App, Hsla, Image, ImageSource, MouseDownEvent, RenderImage, Window, rgb};
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
