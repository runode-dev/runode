//! 让远程访问的监听跟着配置走：调用方随时告诉它想要的端口（`None` 是关），它在后台线程里开、关、
//! 换端口。开不了（端口被占、另一个 runode 进程正占着监听，比如交接时还没退出的旧宿主）就每隔
//! `RETRY` 再试，直到开成或者不要了。

use std::{
    sync::{Arc, Condvar, Mutex, PoisonError},
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::listener::{Listener, Options};

/// 开不了监听时隔多久再试。
const RETRY: Duration = Duration::from_secs(2);

/// 跟着配置开关的监听，丢掉时停下。
pub struct Service {
    shared: Arc<Wanted>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct Wanted {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    port: Option<u16>,
    quit: bool,
    /// 每改一次加一，后台线程据此知道等的时候有没有变。
    generation: u64,
}

impl Service {
    /// 起后台线程，先关着，等 `set_port`。`options.port` 不用，每次开时换成要的端口。
    pub fn start(options: Options) -> std::io::Result<Self> {
        let shared = Arc::new(Wanted::default());
        let thread_shared = shared.clone();
        let thread = thread::Builder::new().name("remote-access".into()).spawn(move || run(&thread_shared, options))?;
        Ok(Self { shared, thread: Some(thread) })
    }

    /// 要在 `port` 上开着；`None` 时关掉。和现在的一样时什么都不做。
    pub fn set_port(&self, port: Option<u16>) {
        let mut state = self.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.port != port {
            state.port = port;
            state.generation += 1;
            self.shared.changed.notify_all();
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        {
            let mut state = self.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.quit = true;
            self.shared.changed.notify_all();
        }
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::warn!("the remote access thread panicked");
        }
    }
}

fn run(shared: &Wanted, options: Options) {
    let mut running: Option<Listener> = None;
    // 连着开不成几次了：第一次记警告，之后只记调试日志，免得每两秒一条。
    let mut failures = 0u32;
    let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
        if state.quit {
            break;
        }
        let (wanted, generation) = (state.port, state.generation);
        drop(state);
        if running.as_ref().is_some_and(|listener| Some(listener.port()) != wanted) {
            running = None;
        }
        if let (Some(port), None) = (wanted, &running) {
            match Listener::start(Options { port, ..options.clone() }) {
                Ok(listener) => {
                    running = Some(listener);
                    failures = 0;
                }
                Err(err) => {
                    if failures == 0 {
                        tracing::warn!("cannot start remote access on port {port}, will keep trying: {err}");
                    } else {
                        tracing::debug!("still cannot start remote access on port {port}: {err}");
                    }
                    failures += 1;
                }
            }
        }
        let retry = wanted.is_some() && running.is_none();
        state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.generation == generation && !state.quit {
            state = if retry {
                shared.changed.wait_timeout(state, RETRY).map_or_else(|err| err.into_inner().0, |(s, _)| s)
            } else {
                shared.changed.wait(state).unwrap_or_else(PoisonError::into_inner)
            };
        }
    }
    drop(state);
    drop(running);
}
