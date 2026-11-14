//! Android 端跨网络连电脑：iroh 按电脑的公钥拨号，先打洞直连，打不通走中继，连上后开一条双向流。流上跑什么
//! 这里不管，TLS 和门禁都在 Kotlin 那边；ALPN 也由 Kotlin 传进来。
//!
//! 对 Kotlin 只给编号（handle），不给指针。端点和流各放在一张编号到对象的表（`Handles`）里，编号从 1 起递增、
//! 不复用；关掉时从表里删掉，之后拿这个编号来调的一律报已关闭，不会碰到别的对象，也碰不到释放了的内存。每次
//! 调用先在表里取出对象的 `Arc` 再干活，所以一条线程阻塞在读上时，另一条线程关掉它也是安全的：表里删了，读着的
//! 那份 `Arc` 等读返回后才放掉。几个函数都能从任意线程同时调。
//!
//! iroh 是 async 的，所有调用共用一个多线程的 tokio 运行时，调用方的线程 `block_on` 等结果。

use std::{
    collections::BTreeMap,
    io,
    str::FromStr as _,
    sync::{
        Arc, Mutex, OnceLock, PoisonError,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};

use iroh::{
    Endpoint, EndpointAddr, EndpointId,
    endpoint::{Connection, RecvStream, SendStream, VarInt, presets},
};
use tokio::runtime::Runtime;

mod java;

/// 编号到对象的表。0 不会被用作编号，Kotlin 那边可以拿它表示「没有」。
struct Handles<T> {
    next: AtomicI64,
    entries: Mutex<BTreeMap<i64, Arc<T>>>,
}

impl<T> Handles<T> {
    const fn new() -> Self {
        Self { next: AtomicI64::new(1), entries: Mutex::new(BTreeMap::new()) }
    }

    fn insert(&self, value: T) -> i64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.entries.lock().unwrap_or_else(PoisonError::into_inner).insert(id, Arc::new(value));
        id
    }

    fn get(&self, id: i64) -> io::Result<Arc<T>> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.get(&id).cloned().ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "already closed"))
    }

    fn remove(&self, id: i64) -> Option<Arc<T>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner).remove(&id)
    }
}

/// 连接上开出的那条双向流。读和写各有一把锁，一读一写可以在两条线程上同时进行。
struct Stream {
    connection: Connection,
    send: tokio::sync::Mutex<SendStream>,
    recv: tokio::sync::Mutex<RecvStream>,
}

static ENDPOINTS: Handles<Endpoint> = Handles::new();
static STREAMS: Handles<Stream> = Handles::new();
static RUNTIME: OnceLock<Runtime> = OnceLock::new();

fn runtime() -> io::Result<&'static Runtime> {
    if let Some(runtime) = RUNTIME.get() {
        return Ok(runtime);
    }
    let built = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("runode-iroh")
        .enable_all()
        .build()?;
    Ok(RUNTIME.get_or_init(|| built))
}

/// 建一个用 n0 的中继和地址查找的端点，返回它的编号。整个 app 共用一个就够了。
pub(crate) fn bind() -> io::Result<i64> {
    let endpoint = runtime()?.block_on(Endpoint::builder(presets::N0).bind()).map_err(io::Error::other)?;
    Ok(ENDPOINTS.insert(endpoint))
}

/// 经编号是 `endpoint` 的端点拨公钥是 `node`（十六进制）的电脑，连上后开一条双向流，返回流的编号。`node` 不是
/// 公钥时报 `InvalidInput`；`timeout` 内没开出流时报 `TimedOut`。
pub(crate) fn connect(endpoint: i64, node: &str, alpn: &[u8], timeout: Duration) -> io::Result<i64> {
    let id = EndpointId::from_str(node)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, format!("not an iroh endpoint id: {err}")))?;
    connect_to(endpoint, EndpointAddr::new(id), alpn, timeout)
}

fn connect_to(endpoint: i64, addr: EndpointAddr, alpn: &[u8], timeout: Duration) -> io::Result<i64> {
    let endpoint = ENDPOINTS.get(endpoint)?;
    let open = async {
        let connection = endpoint.connect(addr, alpn).await.map_err(io::Error::other)?;
        match connection.open_bi().await {
            Ok((send, recv)) => Ok((connection, send, recv)),
            Err(err) => {
                connection.close(VarInt::from_u32(0), b"");
                Err(io::Error::other(err))
            }
        }
    };
    // 计时器要在运行时里建。
    let opened = runtime()?.block_on(async { tokio::time::timeout(timeout, open).await });
    let (connection, send, recv) =
        opened.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "timed out connecting over iroh"))??;
    Ok(STREAMS.insert(Stream { connection, send: tokio::sync::Mutex::new(send), recv: tokio::sync::Mutex::new(recv) }))
}

/// 读进 `buf`，返回读到的字节数；对面说完了时为空。阻塞到有数据、对面说完、出错或者流被 [`close`] 为止。
pub(crate) fn read(stream: i64, buf: &mut [u8]) -> io::Result<Option<usize>> {
    let stream = STREAMS.get(stream)?;
    runtime()?.block_on(async { stream.recv.lock().await.read(buf).await }).map_err(io::Error::other)
}

/// 把 `buf` 整个写出去，对面收得慢时阻塞。
pub(crate) fn write(stream: i64, buf: &[u8]) -> io::Result<()> {
    let stream = STREAMS.get(stream)?;
    runtime()?.block_on(async { stream.send.lock().await.write_all(buf).await }).map_err(io::Error::other)
}

/// 关掉流和它所在的连接，阻塞着的 [`read`]、[`write`] 随之报错返回。编号已经关过时什么也不做。
pub(crate) fn close(stream: i64) {
    if let Some(stream) = STREAMS.remove(stream) {
        stream.connection.close(VarInt::from_u32(0), b"");
    }
}

/// 关掉端点，等它把连接都关好。编号已经关过时什么也不做。
pub(crate) fn close_endpoint(endpoint: i64) {
    if let Some(endpoint) = ENDPOINTS.remove(endpoint)
        && let Ok(runtime) = runtime()
    {
        runtime.block_on(endpoint.close());
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{Ipv4Addr, SocketAddr},
        sync::mpsc,
        thread,
    };

    use super::*;

    const ALPN: &[u8] = b"runode/test/1";
    const WAIT: Duration = Duration::from_secs(10);

    /// 只走本机：不连 n0 的中继和 DNS，测试不碰外网。
    fn local_endpoint(alpns: Vec<Vec<u8>>) -> Endpoint {
        runtime().unwrap().block_on(Endpoint::builder(presets::Minimal).alpns(alpns).bind()).unwrap()
    }

    /// 在本机起一个接一条连接的端点，把第一条双向流交给 `serve`；返回它的地址。
    fn serve<F, Fut>(serve: F) -> EndpointAddr
    where
        F: FnOnce(SendStream, RecvStream) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send,
    {
        let server = local_endpoint(vec![ALPN.to_vec()]);
        let addr = server
            .bound_sockets()
            .into_iter()
            .filter(SocketAddr::is_ipv4)
            .fold(EndpointAddr::new(server.id()), |addr, socket| {
                addr.with_ip_addr((Ipv4Addr::LOCALHOST, socket.port()).into())
            });
        runtime().unwrap().spawn(async move {
            let connection = server.accept().await.unwrap().await.unwrap();
            let (send, recv) = connection.accept_bi().await.unwrap();
            serve(send, recv).await;
            connection.closed().await;
            server.close().await;
        });
        addr
    }

    /// 本机的手机端点上开一条到 `addr` 的流。
    fn dial(addr: EndpointAddr) -> (i64, i64) {
        let endpoint = ENDPOINTS.insert(local_endpoint(Vec::new()));
        (endpoint, connect_to(endpoint, addr, ALPN, WAIT).unwrap())
    }

    fn read_exactly(stream: i64, count: usize) -> Vec<u8> {
        let mut got = Vec::new();
        while got.len() < count {
            let mut buf = [0; 64];
            let n = read(stream, &mut buf).unwrap().expect("ended early");
            got.extend_from_slice(&buf[..n]);
        }
        got
    }

    /// 锁定两个方向都原样搬字节，对面说完后读到结尾。
    #[test]
    fn reads_and_writes_both_ways() {
        let addr = serve(|mut send, mut recv| async move {
            let mut got = [0; 5];
            recv.read_exact(&mut got).await.unwrap();
            send.write_all(&[got.as_slice(), b" back"].concat()).await.unwrap();
            send.finish().unwrap();
        });
        let (endpoint, stream) = dial(addr);
        write(stream, b"hello").unwrap();
        assert_eq!(read_exactly(stream, 10), b"hello back");
        assert_eq!(read(stream, &mut [0; 8]).unwrap(), None);
        close(stream);
        close_endpoint(endpoint);
    }

    /// 锁定另一条线程上的 `close` 打断阻塞着的读，之后再用这个编号读写都报错，不碰释放了的东西。
    #[test]
    fn closing_interrupts_a_blocked_read() {
        // 对面什么也不发，读只能一直等。
        let addr = serve(|send, recv| async move {
            let _streams = (send, recv);
            std::future::pending::<()>().await;
        });
        let (endpoint, stream) = dial(addr);
        // 先写一点：对面要收到数据才认得这条流。
        write(stream, b"hi").unwrap();
        let (done, finished) = mpsc::channel();
        let reader = thread::spawn(move || done.send(read(stream, &mut [0; 16])).unwrap());
        thread::sleep(Duration::from_millis(200));
        assert!(finished.try_recv().is_err(), "the read should still be blocked");
        close(stream);
        assert!(finished.recv_timeout(WAIT).unwrap().is_err());
        reader.join().unwrap();
        assert_eq!(read(stream, &mut [0; 16]).unwrap_err().kind(), io::ErrorKind::NotConnected);
        assert_eq!(write(stream, b"x").unwrap_err().kind(), io::ErrorKind::NotConnected);
        close(stream);
        close_endpoint(endpoint);
    }

    /// 锁定关掉的端点不能再拨，关两次也没事。
    #[test]
    fn a_closed_endpoint_refuses_to_dial() {
        let endpoint = ENDPOINTS.insert(local_endpoint(Vec::new()));
        close_endpoint(endpoint);
        close_endpoint(endpoint);
        let node = EndpointId::from_bytes(&[7; 32]).map_or_else(|_| "00".repeat(32), |id| id.to_string());
        let err = connect(endpoint, &node, ALPN, WAIT).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotConnected);
    }

    /// 锁定不是公钥的 `node` 报 `InvalidInput`，Kotlin 那边据此当成电脑报错了地址。
    #[test]
    fn a_malformed_node_is_invalid_input() {
        let endpoint = ENDPOINTS.insert(local_endpoint(Vec::new()));
        let err = connect(endpoint, "not a key", ALPN, WAIT).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        close_endpoint(endpoint);
    }
}
