//! 配置：兼容 Ghostty 的配置文件。
//!
//! 先按 Ghostty 自己的顺序读它的配置（XDG 目录，再到 macOS 的 Application
//! Support），再读 runode 自己的 `$XDG_CONFIG_HOME/runode/config.conf`。语法与键名都和
//! Ghostty 相同；同一个键 runode 也设了时以 runode 为准。runode 不认识的键直接忽略，
//! 因为 Ghostty 的配置里大部分键与 runode 无关。runode 的配置文件不存在时，启动时会
//! 写一份全部注释掉的模板，列出支持的键和默认值。

pub mod color;
mod edit;
pub mod i18n;
pub mod keybind;
mod model;
mod parse;
mod template;
mod theme;
mod values;

// 配置模板和动作说明的翻译，见 `i18n`；某种语言缺了某个键时取英文。
rust_i18n::i18n!("locales", fallback = "en");

pub use edit::{ConfigFile, check_value};
pub use model::{CellHeight, Config, Keybind, PreviewClick};
pub use parse::{KEYS, config_path};
pub use template::create_config_file;
pub use theme::theme_names;
pub use values::hex;
