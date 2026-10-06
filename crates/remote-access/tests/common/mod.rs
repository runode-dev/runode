//! 门禁测试共用的：开在回环地址上的监听（宿主换成一对 Unix socket 的假连接），和一部用 rustls
//! 连上来、按证书指纹认 Mac 的「手机」。

#![allow(dead_code)]

use std::{
    io::{self, Write as _},
    net::{SocketAddr, TcpStream},
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

use ring::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair as _},
};
use runode_paths::Dirs;
use runode_protocol::{
    ClientMsg, FrameKind, HostMsg, read_frame,
    remote::{
        Bytes, DeviceId, EXPORTER_LABEL, GateClientMsg, GateHostMsg, Purpose, decode_base64url, read_gate_frame,
        signed_bytes,
    },
    write_frame,
};
use runode_remote_access::{Bind, Listener, Options, PairingTicket, listener_status};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{CryptoProvider, ring::default_provider, verify_tls13_signature},
    pki_types::{CertificateDer, ServerName, UnixTime},
    version::TLS13,
};

/// 读写最多等这么久，监听那边出了问题时测试失败而不是卡住。
pub const PATIENCE: Duration = Duration::from_secs(5);

/// 开着的监听和它的文件。
pub struct Harness {
    root: PathBuf,
    pub dirs: Dirs,
    /// `restart` 时短暂为 `None`。
    pub listener: Option<Listener>,
    pub addr: SocketAddr,
    pub fingerprint: [u8; 32],
    /// 门禁过了以后每要一条到宿主的连接，宿主那一端就送到这里。
    pub hosts: mpsc::Receiver<UnixStream>,
}

impl Harness {
    pub fn start(name: &str) -> Self {
        // 路径短一点：别的测试的 socket 路径有长度限制，这里虽然没有 socket，也照同样的习惯。
        let root = std::env::temp_dir().join(format!("rra-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let dirs = Dirs {
            home: Some(root.clone()),
            config: Some(root.clone()),
            data: Some(root.join("runode")),
            cache: Some(root.join("runode/cache")),
        };
        let (tx, hosts) = mpsc::channel();
        let tx = Mutex::new(tx);
        let connect = Arc::new(move || -> io::Result<UnixStream> {
            let (ours, theirs) = UnixStream::pair()?;
            tx.lock().unwrap().send(theirs).map_err(io::Error::other)?;
            Ok(ours)
        });
        let listener = Listener::start(options(&dirs, connect)).unwrap();
        let addr = listener.local_addrs()[0];
        let status = listener_status(&dirs).unwrap().expect("the listener writes its status");
        assert_eq!((status.port, status.host_name.as_str()), (addr.port(), "测试的 Mac"));
        let fingerprint = status.fingerprint.0.try_into().unwrap();
        Self { root, dirs, listener: Some(listener), addr, fingerprint, hosts }
    }

    /// 停掉监听再开一个，返回新的证书指纹。宿主连接换成一个连不上的。
    pub fn restart(&mut self) -> [u8; 32] {
        self.listener = None;
        let listener = Listener::start(options(&self.dirs, Arc::new(|| Err(io::Error::other("no host"))))).unwrap();
        self.addr = listener.local_addrs()[0];
        self.listener = Some(listener);
        listener_status(&self.dirs).unwrap().unwrap().fingerprint.0.try_into().unwrap()
    }

    pub fn phone(&self) -> Phone {
        Phone::connect(self.addr, self.fingerprint).unwrap()
    }

    pub fn ticket(&self) -> PairingTicket {
        PairingTicket::begin(&self.dirs, runode_protocol::remote::PAIRING_TTL).unwrap()
    }

    /// 用 `key` 配对一台设备，返回它的标识。
    pub fn pair(&self, key: &Key) -> DeviceId {
        let ticket = self.ticket();
        let mut phone = self.phone();
        let pair = phone.pair_message(&ticket.secret(), "测试手机", key);
        match phone.ask(&pair) {
            Some(GateHostMsg::RemoteAccepted { device_id }) => device_id,
            other => panic!("pairing failed: {other:?}"),
        }
    }

    /// 等监听要一条到宿主的连接。
    pub fn host(&self) -> UnixStream {
        let host = self.hosts.recv_timeout(PATIENCE).expect("no connection to the host");
        host.set_read_timeout(Some(PATIENCE)).unwrap();
        host
    }

    /// 设备 `device_id` 用 `key` 登录、发了 Hello，接到了宿主上：返回手机和宿主那一端（Hello 已经读掉）。
    pub fn bridged(&self, device_id: DeviceId, key: &Key) -> (Phone, UnixStream) {
        let mut phone = self.phone();
        let auth = phone.auth_message(device_id, key);
        assert_eq!(phone.ask(&auth), Some(GateHostMsg::RemoteAccepted { device_id }));
        phone.send_control(&Phone::hello("mobile"));
        let host = self.host();
        assert!(matches!(read_client_msg(&host), Some(ClientMsg::Hello { .. })));
        (phone, host)
    }
}

fn options(dirs: &Dirs, connect: runode_remote_access::Connect) -> Options {
    Options {
        dirs: dirs.clone(),
        port: 0,
        bind: Bind::Loopback,
        host_name: Some("测试的 Mac".into()),
        advertise: false,
        connect,
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// 一把设备私钥（P-256）。
pub struct Key {
    pair: EcdsaKeyPair,
}

impl Key {
    pub fn generate() -> Self {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        Self { pair: EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng).unwrap() }
    }

    /// X9.63 未压缩的公钥，65 字节。
    pub fn public(&self) -> Vec<u8> {
        self.pair.public_key().as_ref().to_vec()
    }

    /// DER 编码的签名。
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        self.pair.sign(&SystemRandom::new(), message).unwrap().as_ref().to_vec()
    }
}

/// 连上来、过完 TLS 握手、读了 `RemoteChallenge` 的手机。
pub struct Phone {
    pub tls: StreamOwned<ClientConnection, TcpStream>,
    pub nonce: [u8; 32],
    pub exporter: [u8; 32],
    pub host_name: String,
}

impl Phone {
    pub fn connect(addr: SocketAddr, fingerprint: [u8; 32]) -> io::Result<Self> {
        let provider = Arc::new(default_provider());
        let config = ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&TLS13])
            .map_err(io::Error::other)?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Pinned { fingerprint, provider }))
            .with_no_client_auth();
        let conn = ClientConnection::new(Arc::new(config), ServerName::try_from("runode").unwrap())
            .map_err(io::Error::other)?;
        let tcp = TcpStream::connect(addr)?;
        tcp.set_read_timeout(Some(PATIENCE))?;
        let mut tls = StreamOwned::new(conn, tcp);
        while tls.conn.is_handshaking() {
            tls.conn.complete_io(&mut tls.sock)?;
        }
        let exporter = tls.conn.export_keying_material([0u8; 32], EXPORTER_LABEL, None).map_err(io::Error::other)?;
        let mut phone = Self { tls, nonce: [0; 32], exporter, host_name: String::new() };
        match phone.read_gate() {
            Some(GateHostMsg::RemoteChallenge { version: 1, nonce, host_name }) => {
                phone.nonce = nonce.0.try_into().unwrap();
                phone.host_name = host_name;
                Ok(phone)
            }
            other => Err(io::Error::other(format!("expected a challenge, got {other:?}"))),
        }
    }

    pub fn signed(&self, purpose: Purpose) -> Vec<u8> {
        signed_bytes(&self.nonce, &self.exporter, purpose)
    }

    pub fn pair_message(&self, secret: &[u8], name: &str, key: &Key) -> GateClientMsg {
        GateClientMsg::RemotePair {
            secret: Bytes(secret.to_vec()),
            device_name: name.into(),
            public_key: Bytes(key.public()),
            signature: Bytes(key.sign(&self.signed(Purpose::Pair))),
        }
    }

    pub fn auth_message(&self, device_id: DeviceId, key: &Key) -> GateClientMsg {
        GateClientMsg::RemoteAuth { device_id, signature: Bytes(key.sign(&self.signed(Purpose::Auth))) }
    }

    pub fn send_gate(&mut self, message: &GateClientMsg) {
        self.send_control(&serde_json::to_vec(message).unwrap());
    }

    pub fn send_control(&mut self, payload: &[u8]) {
        write_frame(&mut self.tls, FrameKind::Control, 0, payload).unwrap();
        self.tls.flush().unwrap();
    }

    /// 读一条门禁消息；连接关了时为 `None`。
    pub fn read_gate(&mut self) -> Option<GateHostMsg> {
        let frame = read_gate_frame(&mut self.tls).ok()??;
        assert_eq!((frame.kind, frame.channel), (FrameKind::Control, 0));
        Some(frame.message().unwrap())
    }

    /// 发一条门禁消息，读宿主的回话。
    pub fn ask(&mut self, message: &GateClientMsg) -> Option<GateHostMsg> {
        self.send_gate(message);
        self.read_gate()
    }

    /// 连接被对面关了：读到结尾或者出错（不是超时）。
    pub fn closed(&mut self) -> bool {
        loop {
            match read_frame(&mut self.tls) {
                Ok(None) => return true,
                Ok(Some(_)) => {}
                Err(runode_protocol::FrameError::Io(err))
                    if matches!(err.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) =>
                {
                    return false;
                }
                Err(_) => return true,
            }
        }
    }

    pub fn hello(client: &str) -> Vec<u8> {
        format!(r#"{{"type":"hello","protocol":4,"build":"phone","client":"{client}","device":"测试手机"}}"#)
            .into_bytes()
    }

    /// 读一条宿主的控制消息。
    pub fn read_host_msg(&mut self) -> HostMsg {
        let frame = read_frame(&mut self.tls).unwrap().expect("closed");
        frame.message().unwrap()
    }
}

/// 宿主那一端读一条前端的控制消息。
pub fn read_client_msg(host: &UnixStream) -> Option<ClientMsg> {
    read_frame(&mut &*host).ok()?.map(|frame| frame.message().unwrap())
}

/// 只认证书指纹的校验，和手机端一样。
#[derive(Debug)]
struct Pinned {
    fingerprint: [u8; 32],
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let digest = ring::digest::digest(&ring::digest::SHA256, end_entity.as_ref());
        if digest.as_ref() == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("the certificate fingerprint does not match".into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 is not allowed".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// 样例文件里的 base64url 字段。
pub fn fixture_bytes(value: &serde_json::Value) -> Vec<u8> {
    decode_base64url(value.as_str().unwrap()).unwrap()
}
