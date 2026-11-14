//! 引导页要不要在启动时打开，记在 `runode_paths::Dirs::onboarding_file` 里：全新安装时记成没走完，走完、
//! 跳过或离开引导页后记成走完。没有这个文件而窗口存档在的是老用户升级上来，不打开。

use std::{fs, io, path::Path};

/// 记在文件里的进度。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Pending,
    Done,
}

impl Stage {
    fn text(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Done => "done",
        }
    }
}

/// 按文件内容 `marker`（没有文件时为空）和窗口存档在不在，定这次启动打不打开引导页、要往文件里写什么。认不出
/// 的内容当作走完，不再打开。
fn decide(marker: Option<&str>, has_windows: bool) -> (bool, Option<Stage>) {
    match marker.map(str::trim) {
        None if has_windows => (false, Some(Stage::Done)),
        None => (true, Some(Stage::Pending)),
        Some(text) if text == Stage::Pending.text() => (true, None),
        Some(_) => (false, None),
    }
}

/// 这次启动要不要打开引导页；第一次判断时把结果记下来。读不了文件时不打开。
pub(crate) fn first_launch() -> bool {
    let dirs = runode_paths::Dirs::from_env();
    let Some(path) = dirs.onboarding_file() else { return false };
    let marker = match fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => {
            tracing::warn!("failed to read {}: {err}", path.display());
            return false;
        }
    };
    let has_windows = dirs.windows_file().is_some_and(|file| file.exists());
    let (show, write) = decide(marker.as_deref(), has_windows);
    if let Some(stage) = write {
        save(&path, stage);
    }
    show
}

/// 记下引导页已经走完，之后启动不再打开。在后台写：家目录可能在慢的网络盘上，界面线程不等它。
pub(crate) fn mark_done(cx: &gpui::App) {
    cx.background_executor()
        .spawn(async {
            if let Some(path) = runode_paths::Dirs::from_env().onboarding_file() {
                save(&path, Stage::Done);
            }
        })
        .detach();
}

fn save(path: &Path, stage: Stage) {
    let result = path
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| runode_paths::replace_file(path, format!("{}\n", stage.text()).as_bytes()));
    if let Err(err) = result {
        tracing::warn!("failed to write {}: {err}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_install_shows_and_records_pending() {
        assert_eq!(decide(None, false), (true, Some(Stage::Pending)));
    }

    #[test]
    fn upgrade_from_an_older_version_records_done() {
        assert_eq!(decide(None, true), (false, Some(Stage::Done)));
    }

    #[test]
    fn pending_shows_again() {
        assert_eq!(decide(Some("pending\n"), true), (true, None));
        assert_eq!(decide(Some("pending"), false), (true, None));
    }

    #[test]
    fn done_or_unknown_stays_closed() {
        assert_eq!(decide(Some("done\n"), false), (false, None));
        assert_eq!(decide(Some("garbage"), true), (false, None));
    }
}
