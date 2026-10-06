import Foundation

// 远程访问的门禁：TLS 握手之后、`Hello` 之前在同一条连接上来回的几条消息。都是通道 0 的
// `FrameKind.control` 帧，载荷是带 `type` 字段的 JSON，二进制字段一律 base64url 不带填充。
// 字节格式两端必须一致，见远程访问规格（v1）。

/// 门禁的版本。宿主的 `remote_challenge` 里报的版本不是它时断开，提示升级。
public let gateVersion: UInt32 = 1

/// 宿主为什么拒绝。不认识的取值读成 `unknown`，按通用失败处理。
public enum GateRejection: Hashable, Sendable, Codable {
    /// 没配对，或者已经在 Mac 上撤销了。
    case unknownDevice
    case badSignature
    /// 配对口令不对、过期或已经用过。
    case pairingInvalid
    case rateLimited
    /// Mac 上关了远程访问。
    case disabled
    case unknown(String)

    var wireName: String {
        switch self {
        case .unknownDevice: "unknown_device"
        case .badSignature: "bad_signature"
        case .pairingInvalid: "pairing_invalid"
        case .rateLimited: "rate_limited"
        case .disabled: "disabled"
        case .unknown(let kind): kind
        }
    }

    private enum Keys: String, CodingKey { case kind }

    public init(from decoder: any Decoder) throws {
        let kind = try decoder.container(keyedBy: Keys.self).decode(String.self, forKey: .kind)
        self =
            [GateRejection.unknownDevice, .badSignature, .pairingInvalid, .rateLimited, .disabled]
            .first { $0.wireName == kind } ?? .unknown(kind)
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        try c.encode(wireName, forKey: .kind)
    }
}

/// 门禁阶段的消息，两个方向的都在这里。
public enum GateMessage: Hashable, Sendable, Codable {
    /// 宿主在 TLS 握手后立刻发：门禁版本、32 字节随机数、主机名。
    case challenge(version: UInt32, nonce: Data, hostName: String)
    /// 已配对的设备登录。
    case auth(deviceId: String, signature: Data)
    /// 第一次配对：二维码里的口令、设备名、P-256 公钥（X9.63 未压缩 65 字节）。
    case pair(secret: Data, deviceName: String, publicKey: Data, signature: Data)
    case accepted(deviceId: String)
    case rejected(GateRejection)
    /// 不认识的种类。
    case unknown(String)

    private enum Keys: String, CodingKey {
        case type, version, nonce, secret, signature, reason
        case hostName = "host_name"
        case deviceId = "device_id"
        case deviceName = "device_name"
        case publicKey = "public_key"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        let type = try c.decode(String.self, forKey: .type)
        func binary(_ key: Keys) throws -> Data {
            let text = try c.decode(String.self, forKey: key)
            guard let data = Base64URL.decode(text) else {
                throw DecodingError.dataCorruptedError(
                    forKey: key, in: c, debugDescription: "\(key.stringValue) is not base64url")
            }
            return data
        }
        switch type {
        case "remote_challenge":
            self = .challenge(
                version: try c.decode(UInt32.self, forKey: .version),
                nonce: try binary(.nonce),
                hostName: try c.decodeIfPresent(String.self, forKey: .hostName) ?? "")
        case "remote_auth":
            self = .auth(deviceId: try c.decode(String.self, forKey: .deviceId), signature: try binary(.signature))
        case "remote_pair":
            self = .pair(
                secret: try binary(.secret),
                deviceName: try c.decode(String.self, forKey: .deviceName),
                publicKey: try binary(.publicKey),
                signature: try binary(.signature))
        case "remote_accepted":
            self = .accepted(deviceId: try c.decode(String.self, forKey: .deviceId))
        case "remote_rejected":
            self = .rejected(try c.decode(GateRejection.self, forKey: .reason))
        default:
            self = .unknown(type)
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        switch self {
        case let .challenge(version, nonce, hostName):
            try c.encode("remote_challenge", forKey: .type)
            try c.encode(version, forKey: .version)
            try c.encode(Base64URL.encode(nonce), forKey: .nonce)
            try c.encode(hostName, forKey: .hostName)
        case let .auth(deviceId, signature):
            try c.encode("remote_auth", forKey: .type)
            try c.encode(deviceId, forKey: .deviceId)
            try c.encode(Base64URL.encode(signature), forKey: .signature)
        case let .pair(secret, deviceName, publicKey, signature):
            try c.encode("remote_pair", forKey: .type)
            try c.encode(Base64URL.encode(secret), forKey: .secret)
            try c.encode(deviceName, forKey: .deviceName)
            try c.encode(Base64URL.encode(publicKey), forKey: .publicKey)
            try c.encode(Base64URL.encode(signature), forKey: .signature)
        case let .accepted(deviceId):
            try c.encode("remote_accepted", forKey: .type)
            try c.encode(deviceId, forKey: .deviceId)
        case let .rejected(reason):
            try c.encode("remote_rejected", forKey: .type)
            try c.encode(reason, forKey: .reason)
        case let .unknown(type):
            try c.encode(type, forKey: .type)
        }
    }
}

/// 签名的用途，拼进被签的字节串末尾。
public enum GatePurpose: String, Sendable {
    case auth
    case pair
}

/// 门禁里被签的字节串：`"runode-remote-v1"`（16 字节 ASCII）、`nonce`（32 字节）、TLS exporter
/// （32 字节，label `EXPORTER-runode-remote`、不带 context）、用途（ASCII）依次拼起来。exporter
/// 把签名绑在这条 TLS 连接上，中间人转发不了。签名是 ECDSA P-256 + SHA-256，DER 编码。
public enum GateSignature {
    public static let domain = Data("runode-remote-v1".utf8)
    /// TLS exporter 的 label 和长度。
    public static let exporterLabel = "EXPORTER-runode-remote"
    public static let exporterLength = 32
    public static let nonceLength = 32

    public static func payload(nonce: Data, exporter: Data, purpose: GatePurpose) -> Data {
        var data = Data(capacity: domain.count + nonce.count + exporter.count + 4)
        data.append(domain)
        data.append(nonce)
        data.append(exporter)
        data.append(Data(purpose.rawValue.utf8))
        return data
    }
}

/// RFC 4648 §5 的 base64url，写的时候不带填充；读的时候带不带填充都认。
public enum Base64URL {
    public static func encode(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    public static func decode(_ text: String) -> Data? {
        guard text.allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-" || $0 == "_" || $0 == "=") })
        else { return nil }
        var base64 = text.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        base64 = base64.replacingOccurrences(of: "=", with: "")
        if base64.count % 4 == 1 { return nil }
        base64 += String(repeating: "=", count: (4 - base64.count % 4) % 4)
        return Data(base64Encoded: base64)
    }
}
