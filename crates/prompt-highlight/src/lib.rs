//! 提示符上输入的语法高亮：把一行输入分成命令名、关键字、选项、引号里的字符串、变量、路径
//! 等几类，配色照 zsh 插件 fast-syntax-highlighting 的默认主题。
//!
//! 只看这一行文字：命令名查 shell 报告的名字和 PATH，路径查文件系统，子命令查补全用的命令
//! 规格（都经 `runode_completion`）；不跑 shell，不展开变量。认不出的地方宁可不上色，也不猜。
//! 不碰界面：颜色给的是调色板下标，由界面按主题换成具体颜色。

mod lexer;
mod style;

pub use lexer::{Span, highlight};
pub use runode_completion::Shell;
pub use style::{Kind, Style};
