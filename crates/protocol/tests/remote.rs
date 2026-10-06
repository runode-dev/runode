//! 远程访问的门禁消息、签名字节串和配对 URI 对着 `tests/fixtures/remote` 里的样例：样例是照规格
//! 另外写出来的（不由这里的类型生成），手机端的测试读同一批文件，两边才对得上。测试失败时先看
//! 是不是改了线上格式，要改回来，不要改样例。

use std::{net::IpAddr, path::Path};

use runode_protocol::{
    ClientKind, ClientMsg, Frame, FrameError, FrameKind,
    remote::{
        Bytes, DeviceId, GATE_MAX_PAYLOAD, GateClientMsg, GateHostMsg, PairingUri, Purpose, RejectReason,
        decode_base64url, encode_base64url, read_gate_frame, signed_bytes,
    },
    write_frame,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

fn fixture(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/remote").join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    serde_json::from_str(&text).unwrap()
}

/// 样例读成 `expected`，`expected` 写出来也和样例一样（字段的先后不算）。
fn matches<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(name: &str, expected: &T) {
    let json = fixture(name);
    let read: T = serde_json::from_value(json.clone()).unwrap_or_else(|err| panic!("{name}: {err}"));
    assert_eq!(&read, expected, "{name}");
    assert_eq!(serde_json::to_value(expected).unwrap(), json, "{name}");
}

fn bytes(field: &Value) -> Vec<u8> {
    decode_base64url(field.as_str().unwrap()).unwrap()
}

fn nonce() -> [u8; 32] {
    std::array::from_fn(|i| i as u8)
}

fn exporter() -> [u8; 32] {
    std::array::from_fn(|i| 0xff - i as u8)
}

fn secret() -> [u8; 32] {
    std::array::from_fn(|i| 0xa0 + i as u8)
}

fn device() -> DeviceId {
    "00112233445566778899aabbccddeeff".parse().unwrap()
}

/// 样例里的签名只是格式对的 DER，签的不是什么。
fn signature() -> Bytes {
    let mut der = vec![0x30, 0x44, 0x02, 0x20];
    der.extend([0x11; 32]);
    der.extend([0x02, 0x20]);
    der.extend([0x22; 32]);
    Bytes(der)
}

#[test]
fn host_messages_match_the_fixtures() {
    matches(
        "remote_challenge.json",
        &GateHostMsg::RemoteChallenge {
            version: 1,
            nonce: Bytes(nonce().to_vec()),
            host_name: "Ethan 的 MacBook Pro".into(),
        },
    );
    matches("remote_accepted.json", &GateHostMsg::RemoteAccepted { device_id: device() });
    for (name, reason) in [
        ("unknown_device", RejectReason::UnknownDevice),
        ("bad_signature", RejectReason::BadSignature),
        ("pairing_invalid", RejectReason::PairingInvalid),
        ("rate_limited", RejectReason::RateLimited),
        ("disabled", RejectReason::Disabled),
    ] {
        matches(&format!("remote_rejected_{name}.json"), &GateHostMsg::RemoteRejected { reason });
    }
}

#[test]
fn client_messages_match_the_fixtures() {
    matches("remote_auth.json", &GateClientMsg::RemoteAuth { device_id: device(), signature: signature() });
    let public_key: Vec<u8> = std::iter::once(4).chain((0..64u32).map(|i| (7 * i + 1) as u8)).collect();
    matches(
        "remote_pair.json",
        &GateClientMsg::RemotePair {
            secret: Bytes(secret().to_vec()),
            device_name: "Ethan 的 iPhone".into(),
            public_key: Bytes(public_key),
            signature: signature(),
        },
    );
}

#[test]
fn unknown_reasons_and_messages_still_read() {
    let read: GateHostMsg = serde_json::from_value(fixture("remote_rejected_future_reason.json")).unwrap();
    assert_eq!(read, GateHostMsg::RemoteRejected { reason: RejectReason::Unknown });
    let read: GateHostMsg = serde_json::from_str(r#"{"type":"remote_something_new","x":1}"#).unwrap();
    assert_eq!(read, GateHostMsg::Unknown);
    let read: GateClientMsg = serde_json::from_str(r#"{"type":"remote_something_new"}"#).unwrap();
    assert_eq!(read, GateClientMsg::Unknown);
    // 多出来的字段忽略。
    let read: GateHostMsg = serde_json::from_str(
        r#"{"type":"remote_accepted","device_id":"00112233445566778899aabbccddeeff","more":true}"#,
    )
    .unwrap();
    assert_eq!(read, GateHostMsg::RemoteAccepted { device_id: device() });
}

#[test]
fn malformed_fields_are_rejected() {
    // 设备标识要正好 32 个十六进制数字。
    for id in ["0011", "00112233445566778899aabbccddeefg", "00112233445566778899aabbccddeeff00"] {
        let json = format!(r#"{{"type":"remote_accepted","device_id":"{id}"}}"#);
        assert!(serde_json::from_str::<GateHostMsg>(&json).is_err(), "{id}");
    }
    // 带填充的、标准 base64 的字符都不认。
    for nonce in ["AAE=", "AA+/", "A"] {
        let json = format!(r#"{{"type":"remote_challenge","version":1,"nonce":"{nonce}","host_name":"x"}}"#);
        assert!(serde_json::from_str::<GateHostMsg>(&json).is_err(), "{nonce}");
    }
    assert_eq!(device().to_string(), "00112233445566778899aabbccddeeff");
    assert_eq!("00112233445566778899AABBCCDDEEFF".parse::<DeviceId>(), Ok(device()));
}

#[test]
fn signed_bytes_match_the_fixtures() {
    for (name, purpose) in [("signed_bytes_auth.json", Purpose::Auth), ("signed_bytes_pair.json", Purpose::Pair)] {
        let json = fixture(name);
        assert_eq!(bytes(&json["nonce"]), nonce());
        assert_eq!(bytes(&json["exporter"]), exporter());
        assert_eq!(json["purpose"].as_str().unwrap().as_bytes(), purpose.as_bytes());
        let signed = signed_bytes(&nonce(), &exporter(), purpose);
        assert_eq!(signed, bytes(&json["signed"]), "{name}");
        assert_eq!(signed.len(), 16 + 32 + 32 + 4);
        assert!(signed.starts_with(b"runode-remote-v1"));
    }
}

#[test]
fn base64url_round_trips_and_is_strict() {
    for len in 0..40 {
        let data: Vec<u8> = (0..len).map(|i| (i * 37 + 250) as u8).collect();
        let text = encode_base64url(&data);
        assert!(!text.contains(['=', '+', '/']), "{text}");
        assert_eq!(decode_base64url(&text).unwrap(), data, "{len}");
    }
    assert_eq!(encode_base64url(b""), "");
    assert_eq!(encode_base64url(b"f"), "Zg");
    assert_eq!(encode_base64url(b"fo"), "Zm8");
    assert_eq!(encode_base64url(b"foo"), "Zm9v");
    assert_eq!(encode_base64url(&[0xfb, 0xff]), "-_8");
    // 最后一个字符多出来的位不是 0：不是规范的写法。
    assert!(decode_base64url("Zh").is_err());
    assert!(decode_base64url("Zm9=").is_err());
    assert!(decode_base64url("Zm9v Zg").is_err());
}

#[test]
fn the_pairing_uri_matches_the_fixture() {
    let json = fixture("pair_uri.json");
    let addrs: Vec<IpAddr> =
        json["addrs"].as_array().unwrap().iter().map(|a| a.as_str().unwrap().parse().unwrap()).collect();
    let uri = PairingUri {
        host_name: json["host_name"].as_str().unwrap().into(),
        fingerprint: bytes(&json["fingerprint"]).try_into().unwrap(),
        secret: secret(),
        port: 7866,
        addrs,
        expires_at: json["expires_at"].as_u64().unwrap(),
    };
    assert_eq!(bytes(&json["secret"]), secret());
    let text = json["uri"].as_str().unwrap();
    assert_eq!(uri.to_string(), text);
    assert_eq!(text.parse::<PairingUri>(), Ok(uri.clone()));
    // 参数换了先后、多了不认识的参数照样读；没有地址也行。
    let shuffled = format!(
        "runode://pair?exp=1790000000&addr=&port=7866&x=1&secret={}&fp={}&name=a%2Bb&v=1",
        encode_base64url(&secret()),
        encode_base64url(&uri.fingerprint)
    );
    let read: PairingUri = shuffled.parse().unwrap();
    assert_eq!((read.host_name.as_str(), read.addrs.len()), ("a+b", 0));
    assert!(text.replace("v=1", "v=2").parse::<PairingUri>().is_err());
    assert!(text.replace("&port=7866", "").parse::<PairingUri>().is_err());
    assert!(text.replace("runode://pair", "https://pair").parse::<PairingUri>().is_err());
}

#[test]
fn gate_frames_have_a_small_limit() {
    let message = GateClientMsg::RemoteAuth { device_id: device(), signature: signature() };
    let frame = Frame::control(&message).unwrap();
    let mut stream = Vec::new();
    write_frame(&mut stream, frame.kind, frame.channel, &frame.payload).unwrap();
    let read = read_gate_frame(&mut &stream[..]).unwrap().unwrap();
    assert_eq!((read.kind, read.channel), (FrameKind::Control, 0));
    assert_eq!(read.message::<GateClientMsg>().unwrap(), message);
    let mut big = Vec::new();
    write_frame(&mut big, FrameKind::Control, 0, &vec![b' '; GATE_MAX_PAYLOAD as usize + 1]).unwrap();
    assert!(matches!(read_gate_frame(&mut &big[..]), Err(FrameError::TooLong(_))));
    // 通过门禁后的第一帧是普通的 `Hello`。
    let hello: ClientMsg =
        serde_json::from_str(r#"{"type":"hello","protocol":4,"build":"x","client":"mobile"}"#).unwrap();
    assert!(matches!(hello, ClientMsg::Hello { client: ClientKind::Mobile, .. }));
}
