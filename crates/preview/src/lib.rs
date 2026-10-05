//! 文件预览不碰界面的部分：读文件并判断能怎么显示（文本、图片，或者只能给一句说明），
//! 给文本做语法高亮，以及把一行文字换成画在屏幕上的样子（制表符展开、过长截断）。
//!
//! 高亮的结果只说用调色板里的哪种颜色（ANSI 16 色的序号或默认前景色）、粗体还是斜体，
//! 由界面按当前终端主题换成实际的颜色，换主题时不必重新高亮。

mod highlight;
mod load;
mod line;

pub use highlight::{Color, Span, Style, highlight, syntax_name};
pub use line::{DisplayLine, TAB_WIDTH, display_line};
pub use load::{Content, ImageFormat, MAX_IMAGE_BYTES, MAX_LINES, MAX_TEXT_BYTES, Text, image_format, load};
