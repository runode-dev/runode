//! 监听一个项目目录（连同不在它下面的 git 目录）里的改动。

use std::path::PathBuf;

use notify::Watcher as _;

/// 监听到的一批改动。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// 变了的路径。
    pub paths: Vec<PathBuf>,
    /// 监听出了错、要求整个重扫：说不准哪里变了，按路径筛的（预览的文件、分支和 tag、命令文件）都要当作
    /// 变了，不能只看 `paths`。
    pub everything: bool,
}

impl Changes {
    /// 并上后到的一批。
    pub fn merge(&mut self, more: Self) {
        self.paths.extend(more.paths);
        self.everything |= more.everything;
    }
}

/// 开着的监听，丢掉就停。
pub struct Watch {
    _watcher: notify::RecommendedWatcher,
}

/// 递归地监听 `dirs`，每批改动交给 `events`（只是读了文件的不交）；监听不了时为空，由调用方退回定时重读。
pub fn watch(dirs: Vec<PathBuf>, events: impl Fn(Changes) + Send + 'static) -> Option<Watch> {
    let everything = dirs.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Some(changes) = changed_paths(event, &everything) {
            events(changes);
        }
    })
    .inspect_err(|err| tracing::warn!("监听 {:?} 失败：{err}", dirs.first()))
    .ok()?;
    for dir in &dirs {
        if let Err(err) = watcher.watch(dir, notify::RecursiveMode::Recursive) {
            tracing::warn!("监听 {} 失败：{err}", dir.display());
            return None;
        }
    }
    Some(Watch { _watcher: watcher })
}

/// 监听到的事件里变了的路径；只是读了文件的为空。出错（比如事件太多丢了一些）或者要求整个重扫时，
/// 说不准哪里变了，标上 `Changes::everything`，路径是监听的那几个目录，重读一遍。
fn changed_paths(event: notify::Result<notify::Event>, everything: &[PathBuf]) -> Option<Changes> {
    let all = || Changes { paths: everything.to_vec(), everything: true };
    match event {
        Ok(event) if event.need_rescan() => Some(all()),
        Ok(event) if event.kind.is_access() => None,
        Ok(event) => Some(Changes { paths: event.paths, everything: false }),
        Err(err) => {
            tracing::debug!("监听出错，整个重读：{err}");
            Some(all())
        }
    }
}

#[cfg(test)]
mod tests {
    use notify::{
        Event, EventKind,
        event::{AccessKind, Flag, ModifyKind},
    };

    use super::*;

    /// 丢了事件、要求重扫时不会因为没有路径就什么都不读。
    #[test]
    fn errors_and_rescans_count_as_everything_changed() {
        let everything = [PathBuf::from("/repo"), PathBuf::from("/main/.git/worktrees/repo")];
        let modified = Event::new(EventKind::Modify(ModifyKind::Any)).add_path("/repo/a.rs".into());
        let file = Changes { paths: vec![PathBuf::from("/repo/a.rs")], everything: false };
        assert_eq!(changed_paths(Ok(modified), &everything), Some(file));
        let read = Event::new(EventKind::Access(AccessKind::Any)).add_path("/repo/a.rs".into());
        assert_eq!(changed_paths(Ok(read), &everything), None);
        let all = Changes { paths: everything.to_vec(), everything: true };
        let rescan = Event::new(EventKind::Other).set_flag(Flag::Rescan);
        assert_eq!(changed_paths(Ok(rescan), &everything).as_ref(), Some(&all));
        let failed = Err(notify::Error::generic("dropped events"));
        assert_eq!(changed_paths(failed, &everything).as_ref(), Some(&all));
    }
}
