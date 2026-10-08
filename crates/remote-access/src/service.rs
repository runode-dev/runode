//! 让远程访问的监听跟着配置走：调用方随时告诉它想要的端口（`None` 是关）和给手机看的名字，它在后台
//! 线程里开、关、换端口；名字变了就重开一次监听，Bonjour、状态文件和门禁里的名字一起换掉，连着的
//! 手机会断开后自己重连。开不了（端口被占、另一个 runode 进程正占着监听，比如交接时还没退出的旧宿主）就每隔
//! `RETRY` 再试，直到开成或者不要了。
//!
//! agent 等回答时推 Live Activity 的 `Pusher` 也归它管：监听开着时才推（只有占着监听的那个进程推，
//! 频道文件不会被两个进程抢着写），推送的设置由 `set_push` 给。

use std::{
    sync::{Arc, Condvar, Mutex, PoisonError},
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::{
    listener::{Listener, Options},
    push::{Listening, PushSettings, Pusher},
};

/// 开不了监听时隔多久再试。
const RETRY: Duration = Duration::from_secs(2);

/// 跟着配置开关的监听，丢掉时停下。
pub struct Service {
    shared: Arc<Wanted>,
    thread: Option<JoinHandle<()>>,
    pusher: Arc<Pusher>,
}

#[derive(Default)]
struct Wanted {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    port: Option<u16>,
    /// 给手机看的名字；`None` 时用电脑名，见 `Options::host_name`。
    name: Option<String>,
    quit: bool,
    /// 每改一次加一，后台线程据此知道等的时候有没有变。
    generation: u64,
}

impl Service {
    /// 起后台线程，先关着，等 `set`。`options.port` 和 `options.host_name` 不用，每次开时换成要的。
    /// 推送先按 `PushSettings::default`，等 `set_push`。
    pub fn start(options: Options) -> std::io::Result<Self> {
        let pusher = Arc::new(Pusher::start(options.dirs.clone(), options.connect.clone())?);
        let shared = Arc::new(Wanted::default());
        let (thread_shared, thread_pusher) = (shared.clone(), pusher.clone());
        let thread = thread::Builder::new()
            .name("remote-access".into())
            .spawn(move || run(&thread_shared, &thread_pusher, options))?;
        Ok(Self { shared, thread: Some(thread), pusher })
    }

    /// 推送的设置换成 `settings`，几秒内生效。和现在的一样时什么都不做。
    pub fn set_push(&self, settings: PushSettings) {
        self.pusher.set_settings(settings);
    }

    /// 要在 `port` 上开着，手机看到的名字是 `name`（`None` 是电脑名）；`port` 为 `None` 时关掉。和现在的
    /// 一样时什么都不做。
    pub fn set(&self, port: Option<u16>, name: Option<String>) {
        let mut state = self.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.port != port || state.name != name {
            state.port = port;
            state.name = name;
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

fn run(shared: &Wanted, pusher: &Pusher, options: Options) {
    // 开着的监听和开它时用的名字。
    let mut running: Option<(Listener, Option<String>)> = None;
    // 连着开不成几次了：第一次记警告，之后只记调试日志，免得每两秒一条。
    let mut failures = 0u32;
    let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
        if state.quit {
            break;
        }
        let (wanted, name, generation) = (state.port, state.name.clone(), state.generation);
        drop(state);
        if running.as_ref().is_some_and(|(listener, named)| Some(listener.port()) != wanted || *named != name) {
            running = None;
        }
        if let (Some(port), None) = (wanted, &running) {
            match Listener::start(Options { port, host_name: name.clone(), ..options.clone() }) {
                Ok(listener) => {
                    running = Some((listener, name));
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
        pusher.set_listening(match (wanted, &running) {
            (None, _) => Listening::Off,
            (Some(_), None) => Listening::Starting,
            (Some(_), Some(_)) => Listening::On,
        });
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
    // 进程要退出或者不要远程访问了：推送停下，开着的回合留在频道文件里，下次接着办。
    pusher.set_listening(Listening::Starting);
    drop(running);
}
