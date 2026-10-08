//! 一条远程连接从头到尾：TLS 握手、门禁（见 `runode_protocol::remote` 的模块文档）、检查第一帧是
//! 手机的 `Hello`，然后接到宿主上搬字节（`bridge`）。门禁阶段的读写都按同一个截止时刻
//! （`GATE_TIMEOUT`）设超时，对面一个字节一个字节地慢慢发也拖不过它。

use std::{
    io::{self, Read, Write},
    net::{IpAddr, Shutdown, TcpStream},
    os::fd::AsRawFd as _,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use ring::signature::{ECDSA_P256_SHA256_ASN1, UnparsedPublicKey};
use runode_protocol::{
    ClientKind, ClientMsg, FrameKind,
    remote::{
        Bytes, DeviceId, EXPORTER_LABEL, EXPORTER_LEN, GATE_TIMEOUT, GATE_VERSION, GateClientMsg, GateHostMsg,
        NONCE_LEN, PUBLIC_KEY_LEN, Purpose, RejectReason, SECRET_LEN, read_gate_frame, signed_bytes,
    },
    write_frame,
};
use rustls::{ServerConnection, StreamOwned};

use crate::{
    bridge::{bridge, push::PushRequests},
    devices::{self, Device},
    files::random,
    listener::Shared,
    now_unix,
};

/// 拒绝以后最多等这么久让对面读完回话、自己关连接，免得先关的这边把没读的回话冲掉。
const LINGER: Duration = Duration::from_secs(1);
/// 登录时设备表里的 `last_seen` 比这旧才改写，免得手机频繁重连时一直写文件。
const SEEN_GRANULARITY: u64 = 60;
/// 设备名最多留这么多个字符。
const MAX_DEVICE_NAME: usize = 64;

/// 服务手机连来的 `tcp`（从 `peer` 来）：过了门禁就一直搬到有一边断开，没过就回话后断开。`cut`
/// 被置上时（撤销、关掉远程访问）尽快结束。返回前关掉连接。
pub(crate) fn serve(shared: &Shared, id: u64, tcp: &TcpStream, peer: IpAddr, cut: &AtomicBool) {
    if let Err(err) = run(shared, id, tcp, peer, cut) {
        tracing::debug!("remote connection from {peer} ended: {err}");
    }
    let _ = tcp.shutdown(Shutdown::Both);
}

fn run(shared: &Shared, id: u64, tcp: &TcpStream, peer: IpAddr, cut: &AtomicBool) -> io::Result<()> {
    let _ = tcp.set_nodelay(true);
    if let Err(err) = keep_alive(tcp) {
        tracing::warn!("cannot turn on keepalive for a remote connection: {err}");
    }
    let conn = ServerConnection::new(shared.tls.clone()).map_err(io::Error::other)?;
    let mut tls = StreamOwned::new(conn, Deadline { tcp: tcp.try_clone()?, until: Instant::now() + GATE_TIMEOUT });
    while tls.conn.is_handshaking() {
        tls.conn.complete_io(&mut tls.sock)?;
    }
    let nonce = random::<NONCE_LEN>()?;
    let exporter =
        tls.conn.export_keying_material([0u8; EXPORTER_LEN], EXPORTER_LABEL, None).map_err(io::Error::other)?;
    send(
        &mut tls,
        &GateHostMsg::RemoteChallenge {
            version: GATE_VERSION,
            nonce: Bytes(nonce.to_vec()),
            host_name: shared.host_name.clone(),
        },
    )?;
    let message: GateClientMsg = read_control(&mut tls)?;
    let verdict = if shared.stopping.load(Ordering::Relaxed) || cut.load(Ordering::Relaxed) {
        Err(RejectReason::Disabled)
    } else if shared.limiter().limited(peer, Instant::now()) {
        Err(RejectReason::RateLimited)
    } else {
        match message {
            GateClientMsg::RemoteAuth { device_id, signature } => {
                auth(shared, device_id, &signature, &signed_bytes(&nonce, &exporter, Purpose::Auth))
            }
            GateClientMsg::RemotePair { secret, device_name, public_key, signature } => {
                let signed = signed_bytes(&nonce, &exporter, Purpose::Pair);
                pair(shared, &secret, &device_name, public_key, &signature, &signed)
            }
            GateClientMsg::Unknown => return Err(io::Error::other("an unknown gate message")),
        }
    };
    let device_id = match verdict {
        Ok(device_id) => device_id,
        Err(reason) => {
            if !matches!(reason, RejectReason::RateLimited | RejectReason::Disabled) {
                shared.limiter().failed(peer, Instant::now());
            }
            tracing::info!("refused a remote connection from {peer}: {reason:?}");
            send(&mut tls, &GateHostMsg::RemoteRejected { reason })?;
            tls.conn.send_close_notify();
            tls.flush()?;
            linger(tcp);
            return Ok(());
        }
    };
    // 先登记再回话：从这里起撤销这台设备会断开这条连接。登记之前刚好撤销的，撤销的检查可能已经
    // 看过了这条连接，这里再看一眼设备表。
    shared.attach_device(id, device_id);
    if !matches!(devices::find(&shared.dirs, device_id), Ok(Some(_))) {
        send(&mut tls, &GateHostMsg::RemoteRejected { reason: RejectReason::UnknownDevice })?;
        return Ok(());
    }
    send(&mut tls, &GateHostMsg::RemoteAccepted { device_id })?;
    tracing::info!("remote device {device_id} connected from {peer}");
    tls.sock.until = Instant::now() + GATE_TIMEOUT;
    let hello = read_gate_frame(&mut tls)
        .map_err(io::Error::other)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "closed before hello"))?;
    let is_mobile_hello = hello.kind == FrameKind::Control
        && hello.channel == 0
        && matches!(hello.message::<ClientMsg>(), Ok(ClientMsg::Hello { client: ClientKind::Mobile, .. }));
    if !is_mobile_hello {
        return Err(io::Error::other("the first frame after the gate is not a mobile hello"));
    }
    let host = (shared.connect)()?;
    write_frame(&mut &host, hello.kind, hello.channel, &hello.payload).map_err(io::Error::other)?;
    let StreamOwned { mut conn, sock } = tls;
    drop(sock);
    tcp.set_read_timeout(None)?;
    tcp.set_write_timeout(None)?;
    let mut push = PushRequests { dirs: &shared.dirs, device: device_id };
    bridge(&mut conn, tcp, &host, cut, &mut push);
    let _ = host.shutdown(Shutdown::Both);
    tracing::info!("remote device {device_id} disconnected");
    Ok(())
}

/// 已经配对过的设备登录：设备表里有它、签名用它的公钥验得过。
fn auth(shared: &Shared, id: DeviceId, signature: &Bytes, signed: &[u8]) -> Result<DeviceId, RejectReason> {
    let device = match devices::find(&shared.dirs, id) {
        Ok(Some(device)) => device,
        Ok(None) => return Err(RejectReason::UnknownDevice),
        Err(err) => {
            tracing::warn!("cannot read the paired device table: {err}");
            return Err(RejectReason::UnknownDevice);
        }
    };
    if !verify(&device.public_key.0, signed, &signature.0) {
        return Err(RejectReason::BadSignature);
    }
    let now = now_unix();
    if now.saturating_sub(device.last_seen) >= SEEN_GRANULARITY
        && let Err(err) = devices::touch(&shared.dirs, id, now)
    {
        tracing::warn!("cannot record when {id} was last seen: {err}");
    }
    Ok(id)
}

/// 第一次配对：口令对得上（见 `pairing::Desk`），签名用设备给的公钥验得过，就登记这台设备。
fn pair(
    shared: &Shared,
    secret: &Bytes,
    name: &str,
    public_key: Bytes,
    signature: &Bytes,
    signed: &[u8],
) -> Result<DeviceId, RejectReason> {
    if secret.0.len() != SECRET_LEN {
        return Err(RejectReason::PairingInvalid);
    }
    let name = device_name(name);
    let device = shared.desk().redeem(&shared.dirs, &secret.0, || {
        let key = &public_key.0;
        if key.len() != PUBLIC_KEY_LEN || key[0] != 0x04 || !verify(key, signed, &signature.0) {
            return Err(RejectReason::BadSignature);
        }
        let device_id = DeviceId(random().map_err(|_| RejectReason::PairingInvalid)?);
        let now = now_unix();
        Ok(Device { device_id, name: name.clone(), public_key: public_key.clone(), paired_at: now, last_seen: now })
    })?;
    tracing::info!("paired remote device {} ({})", device.device_id, device.name);
    Ok(device.device_id)
}

/// `signature`（DER）是不是 `public_key`（X9.63 未压缩）对 `signed` 的 ECDSA P-256 SHA-256 签名。
pub(crate) fn verify(public_key: &[u8], signed: &[u8], signature: &[u8]) -> bool {
    UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, public_key).verify(signed, signature).is_ok()
}

/// 设备报的名字：去掉控制字符和两头的空白，太长的截短，空的换成一个说得过去的。
fn device_name(name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).take(MAX_DEVICE_NAME).collect();
    let name = name.trim();
    if name.is_empty() { "unnamed device".into() } else { name.into() }
}

fn send(tls: &mut StreamOwned<ServerConnection, Deadline>, message: &GateHostMsg) -> io::Result<()> {
    let payload = serde_json::to_vec(message).map_err(io::Error::other)?;
    write_frame(tls, FrameKind::Control, 0, &payload).map_err(io::Error::other)?;
    tls.flush()
}

/// 读一条门禁消息：要是通道 0 的控制帧，载荷不超过 `GATE_MAX_PAYLOAD`。
fn read_control(tls: &mut StreamOwned<ServerConnection, Deadline>) -> io::Result<GateClientMsg> {
    let frame = read_gate_frame(tls)
        .map_err(io::Error::other)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "closed during the gate"))?;
    if frame.kind != FrameKind::Control || frame.channel != 0 {
        return Err(io::Error::other("a gate message must be a control frame on channel 0"));
    }
    frame.message().map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

/// TCP keepalive 开始探测前空闲多久。
const KEEPALIVE_IDLE_SECS: libc::c_int = 20;
/// 两次探测之间隔多久。
const KEEPALIVE_INTERVAL_SECS: libc::c_int = 10;
/// 连着这么多次探测没回应就断开：空闲的连接在 `KEEPALIVE_IDLE_SECS` 加这么多个间隔（一分钟）内
/// 发现对面没了。
const KEEPALIVE_PROBES: libc::c_int = 4;
const _: () = assert!(KEEPALIVE_IDLE_SECS + KEEPALIVE_INTERVAL_SECS * KEEPALIVE_PROBES <= 60);
/// macOS 的 `TCP_RXT_CONNDROPTIME`（`netinet/tcp.h`，`libc` 里没有）：发出去的数据一直没有回应、
/// 重传这么久（秒）后断开。keepalive 只管空闲的连接，宿主正往手机发输出时对面没了靠这个发现。
#[cfg(target_os = "macos")]
const TCP_RXT_CONNDROPTIME: libc::c_int = 0x80;
/// 有数据没送到时最多重传多久就断开，和 keepalive 发现空闲连接断了的时间相当。
#[cfg(target_os = "macos")]
const RETRANSMIT_DROP_SECS: libc::c_int = 60;

/// 让手机静默消失（离开 Wi-Fi、休眠、切网）的连接在一分钟左右断开：没有这个，半开的连接永远
/// 不会读到结尾，桥接的线程、描述符和宿主里的那条连接都不会放开。不另设应用层的空闲超时：协议
/// 里没有心跳，手机静静看着一个没输出的终端也是正常的连接；对面的系统还在时会回 keepalive 的探测，
/// 不在了才断。
pub(crate) fn keep_alive(tcp: &TcpStream) -> io::Result<()> {
    let set = |level: libc::c_int, name: libc::c_int, value: libc::c_int| {
        // SAFETY: 描述符来自 `tcp`；值指向本地变量，长度是它的大小。
        let result = unsafe {
            libc::setsockopt(
                tcp.as_raw_fd(),
                level,
                name,
                (&raw const value).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        if result == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
    };
    set(libc::SOL_SOCKET, libc::SO_KEEPALIVE, 1)?;
    #[cfg(target_os = "macos")]
    set(libc::IPPROTO_TCP, libc::TCP_KEEPALIVE, KEEPALIVE_IDLE_SECS)?;
    #[cfg(not(target_os = "macos"))]
    set(libc::IPPROTO_TCP, libc::TCP_KEEPIDLE, KEEPALIVE_IDLE_SECS)?;
    set(libc::IPPROTO_TCP, libc::TCP_KEEPINTVL, KEEPALIVE_INTERVAL_SECS)?;
    set(libc::IPPROTO_TCP, libc::TCP_KEEPCNT, KEEPALIVE_PROBES)?;
    #[cfg(target_os = "macos")]
    set(libc::IPPROTO_TCP, TCP_RXT_CONNDROPTIME, RETRANSMIT_DROP_SECS)?;
    Ok(())
}

/// 关掉写的一边，等对面读完回话后关连接（读到结尾），最多等 `LINGER`。
fn linger(tcp: &TcpStream) {
    let _ = tcp.shutdown(Shutdown::Write);
    let deadline = Instant::now() + LINGER;
    let mut buf = [0u8; 1024];
    while let Some(left) = deadline.checked_duration_since(Instant::now()).filter(|left| !left.is_zero()) {
        if tcp.set_read_timeout(Some(left)).is_err() || !matches!((&*tcp).read(&mut buf), Ok(n) if n > 0) {
            return;
        }
    }
}

/// 读写都不超过 `until` 的 TCP 连接，门禁阶段给 rustls 用。
pub(crate) struct Deadline {
    tcp: TcpStream,
    pub(crate) until: Instant,
}

impl Deadline {
    fn left(&self) -> io::Result<Duration> {
        self.until
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "the gate timed out"))
    }
}

/// 超时（`SO_RCVTIMEO` 到了时系统报的是 `WouldBlock`）一律报成 `TimedOut`，rustls 不会当成「等一下再试」。
fn timed_out(err: io::Error) -> io::Error {
    if err.kind() == io::ErrorKind::WouldBlock {
        io::Error::new(io::ErrorKind::TimedOut, "the gate timed out")
    } else {
        err
    }
}

impl Read for Deadline {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.tcp.set_read_timeout(Some(self.left()?))?;
        self.tcp.read(buf).map_err(timed_out)
    }
}

impl Write for Deadline {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.tcp.set_write_timeout(Some(self.left()?))?;
        self.tcp.write(buf).map_err(timed_out)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.tcp.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connections_detect_a_vanished_phone_within_a_minute() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let tcp = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        keep_alive(&tcp).unwrap();
        let get = |level: libc::c_int, name: libc::c_int| {
            let mut value: libc::c_int = 0;
            let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
            // SAFETY: 描述符来自 `tcp`；输出参数指向本地变量，长度如实给出。
            let result =
                unsafe { libc::getsockopt(tcp.as_raw_fd(), level, name, (&raw mut value).cast(), &raw mut len) };
            assert_eq!(result, 0, "{}", io::Error::last_os_error());
            value
        };
        assert_ne!(get(libc::SOL_SOCKET, libc::SO_KEEPALIVE), 0);
        assert_eq!(get(libc::IPPROTO_TCP, libc::TCP_KEEPINTVL), KEEPALIVE_INTERVAL_SECS);
        assert_eq!(get(libc::IPPROTO_TCP, libc::TCP_KEEPCNT), KEEPALIVE_PROBES);
        #[cfg(target_os = "macos")]
        {
            assert_eq!(get(libc::IPPROTO_TCP, libc::TCP_KEEPALIVE), KEEPALIVE_IDLE_SECS);
            assert_eq!(get(libc::IPPROTO_TCP, TCP_RXT_CONNDROPTIME), RETRANSMIT_DROP_SECS);
        }
    }

    #[test]
    fn device_names_are_tidied() {
        assert_eq!(device_name("  Ethan 的 iPhone\n"), "Ethan 的 iPhone");
        assert_eq!(device_name("\u{1b}[31m"), "[31m");
        assert_eq!(device_name(""), "unnamed device");
        assert_eq!(device_name(&"x".repeat(100)).len(), MAX_DEVICE_NAME);
    }
}
