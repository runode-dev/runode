//! 远程访问：手机（以后也有别的设备）经局域网或 Tailscale 这类网络连到 Mac 上的宿主。这里是线上
//! 格式的正式定义，手机端照这里实现，任何一边改了下面写的字节格式，另一边就连不上。
//!
//! # 连接
//!
//! Mac 上另开一个 TCP 监听（默认端口 `DEFAULT_PORT`，配置项 `remote-access-port`；总开关
//! `remote-access`，默认关），IPv4 和 IPv6 的所有网卡都听。连上来先做 TLS 1.3（不接受 1.2）。
//! 服务端证书是 Mac 自己生成的自签证书（ECDSA P-256），第一次开远程访问时生成，之后不变。客户端
//! 不走 CA 校验也不校验主机名，只比对证书指纹：**指纹 = 叶子证书 DER 的 SHA-256**
//! （`FINGERPRINT_LEN` 字节），扫码配对时拿到，之后固定。
//!
//! TLS 握手完成后，连接上跑的就是 `frame` 的帧。先是一段「门禁」：门禁消息全部是
//! `FrameKind::Control`、通道 0 的帧，载荷是 JSON，带 `type` 字段区分种类，和 `ClientMsg`、
//! `HostMsg` 一样的风格；宿主发的是 `GateHostMsg`，客户端发的是 `GateClientMsg`。门禁阶段从 TLS
//! 握手开始算超时 `GATE_TIMEOUT`，单帧载荷上限 `GATE_MAX_PAYLOAD`（见 `read_frame_limited`）。
//! 所有二进制字段一律 base64url、不带填充（RFC 4648 §5，见 `Bytes`、`encode_base64url`）。
//!
//! # 门禁
//!
//! 1. TLS 握手完成，宿主立刻发 `GateHostMsg::RemoteChallenge`：
//!    `{"type":"remote_challenge","version":1,"nonce":"…","host_name":"Ethan 的 MacBook Pro"}`。
//!    `version` 是这套门禁的版本（`GATE_VERSION`），客户端不认识就断开并提示升级；`nonce` 是
//!    `NONCE_LEN` 字节的随机数。
//! 2. 客户端回两种之一：
//!    - 配对过的设备登录（`GateClientMsg::RemoteAuth`）：
//!      `{"type":"remote_auth","device_id":"…","signature":"…"}`；`device_id` 是配对时宿主发的
//!      `DeviceId`。
//!    - 第一次配对（`GateClientMsg::RemotePair`）：
//!      `{"type":"remote_pair","secret":"…","device_name":"Ethan 的 iPhone","public_key":"…","signature":"…"}`；
//!      `secret` 是二维码里的配对口令（`SECRET_LEN` 字节），`public_key` 是设备的 P-256 公钥，
//!      X9.63 未压缩格式（`PUBLIC_KEY_LEN` 字节，`0x04` 开头）。
//! 3. 签名是 ECDSA P-256 + SHA-256，DER 编码（CryptoKit 的 `derRepresentation`，ring 的
//!    `ECDSA_P256_SHA256_ASN1`），被签的字节串见 `signed_bytes`：`SIGNATURE_DOMAIN`、`nonce`、
//!    这条 TLS 连接的 exporter（label `EXPORTER_LABEL`，不带 context，`EXPORTER_LEN` 字节）、
//!    用途（`Purpose`）依次拼起来。exporter 把签名绑在这条 TLS 连接上，中间人转发不了。
//!    TLS 1.3 里不带 context 和空 context 等价。Network.framework 用
//!    `sec_protocol_metadata_create_secret`，rustls 用 `export_keying_material(…, None)`。
//! 4. 宿主回 `GateHostMsg::RemoteAccepted`（`{"type":"remote_accepted","device_id":"…"}`，配对时是
//!    新分配的标识），或者 `GateHostMsg::RemoteRejected`
//!    （`{"type":"remote_rejected","reason":{"kind":"unknown_device"}}`，原因见 `RejectReason`），
//!    拒绝后宿主关闭连接。
//! 5. 通过以后，客户端发的第一帧必须是 `ClientMsg::Hello`，`client` 是 `ClientKind::Mobile`，否则
//!    宿主直接断开。之后整条连接就是普通的连接，和本机 Unix socket 上的一样，只有一个例外：管宿主
//!    本身、或者只有本机的 app 才该做的请求——`ClientMsg::Shutdown`、交接用的 `Handoff`、
//!    `HandoffReady`、`HandoffAbort`、`HandoffDone`，以及 `UiReply`、`SetOptions`、`SetTheme`——不转给
//!    宿主，宿主在发往客户端的方向、两帧之间回一条 `HostMsg::Error`（这几种都不带 `req` 和会话，
//!    `Error` 也就不带），连接照旧。关单个会话（`Kill`）、开会话、输入、改尺寸这些照常。
//!
//! # 配对
//!
//! 配对口令是 `SECRET_LEN` 字节的随机数，一次性，`PAIRING_TTL` 内有效，连续
//! `PAIRING_MAX_FAILURES` 次口令错误后作废。配对成功后宿主给设备分配 `DeviceId`，记进设备表；
//! 撤销就是从表里删掉，这台设备连着的连接几秒内断开。二维码里是一个 `PairingUri`。
//!
//! # 发现
//!
//! 开着远程访问时，Mac 用 Bonjour 公布 `BONJOUR_SERVICE`，端口是监听端口，实例名是主机名，TXT
//! 记录是 `v=1`（`GATE_VERSION`）和 `fp=<证书指纹>`。客户端按存下的指纹找这台 Mac 现在的地址，
//! 找不到再试上次成功的地址和二维码里的地址。
//!
//! # 以后怎么改
//!
//! 和 `message` 一样：已有字段的名字、类型和含义不改，新加的字段带 `#[serde(default)]`；
//! `RejectReason` 可以加新的取值，旧的一方读成 `RejectReason::Unknown`，当作一般的失败。改了
//! 含义或者签名的拼法时加 `GATE_VERSION`。`tests/fixtures/remote` 里存着每种消息、签名字节串和
//! 配对 URI 的样例，手机端的测试读同一批文件。

use std::{fmt, net::IpAddr, str::FromStr, time::Duration};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::frame::{Frame, FrameError, read_frame_limited};

/// 门禁的版本，见 `GateHostMsg::RemoteChallenge::version`。
pub const GATE_VERSION: u32 = 1;
/// 配置项 `remote-access-port` 的默认值。
pub const DEFAULT_PORT: u16 = 7866;
/// 被签的字节串开头的 16 个 ASCII 字节，见 `signed_bytes`。
pub const SIGNATURE_DOMAIN: &[u8; 16] = b"runode-remote-v1";
/// 取 TLS exporter 用的 label，不带 context，见 `signed_bytes`。
pub const EXPORTER_LABEL: &[u8] = b"EXPORTER-runode-remote";
/// TLS exporter 取多少字节。
pub const EXPORTER_LEN: usize = 32;
/// `GateHostMsg::RemoteChallenge::nonce` 的字节数。
pub const NONCE_LEN: usize = 32;
/// 配对口令的字节数。
pub const SECRET_LEN: usize = 32;
/// 设备公钥的字节数：P-256，X9.63 未压缩格式（`0x04 | X | Y`）。
pub const PUBLIC_KEY_LEN: usize = 65;
/// 证书指纹（叶子证书 DER 的 SHA-256）的字节数。
pub const FINGERPRINT_LEN: usize = 32;
/// 门禁阶段（从 TLS 握手开始到宿主回话）最长多久。
pub const GATE_TIMEOUT: Duration = Duration::from_secs(10);
/// 门禁阶段单帧载荷的上限。
pub const GATE_MAX_PAYLOAD: u32 = 16 << 10;
/// 配对口令多久后过期。
pub const PAIRING_TTL: Duration = Duration::from_secs(5 * 60);
/// 配对口令连续错几次后作废。
pub const PAIRING_MAX_FAILURES: u32 = 5;
/// Bonjour 公布的服务类型。
pub const BONJOUR_SERVICE: &str = "_runode._tcp";
/// 配对 URI 的开头，见 `PairingUri`。
pub const PAIRING_URI_PREFIX: &str = "runode://pair?";

/// 读一帧门禁消息：载荷上限是 `GATE_MAX_PAYLOAD`，其余同 `read_frame`。
pub fn read_gate_frame(reader: &mut impl std::io::Read) -> Result<Option<Frame>, FrameError> {
    read_frame_limited(reader, GATE_MAX_PAYLOAD)
}

/// 宿主在门禁阶段发的消息。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GateHostMsg {
    /// TLS 握手完成后宿主发的第一条消息。`host_name` 是给人看的主机名（macOS 的电脑名）。
    RemoteChallenge { version: u32, nonce: Bytes, host_name: String },
    /// 门禁通过了；配对时 `device_id` 是新分配给这台设备的，客户端存下来以后登录用。
    RemoteAccepted { device_id: DeviceId },
    /// 门禁没通过，宿主接着关闭连接。
    RemoteRejected { reason: RejectReason },
    /// 比自己新的宿主才有的消息，客户端按通用失败处理。
    #[serde(other)]
    Unknown,
}

/// 客户端在门禁阶段发的消息，回 `GateHostMsg::RemoteChallenge`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GateClientMsg {
    /// 配对过的设备登录：`signature` 签的是 `signed_bytes(…, Purpose::Auth)`，用配对时登记的公钥验。
    RemoteAuth { device_id: DeviceId, signature: Bytes },
    /// 第一次配对：`secret` 是二维码里的配对口令，`public_key` 是设备以后登录用的公钥，
    /// `signature` 用这把公钥对应的私钥签 `signed_bytes(…, Purpose::Pair)`，证明设备拿着私钥。
    RemotePair { secret: Bytes, device_name: String, public_key: Bytes, signature: Bytes },
    /// 比自己新的客户端才有的消息，宿主断开。
    #[serde(other)]
    Unknown,
}

/// 宿主为什么拒绝，见 `GateHostMsg::RemoteRejected`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RejectReason {
    /// 没配对过，或者已经被撤销。
    UnknownDevice,
    /// 签名验不过。
    BadSignature,
    /// 配对口令不对、过期了或者已经用过。
    PairingInvalid,
    /// 这个地址失败太多次，过一会儿再试。
    RateLimited,
    /// 远程访问正在关掉。
    Disabled,
    /// 比自己新的宿主才有的原因，客户端按通用失败处理。
    #[serde(other)]
    Unknown,
}

/// 签名的用途，拼在被签的字节串末尾，见 `signed_bytes`：登录的签名拿去配对（或者反过来）对不上。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// `GateClientMsg::RemoteAuth`，ASCII `auth`。
    Auth,
    /// `GateClientMsg::RemotePair`，ASCII `pair`。
    Pair,
}

impl Purpose {
    pub fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Auth => b"auth",
            Self::Pair => b"pair",
        }
    }
}

/// 设备要签的字节串：`SIGNATURE_DOMAIN`（16 字节）、`nonce`（32 字节，`RemoteChallenge` 里的）、
/// `exporter`（32 字节，这条 TLS 连接的 exporter，见 `EXPORTER_LABEL`）、用途的 ASCII，依次拼接，
/// 中间没有分隔和长度。
pub fn signed_bytes(nonce: &[u8; NONCE_LEN], exporter: &[u8; EXPORTER_LEN], purpose: Purpose) -> Vec<u8> {
    let purpose = purpose.as_bytes();
    let mut bytes = Vec::with_capacity(SIGNATURE_DOMAIN.len() + NONCE_LEN + EXPORTER_LEN + purpose.len());
    bytes.extend_from_slice(SIGNATURE_DOMAIN);
    bytes.extend_from_slice(nonce);
    bytes.extend_from_slice(exporter);
    bytes.extend_from_slice(purpose);
    bytes
}

/// 配对时宿主发给设备的标识：16 字节随机数，写成 32 个小写十六进制数字。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceId(pub [u8; 16]);

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// `DeviceId` 的写法不对：不是正好 32 个十六进制数字。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidDeviceId;

impl fmt::Display for InvalidDeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "a device id is 32 hexadecimal digits")
    }
}

impl std::error::Error for InvalidDeviceId {}

impl FromStr for DeviceId {
    type Err = InvalidDeviceId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 32 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(InvalidDeviceId);
        }
        let mut bytes = [0u8; 16];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|_| InvalidDeviceId)?;
        }
        Ok(Self(bytes))
    }
}

impl Serialize for DeviceId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for DeviceId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// 门禁消息里的二进制字段，JSON 里写成 base64url、不带填充的字符串。长度由用到它的地方检查。
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Bytes(pub Vec<u8>);

impl fmt::Debug for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Bytes({})", encode_base64url(&self.0))
    }
}

impl From<&[u8]> for Bytes {
    fn from(bytes: &[u8]) -> Self {
        Self(bytes.to_vec())
    }
}

impl Serialize for Bytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&encode_base64url(&self.0))
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        decode_base64url(&s).map(Self).map_err(serde::de::Error::custom)
    }
}

const BASE64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// base64url、不带填充（RFC 4648 §5）。
pub fn encode_base64url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        // 3 字节出 4 个字符，不满 3 字节时只出用到的那几个（1 字节 2 个、2 字节 3 个）。
        for i in 0..=chunk.len() {
            out.push(char::from(BASE64URL[(n >> (18 - 6 * i) & 0x3f) as usize]));
        }
    }
    out
}

/// `decode_base64url` 读不了：有字母表以外的字符（包括填充的 `=`）、长度不可能，或者最后一个
/// 字符多出来的位不是 0（不是规范的写法）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidBase64;

impl fmt::Display for InvalidBase64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not unpadded base64url")
    }
}

impl std::error::Error for InvalidBase64 {}

/// 解 base64url、不带填充的字符串，只认规范的写法，见 `InvalidBase64`。
pub fn decode_base64url(text: &str) -> Result<Vec<u8>, InvalidBase64> {
    let value = |c: u8| -> Result<u32, InvalidBase64> {
        BASE64URL.iter().position(|&a| a == c).map(|v| v as u32).ok_or(InvalidBase64)
    };
    let text = text.as_bytes();
    if text.len() % 4 == 1 {
        return Err(InvalidBase64);
    }
    let mut out = Vec::with_capacity(text.len() / 4 * 3 + 2);
    for chunk in text.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= value(c)? << (18 - 6 * i);
        }
        let bytes = chunk.len() - 1;
        // 不满 4 个字符时，最后一个字符里没用上的位要是 0。
        if n & (0x00ff_ffff >> (8 * bytes)) != 0 {
            return Err(InvalidBase64);
        }
        out.extend((0..bytes).map(|i| (n >> (16 - 8 * i)) as u8));
    }
    Ok(out)
}

/// 二维码里的配对 URI：
///
/// ```text
/// runode://pair?v=1&name=<主机名，百分号编码>&fp=<证书指纹>&secret=<配对口令>&port=<端口>&addr=<地址1>,<地址2>,...&exp=<Unix 秒>
/// ```
///
/// 指纹和口令是 base64url、不带填充；`addr` 是生成时 Mac 所有非回环、非链路本地的 IPv4、IPv6
/// 地址（IPv6 不带方括号），逗号隔开，可以为空；`exp` 是口令过期的时刻。读的时候参数的先后不限，
/// 不认识的参数忽略。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingUri {
    pub host_name: String,
    pub fingerprint: [u8; FINGERPRINT_LEN],
    pub secret: [u8; SECRET_LEN],
    pub port: u16,
    pub addrs: Vec<IpAddr>,
    /// 口令过期的时刻，Unix 秒。
    pub expires_at: u64,
}

impl fmt::Display for PairingUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let addrs: Vec<String> = self.addrs.iter().map(IpAddr::to_string).collect();
        write!(
            f,
            "{PAIRING_URI_PREFIX}v={GATE_VERSION}&name={}&fp={}&secret={}&port={}&addr={}&exp={}",
            percent_encode(&self.host_name),
            encode_base64url(&self.fingerprint),
            encode_base64url(&self.secret),
            self.port,
            addrs.join(","),
            self.expires_at,
        )
    }
}

/// 读不了的配对 URI，带着哪里不对。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidPairingUri(pub String);

impl fmt::Display for InvalidPairingUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid pairing uri: {}", self.0)
    }
}

impl std::error::Error for InvalidPairingUri {}

impl FromStr for PairingUri {
    type Err = InvalidPairingUri;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = |what: &str| InvalidPairingUri(what.into());
        let query = s.strip_prefix(PAIRING_URI_PREFIX).ok_or_else(|| bad("not a runode://pair uri"))?;
        let mut params = std::collections::HashMap::new();
        for pair in query.split('&') {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            params.insert(key, value);
        }
        let param = |key: &str| params.get(key).copied().ok_or_else(|| bad(&format!("no {key}")));
        if param("v")? != GATE_VERSION.to_string() {
            return Err(bad("unsupported version"));
        }
        let fixed = |key: &str| -> Result<[u8; 32], InvalidPairingUri> {
            let bytes = decode_base64url(param(key)?).map_err(|_| bad(&format!("{key} is not base64url")))?;
            bytes.try_into().map_err(|_| bad(&format!("{key} has the wrong length")))
        };
        let addrs = match param("addr")? {
            "" => Vec::new(),
            list => list.split(',').map(|addr| addr.parse()).collect::<Result<_, _>>().map_err(|_| bad("bad addr"))?,
        };
        Ok(Self {
            host_name: percent_decode(param("name")?).ok_or_else(|| bad("bad name"))?,
            fingerprint: fixed("fp")?,
            secret: fixed("secret")?,
            port: param("port")?.parse().map_err(|_| bad("bad port"))?,
            addrs,
            expires_at: param("exp")?.parse().map_err(|_| bad("bad exp"))?,
        })
    }
}

/// 百分号编码：RFC 3986 的非保留字符（字母、数字、`-._~`）原样，其余按 UTF-8 的每个字节写成
/// `%XX`（大写）。
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for &byte in text.as_bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn percent_decode(text: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((&first, tail)) = rest.split_first() {
        if first == b'%' {
            let hex = tail.get(..2).filter(|hex| hex.iter().all(u8::is_ascii_hexdigit))?;
            bytes.push(u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?);
            rest = &tail[2..];
        } else {
            bytes.push(first);
            rest = tail;
        }
    }
    String::from_utf8(bytes).ok()
}
