//! 取消拨号的句柄（`Cancel`）：取消时当场调登记过的钩子，正在等的拨号、握手、iroh 连接各自用钩子叫醒自己。

use std::{
    sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc},
    time::Duration,
};

/// 取消正在进行的 `dial`、`dial_via`、`pair`：`cancel` 以后它们马上返回 `DialError::Cancelled`，还在握手的连接
/// 断掉，iroh 那一路也停下。克隆出来的是同一个句柄；取消了就一直是取消的，再拨号要用新的。
#[derive(Clone, Default)]
pub struct Cancel(Arc<Mutex<Hooks>>);

#[derive(Default)]
struct Hooks {
    cancelled: bool,
    next: u64,
    waiting: Vec<(u64, Box<dyn FnOnce() + Send>)>,
}

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    /// 取消；已经取消过时什么都不做。登记的钩子在调它的线程上跑完才返回，钩子只做不阻塞的事。
    pub fn cancel(&self) {
        let waiting = {
            let mut hooks = self.hooks();
            if hooks.cancelled {
                return;
            }
            hooks.cancelled = true;
            std::mem::take(&mut hooks.waiting)
        };
        for (_, hook) in waiting {
            hook();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.hooks().cancelled
    }

    /// 取消时调 `hook`；已经取消了时当场调。丢掉返回的 `Hook` 就不再调。
    pub(super) fn on_cancel(&self, hook: impl FnOnce() + Send + 'static) -> Hook {
        let mut hooks = self.hooks();
        if hooks.cancelled {
            drop(hooks);
            hook();
            return Hook { cancel: self.clone(), id: None };
        }
        let id = hooks.next;
        hooks.next += 1;
        hooks.waiting.push((id, Box::new(hook)));
        Hook { cancel: self.clone(), id: Some(id) }
    }

    /// 等 `timeout`，取消了就提前回来。返回是不是取消了。
    pub(super) fn sleep(&self, timeout: Duration) -> bool {
        let (woken, wake) = mpsc::channel();
        let _hook = self.on_cancel(move || {
            let _ = woken.send(());
        });
        wake.recv_timeout(timeout).is_ok()
    }

    fn hooks(&self) -> MutexGuard<'_, Hooks> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// `Cancel::on_cancel` 登记的钩子，丢掉时撤销登记。
pub(super) struct Hook {
    cancel: Cancel,
    id: Option<u64>,
}

impl Drop for Hook {
    fn drop(&mut self) {
        if let Some(id) = self.id {
            self.cancel.hooks().waiting.retain(|(waiting, _)| *waiting != id);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };

    use super::Cancel;

    /// 钩子在取消时调一次，撤销了登记的不调，取消以后登记的当场调。
    #[test]
    fn hooks_run_once_unless_dropped() {
        let cancel = Cancel::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = |calls: &Arc<AtomicUsize>| {
            let calls = calls.clone();
            move || {
                calls.fetch_add(1, Ordering::Relaxed);
            }
        };
        let _kept = cancel.on_cancel(count(&calls));
        drop(cancel.on_cancel(count(&calls)));
        let shared = cancel.clone();
        shared.cancel();
        cancel.cancel();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(cancel.is_cancelled());
        let _late = cancel.on_cancel(count(&calls));
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn sleeping_wakes_on_cancel() {
        let cancel = Cancel::new();
        assert!(!cancel.sleep(Duration::from_millis(10)));
        let other = cancel.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            other.cancel();
        });
        let started = Instant::now();
        assert!(cancel.sleep(Duration::from_secs(10)));
        assert!(started.elapsed() < Duration::from_secs(5));
        canceller.join().unwrap();
    }
}
