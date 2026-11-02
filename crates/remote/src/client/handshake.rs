//! 一条连接从 TCP 到过了门禁：TLS 1.3 只认证书指纹，读 `RemoteChallenge`，发登录或配对的消息，读回话。
//! 这一段的读写都不超过调用方给的截止时刻。

use std::{
    io::{self, Read, Write},
    net::TcpStream,
    sync::Arc,
    time::{Duration, Instant},
};

use ring::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair as _},
};
use runode_protocol::{
    FrameKind,
    remote::{
        Bytes, DeviceId, DeviceKind, EXPORTER_LABEL, EXPORTER_LEN, FINGERPRINT_LEN, GATE_VERSION, GateClientMsg,
        GateHostMsg, NONCE_LEN, Purpose, SECRET_LEN, read_gate_frame, signed_bytes,
    },
    write_frame,
};
use rustls::{
    CertificateError, ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{CryptoProvider, ring::default_provider, verify_tls13_signature},
    pki_types::{CertificateDer, ServerName, UnixTime},
    version::TLS13,
};

use super::DialError;

/// 证书里不看主机名，随便给一个合法的。
const SERVER_NAME: &str = "runode";

/// 握完手、读了 `RemoteChallenge` 的连接。
pub(super) struct Challenged {
    tls: StreamOwned<ClientConnection, Timed>,
    nonce: [u8; NONCE_LEN],
    exporter: [u8; EXPORTER_LEN],
    pub(super) host_name: String,
    pub(super) node: Option<String>,
}

/// 过了门禁的连接：TLS 状态和底下的 TCP（读写的超时已经去掉）。
pub(super) struct Gated {
    pub(super) conn: ClientConnection,
    pub(super) tcp: TcpStream,
    pub(super) device_id: DeviceId,
}

/// 在 `tcp` 上握手，证书指纹要是 `fingerprint`，再读 `RemoteChallenge`。
pub(super) fn open(
    tcp: TcpStream,
    fingerprint: &[u8; FINGERPRINT_LEN],
    until: Instant,
) -> Result<Challenged, DialError> {
    let provider = Arc::new(default_provider());
    let config = ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&TLS13])
        .map_err(|err| DialError::Protocol(err.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned { fingerprint: *fingerprint, provider }))
        .with_no_client_auth();
    let name = ServerName::try_from(SERVER_NAME).map_err(|err| DialError::Protocol(err.to_string()))?;
    let conn = ClientConnection::new(Arc::new(config), name).map_err(|err| DialError::Protocol(err.to_string()))?;
    let mut tls = StreamOwned::new(conn, Timed { tcp, until });
    while tls.conn.is_handshaking() {
        tls.conn.complete_io(&mut tls.sock).map_err(handshake_error)?;
    }
    let exporter = tls
        .conn
        .export_keying_material([0u8; EXPORTER_LEN], EXPORTER_LABEL, None)
        .map_err(|err| DialError::Protocol(err.to_string()))?;
    match read(&mut tls)? {
        GateHostMsg::RemoteChallenge { version, nonce, host_name, node } => {
            if version != GATE_VERSION {
                return Err(DialError::Protocol(format!("unsupported gate version {version}")));
            }
            let nonce = nonce.0.try_into().map_err(|_| DialError::Protocol("a malformed challenge".into()))?;
            Ok(Challenged { tls, nonce, exporter, host_name, node })
        }
        other => Err(DialError::Protocol(format!("expected a challenge, got {other:?}"))),
    }
}

impl Challenged {
    /// 配对过的设备 `device_id` 用私钥 `key` 登录。
    pub(super) fn auth(self, device_id: DeviceId, key: &EcdsaKeyPair) -> Result<Gated, DialError> {
        let signature = sign(key, &signed_bytes(&self.nonce, &self.exporter, Purpose::Auth))?;
        self.finish(&GateClientMsg::RemoteAuth { device_id, signature: Bytes(signature) })
    }

    /// 用配对口令 `secret` 配对，这边叫 `name`，以后用 `key` 登录。
    pub(super) fn pair(self, secret: &[u8; SECRET_LEN], name: &str, key: &EcdsaKeyPair) -> Result<Gated, DialError> {
        let signature = sign(key, &signed_bytes(&self.nonce, &self.exporter, Purpose::Pair))?;
        self.finish(&GateClientMsg::RemotePair {
            secret: Bytes(secret.to_vec()),
            device_name: name.into(),
            public_key: Bytes(key.public_key().as_ref().to_vec()),
            signature: Bytes(signature),
            device_kind: DeviceKind::Desktop,
        })
    }

    fn finish(mut self, message: &GateClientMsg) -> Result<Gated, DialError> {
        let payload = serde_json::to_vec(message).map_err(|err| DialError::Protocol(err.to_string()))?;
        write_frame(&mut self.tls, FrameKind::Control, 0, &payload).map_err(|err| unreachable(err.to_string()))?;
        self.tls.flush().map_err(|err| unreachable(err.to_string()))?;
        match read(&mut self.tls)? {
            GateHostMsg::RemoteAccepted { device_id } => {
                let StreamOwned { conn, sock } = self.tls;
                sock.tcp.set_read_timeout(None).map_err(DialError::Io)?;
                sock.tcp.set_write_timeout(None).map_err(DialError::Io)?;
                Ok(Gated { conn, tcp: sock.tcp, device_id })
            }
            GateHostMsg::RemoteRejected { reason } => Err(DialError::Rejected(reason)),
            other => Err(DialError::Protocol(format!("expected the gate's answer, got {other:?}"))),
        }
    }
}

/// 读一条门禁消息：要是通道 0 的控制帧。
fn read(tls: &mut StreamOwned<ClientConnection, Timed>) -> Result<GateHostMsg, DialError> {
    let frame = read_gate_frame(tls)
        .map_err(|err| unreachable(err.to_string()))?
        .ok_or_else(|| unreachable("closed during the gate".into()))?;
    if frame.kind != FrameKind::Control || frame.channel != 0 {
        return Err(DialError::Protocol("a gate message must be a control frame on channel 0".into()));
    }
    frame.message().map_err(|err| DialError::Protocol(err.to_string()))
}

fn sign(key: &EcdsaKeyPair, message: &[u8]) -> Result<Vec<u8>, DialError> {
    key.sign(&SystemRandom::new(), message)
        .map(|signature| signature.as_ref().to_vec())
        .map_err(|_| DialError::Io(io::Error::other("cannot sign the gate challenge")))
}

fn unreachable(last: String) -> DialError {
    DialError::Unreachable { last }
}

/// 握手失败：证书指纹对不上的报 `FingerprintMismatch`，别的（连不上、超时、对面不是 TLS）都算连不上。
fn handshake_error(err: io::Error) -> DialError {
    let mismatch = err.get_ref().and_then(|inner| inner.downcast_ref::<rustls::Error>())
        == Some(&rustls::Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure));
    if mismatch { DialError::FingerprintMismatch } else { unreachable(err.to_string()) }
}

/// 只认证书指纹的校验：叶子证书 DER 的 SHA-256 要等于 `fingerprint`，不看 CA、主机名和有效期。
#[derive(Debug)]
struct Pinned {
    fingerprint: [u8; FINGERPRINT_LEN],
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
            Err(rustls::Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure))
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

/// 读写都不超过 `until` 的 TCP 连接，门禁阶段给 rustls 用。
struct Timed {
    tcp: TcpStream,
    until: Instant,
}

impl Timed {
    fn left(&self) -> io::Result<Duration> {
        self.until
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "timed out during the gate"))
    }
}

/// 超时（`SO_RCVTIMEO` 到了时系统报的是 `WouldBlock`）一律报成 `TimedOut`，rustls 不会当成「等一下再试」。
fn timed_out(err: io::Error) -> io::Error {
    if err.kind() == io::ErrorKind::WouldBlock {
        io::Error::new(io::ErrorKind::TimedOut, "timed out during the gate")
    } else {
        err
    }
}

impl Read for Timed {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.tcp.set_read_timeout(Some(self.left()?))?;
        self.tcp.read(buf).map_err(timed_out)
    }
}

impl Write for Timed {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.tcp.set_write_timeout(Some(self.left()?))?;
        self.tcp.write(buf).map_err(timed_out)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.tcp.flush()
    }
}

/// 新生成一把设备私钥，返回 PKCS#8 DER。
pub(super) fn generate_key() -> io::Result<Vec<u8>> {
    EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &SystemRandom::new())
        .map(|pkcs8| pkcs8.as_ref().to_vec())
        .map_err(|_| io::Error::other("cannot generate a device key"))
}

/// 读存着的设备私钥。
pub(super) fn load_key(pkcs8: &[u8]) -> io::Result<EcdsaKeyPair> {
    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8, &SystemRandom::new())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "the stored device key is unreadable"))
}
