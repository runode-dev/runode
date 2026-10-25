//! 句柄：APNs 环境、电脑的 UUID 和 device token 用 AES-256-GCM 封起来，前面带上密钥编号和 nonce，
//! 整个 base64url（不带填充）。解得开就是这个中转发出去的，内容没被改过。

use ring::{
    aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey},
    error::Unspecified,
    rand::{SecureRandom, SystemRandom},
};
use runode_protocol::{
    push::{ApnsEnv, MAX_TOKEN_LEN},
    remote::{decode_base64url, encode_base64url},
};

/// 封进句柄的东西。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sealed {
    pub env: ApnsEnv,
    pub machine: [u8; 16],
    pub token: Vec<u8>,
}

/// 绑在密文上的附加数据：换了句柄格式就换它，旧格式的句柄解不开。
const AAD: &[u8] = b"runode-push-v1";
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

/// 封句柄的密钥。第一把用来封，所有的都能解：换密钥时把新的放在最前面，旧的留到手机都重新换过句柄。
pub struct Keys {
    keys: Vec<(u8, LessSafeKey)>,
    rng: SystemRandom,
}

impl Keys {
    /// `spec` 是逗号隔开的 `<编号>=<32 字节的 base64url>`，编号 0 到 255、不重复。
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut keys: Vec<(u8, LessSafeKey)> = Vec::new();
        for item in spec.split(',').map(str::trim).filter(|item| !item.is_empty()) {
            let (id, key) = item.split_once('=').ok_or_else(|| format!("`{item}` is not <id>=<key>"))?;
            let id: u8 = id.trim().parse().map_err(|_| format!("key id `{id}` is not 0 to 255"))?;
            if keys.iter().any(|(known, _)| *known == id) {
                return Err(format!("key id {id} appears twice"));
            }
            let bytes = decode_base64url(key.trim()).map_err(|err| format!("key {id}: {err}"))?;
            if bytes.len() != KEY_LEN {
                return Err(format!("key {id} is {} bytes, not {KEY_LEN}", bytes.len()));
            }
            let key = UnboundKey::new(&AES_256_GCM, &bytes).map_err(|_| format!("key {id} is unusable"))?;
            keys.push((id, LessSafeKey::new(key)));
        }
        if keys.is_empty() {
            return Err("no handle key is given".into());
        }
        Ok(Self { keys, rng: SystemRandom::new() })
    }

    /// 用第一把密钥封；系统随机数出错时失败。
    pub fn seal(&self, sealed: &Sealed) -> Result<String, Unspecified> {
        let (id, key) = &self.keys[0];
        let mut nonce = [0u8; NONCE_LEN];
        self.rng.fill(&mut nonce)?;
        let mut data = Vec::with_capacity(1 + 16 + sealed.token.len() + AES_256_GCM.tag_len());
        data.push(env_byte(sealed.env).ok_or(Unspecified)?);
        data.extend_from_slice(&sealed.machine);
        data.extend_from_slice(&sealed.token);
        key.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::from(AAD), &mut data)?;
        let mut out = Vec::with_capacity(1 + NONCE_LEN + data.len());
        out.push(*id);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&data);
        Ok(encode_base64url(&out))
    }

    /// 解句柄；不是 base64url、密钥编号不认识、被改过时为 `None`。
    pub fn open(&self, handle: &str) -> Option<Sealed> {
        if handle.len() > MAX_TOKEN_LEN {
            return None;
        }
        let bytes = decode_base64url(handle).ok()?;
        let (&id, rest) = bytes.split_first()?;
        let (_, key) = self.keys.iter().find(|(known, _)| *known == id)?;
        let (nonce, sealed) = rest.split_at_checked(NONCE_LEN)?;
        let nonce = Nonce::try_assume_unique_for_key(nonce).ok()?;
        let mut data = sealed.to_vec();
        let plain = key.open_in_place(nonce, Aad::from(AAD), &mut data).ok()?;
        let (&env, rest) = plain.split_first()?;
        let (machine, token) = rest.split_at_checked(16)?;
        let env = match env {
            0 => ApnsEnv::Production,
            1 => ApnsEnv::Development,
            _ => return None,
        };
        Some(Sealed { env, machine: machine.try_into().ok()?, token: token.to_vec() })
    }
}

/// 一把新的密钥，写成 `Keys::parse` 认的 base64url。
pub fn generate_key() -> Result<String, Unspecified> {
    let mut key = [0u8; KEY_LEN];
    SystemRandom::new().fill(&mut key)?;
    Ok(encode_base64url(&key))
}

fn env_byte(env: ApnsEnv) -> Option<u8> {
    match env {
        ApnsEnv::Production => Some(0),
        ApnsEnv::Development => Some(1),
        ApnsEnv::Unknown => None,
    }
}

/// `6F9619FF-8B86-D011-B42D-00C04FC964FF` 这样带连字符的 UUID，不分大小写。
pub fn parse_uuid(text: &str) -> Option<[u8; 16]> {
    let bytes = text.as_bytes();
    if bytes.len() != 36 || [8, 13, 18, 23].iter().any(|&i| bytes[i] != b'-') {
        return None;
    }
    let digits: String = text.chars().filter(|&c| c != '-').collect();
    decode_hex(&digits)?.try_into().ok()
}

/// 十六进制（不分大小写）解成字节；长度是奇数或有别的字符时为 `None`。
pub fn decode_hex(text: &str) -> Option<Vec<u8>> {
    // `from_str_radix` 还认开头的 `+`，先挡掉别的字符。
    if !text.len().is_multiple_of(2) || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

/// 小写十六进制。
pub fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sealed() -> Sealed {
        Sealed { env: ApnsEnv::Development, machine: [7; 16], token: vec![0x80, 0xf0, 0x0a, 0xff] }
    }

    #[test]
    fn handles_round_trip_and_stay_base64url() {
        let keys = Keys::parse(&format!("3={}", generate_key().unwrap())).unwrap();
        let handle = keys.seal(&sealed()).unwrap();
        assert!(handle.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'), "{handle}");
        assert_eq!(keys.open(&handle), Some(sealed()));
        // nonce 每次都换，同样的内容封出来不一样。
        assert_ne!(keys.seal(&sealed()).unwrap(), handle);
    }

    /// 换密钥：新的放前面，旧密钥封的句柄照样解得开；去掉旧密钥后就解不开了。
    #[test]
    fn old_keys_still_open_their_handles() {
        let (old, new) = (generate_key().unwrap(), generate_key().unwrap());
        let handle = Keys::parse(&format!("1={old}")).unwrap().seal(&sealed()).unwrap();
        assert_eq!(Keys::parse(&format!("2={new},1={old}")).unwrap().open(&handle), Some(sealed()));
        assert_eq!(Keys::parse(&format!("2={new}")).unwrap().open(&handle), None);
    }

    #[test]
    fn tampered_handles_do_not_open() {
        let keys = Keys::parse(&format!("0={}", generate_key().unwrap())).unwrap();
        let handle = keys.seal(&sealed()).unwrap();
        let mut bytes = decode_base64url(&handle).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        assert_eq!(keys.open(&encode_base64url(&bytes)), None);
        assert_eq!(keys.open("not a handle"), None);
        assert_eq!(keys.open(""), None);
        assert_eq!(keys.open(&"A".repeat(MAX_TOKEN_LEN + 4)), None);
    }

    #[test]
    fn key_specs_are_checked() {
        let key = generate_key().unwrap();
        assert!(Keys::parse("").is_err());
        assert!(Keys::parse(&format!("1={key},1={key}")).is_err());
        assert!(Keys::parse(&format!("256={key}")).is_err());
        assert!(Keys::parse("1=AAAA").is_err());
        assert!(Keys::parse(&format!(" 1 = {key} , ")).is_ok());
    }

    #[test]
    fn uuids_and_hex_parse_strictly() {
        let uuid = parse_uuid("6F9619FF-8B86-D011-B42D-00C04FC964FF").unwrap();
        assert_eq!(parse_uuid("6f9619ff-8b86-d011-b42d-00c04fc964ff"), Some(uuid));
        assert_eq!(encode_hex(&uuid), "6f9619ff8b86d011b42d00c04fc964ff");
        assert_eq!(parse_uuid("6F9619FF8B86D011B42D00C04FC964FF"), None);
        assert_eq!(parse_uuid("6F9619FF-8B86-D011-B42D-00C04FC964FG"), None);
        assert_eq!(decode_hex("80f00A"), Some(vec![0x80, 0xf0, 0x0a]));
        assert_eq!(decode_hex("80f"), None);
        assert_eq!(decode_hex("+0"), None);
        assert_eq!(decode_hex("é0"), None);
    }
}
