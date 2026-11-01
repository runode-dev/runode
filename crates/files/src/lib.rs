//! 项目目录里不碰界面的文件操作：读文件树要显示的目录（`list_dirs`）、列出所有文件（`walk_files`），新建、改名、移动、复制、移到废纸篓
//! （`ops`），有上限地读文件和判断文件变没变（`read_limited`、`stamp`），以及监听目录里的改动（`watch`）。
//! 桌面在本机直接用，宿主替连着远端的桌面做同样的事时也用它。

mod listing;
pub mod ops;
mod read;
mod watch;

pub use listing::{DirEntry, Listings, MAX_COMPACT, list_dirs, read_dir, single_dir, walk_files};
pub use read::{Limited, Stamp, read_limited, stamp};
pub use watch::{Changes, Watch, watch};
