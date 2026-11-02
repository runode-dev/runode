//! 把配对的电脑的端口转到本机（`hosts::forward` 分流过来）：每个转发在本机 127.0.0.1 上开一个监听，每接一个
//! 连接另拨一条专用连接（`ChannelHello::ForwardHello`），那边回 `ChannelReady` 之后两边原样搬字节，哪边关了就断开。
//! 断开或忘掉那台电脑时监听跟着关（`forget`）。

use std::{
    collections::HashMap,
    io::{self, Read, Write},
    net::{Ipv4Addr, Shutdown, TcpListener, TcpStream},
    sync::{
        Arc, LazyLock, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use anyhow::{Context as _, Result};
use runode_protocol::remote::ChannelHello;

use super::{PairedHost, find};
use crate::host_client::{UnixStream, hosts::forward::pick_local};

/// 本机开着的一个监听。
struct Listening {
    local: u16,
    /// 置位后接下来的连接不再转，监听线程退出。
    stopped: Arc<AtomicBool>,
}

impl Listening {
    /// 停掉监听：置位后连一下自己，叫醒等在 `accept` 上的线程。
    fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect((Ipv4Addr::LOCALHOST, self.local));
    }
}

/// 电脑的标识、远端端口到本机的监听。
static FORWARDS: LazyLock<Mutex<HashMap<(String, u16), Listening>>> = LazyLock::new(Default::default);

/// 电脑 `id` 上转着的端口：远端端口到本机端口，按远端端口排好。
pub fn forwarded(id: &str) -> Vec<(u16, u16)> {
    let forwards = FORWARDS.lock().unwrap_or_else(PoisonError::into_inner);
    let mut ports: Vec<_> = forwards
        .iter()
        .filter(|((host, _), _)| host == id)
        .map(|((_, remote), listening)| (*remote, listening.local))
        .collect();
    ports.sort_unstable();
    ports
}

/// 停掉电脑 `id` 上 `port` 到本机的转发；没转着时什么都不做。已经接上的连接照常搬完。
pub fn stop(id: &str, port: u16) {
    if let Some(listening) = FORWARDS.lock().unwrap_or_else(PoisonError::into_inner).remove(&(id.to_owned(), port)) {
        listening.stop();
        tracing::info!("stopped forwarding localhost:{} to the paired computer {id}:{port}", listening.local);
    }
}

/// 断开或忘掉了电脑 `id`：停掉它所有的转发。
pub(super) fn forget(id: &str) {
    let mut forwards = FORWARDS.lock().unwrap_or_else(PoisonError::into_inner);
    forwards.retain(|(host, _), listening| {
        let keep = host != id;
        if !keep {
            listening.stop();
        }
        keep
    });
}

/// 把那台电脑的 `port` 转到本机，回本机的端口；已经转着时直接用它。会阻塞，在后台线程上调。
pub fn forward(host: &PairedHost, port: u16) -> Result<u16> {
    let key = (host.id.clone(), port);
    if let Some(listening) = FORWARDS.lock().unwrap_or_else(PoisonError::into_inner).get(&key) {
        return Ok(listening.local);
    }
    let local = pick_local(None, port)?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, local)).context("cannot listen on this computer")?;
    let stopped = Arc::new(AtomicBool::new(false));
    let accepting = {
        let (id, stopped) = (host.id.clone(), stopped.clone());
        move || accept(listener, &id, port, &stopped)
    };
    thread::Builder::new().name("paired-forward".into()).spawn(accepting).context("cannot start a thread")?;
    tracing::info!("forwarding localhost:{local} to the paired computer {}:{port}", host.id);
    FORWARDS.lock().unwrap_or_else(PoisonError::into_inner).insert(key, Listening { local, stopped });
    Ok(local)
}

/// 接本机来的连接，每个另起线程拨过去转发，到 `stopped` 置位为止。
fn accept(listener: TcpListener, id: &str, port: u16, stopped: &AtomicBool) {
    for incoming in listener.incoming() {
        if stopped.load(Ordering::Relaxed) {
            return;
        }
        let Ok(local) = incoming else { continue };
        let host = id.to_owned();
        let spawned = thread::Builder::new().name("paired-forward-conn".into()).spawn(move || {
            let peer = local.peer_addr().ok();
            if let Err(err) = carry(local, &host, port) {
                tracing::info!("cannot forward {peer:?} to the paired computer {host}:{port}: {err:#}");
            }
        });
        if let Err(err) = spawned {
            tracing::warn!("cannot start a thread to forward to the paired computer {id}: {err}");
        }
    }
}

/// 拨一条到那台电脑 `port` 的专用连接，和本机来的 `local` 互搬字节，两边都搬完为止。
fn carry(local: TcpStream, id: &str, port: u16) -> Result<()> {
    let host = find(id).context("the paired computer is gone")?;
    let remote = host.open_channel(&ChannelHello::ForwardHello { port }).map_err(|err| anyhow::anyhow!("{err}"))?;
    let _ = local.set_nodelay(true);
    let (local_in, remote_out) = (local.try_clone()?, remote.try_clone()?);
    let upload = thread::Builder::new()
        .name("paired-forward-up".into())
        .spawn(move || splice(local_in, remote_out, |remote: &UnixStream| remote.shutdown(Shutdown::Write)))?;
    splice(remote, local, |local: &TcpStream| local.shutdown(Shutdown::Write));
    let _ = upload.join();
    Ok(())
}

/// 把 `from` 读到头写进 `to`，再用 `close` 关掉 `to` 的写端，对面读到结尾。
fn splice<R: Read, W: Write>(mut from: R, mut to: W, close: impl FnOnce(&W) -> io::Result<()>) {
    let _ = io::copy(&mut from, &mut to);
    let _ = close(&to);
}

#[cfg(test)]
mod tests {
    use std::{
        net::{Ipv4Addr, TcpListener},
        sync::{Arc, atomic::AtomicBool, mpsc},
        thread,
        time::Duration,
    };

    use super::{Listening, accept};

    /// 停掉转发时等在 `accept` 上的线程马上退出，本机的端口放开。
    #[test]
    fn stopping_wakes_the_listener() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let local = listener.local_addr().unwrap().port();
        let stopped = Arc::new(AtomicBool::new(false));
        let (done, finished) = mpsc::channel();
        let flag = stopped.clone();
        thread::spawn(move || {
            accept(listener, "nobody", 80, &flag);
            let _ = done.send(());
        });
        Listening { local, stopped }.stop();
        finished.recv_timeout(Duration::from_secs(5)).expect("the listener did not stop");
        assert!(TcpListener::bind((Ipv4Addr::LOCALHOST, local)).is_ok());
    }
}
