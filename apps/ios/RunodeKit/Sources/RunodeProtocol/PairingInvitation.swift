import Foundation

/// Mac 证书的指纹：叶子证书 DER 的 SHA-256，32 字节。
public struct CertificateFingerprint: Hashable, Sendable, Codable, CustomStringConvertible {
    public let bytes: Data

    public init?(bytes: Data) {
        guard bytes.count == 32 else { return nil }
        self.bytes = bytes
    }

    /// 读二维码和 Bonjour TXT 里的 `fp`。规格说二进制字段一律 base64url，这里另外认 64 位
    /// 十六进制，Mac 那边万一写成十六进制也对得上。
    public init?(text: String) {
        if text.count == 64, text.allSatisfy(\.isHexDigit) {
            var bytes = Data(capacity: 32)
            var index = text.startIndex
            while index < text.endIndex {
                let next = text.index(index, offsetBy: 2)
                guard let byte = UInt8(text[index..<next], radix: 16) else { return nil }
                bytes.append(byte)
                index = next
            }
            self.init(bytes: bytes)
        } else if let bytes = Base64URL.decode(text) {
            self.init(bytes: bytes)
        } else {
            return nil
        }
    }

    public var description: String { Base64URL.encode(bytes) }

    public init(from decoder: any Decoder) throws {
        let text = try decoder.singleValueContainer().decode(String.self)
        guard let fingerprint = CertificateFingerprint(text: text) else {
            throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: "bad fingerprint"))
        }
        self = fingerprint
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(description)
    }
}

/// 二维码里的配对邀请：
/// `runode://pair?v=1&name=<主机名>&fp=<指纹>&secret=<口令>&port=<端口>&addr=<地址1>,<地址2>&exp=<Unix 秒>`。
public struct PairingInvitation: Hashable, Sendable {
    /// 现在认得的邀请版本。
    public static let version = 1

    public var hostName: String
    public var fingerprint: CertificateFingerprint
    /// 32 字节的一次性配对口令。
    public var secret: Data
    public var port: UInt16
    /// 生成二维码时 Mac 的非回环地址，挨个试。
    public var addresses: [String]
    public var expiresAt: Date

    public init(
        hostName: String, fingerprint: CertificateFingerprint, secret: Data, port: UInt16, addresses: [String],
        expiresAt: Date
    ) {
        self.hostName = hostName
        self.fingerprint = fingerprint
        self.secret = secret
        self.port = port
        self.addresses = addresses
        self.expiresAt = expiresAt
    }

    public func isExpired(at now: Date = .now) -> Bool {
        now >= expiresAt
    }

    /// 解析邀请链接，前后的空白不算。
    public static func parse(_ text: String) throws(PairingInvitationError) -> PairingInvitation {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let components = URLComponents(string: trimmed), components.scheme?.lowercased() == "runode",
            components.host?.lowercased() == "pair"
        else { throw .notAnInvitation }
        var fields: [String: String] = [:]
        for item in components.queryItems ?? [] {
            fields[item.name] = item.value ?? ""
        }
        guard let versionText = fields["v"], let version = Int(versionText) else { throw .missing("v") }
        guard version == PairingInvitation.version else { throw .unsupportedVersion(version) }
        guard let name = fields["name"], !name.isEmpty else { throw .missing("name") }
        guard let fpText = fields["fp"] else { throw .missing("fp") }
        guard let fingerprint = CertificateFingerprint(text: fpText) else { throw .invalid("fp") }
        guard let secretText = fields["secret"] else { throw .missing("secret") }
        guard let secret = Base64URL.decode(secretText), secret.count == 32 else { throw .invalid("secret") }
        guard let portText = fields["port"] else { throw .missing("port") }
        guard let port = UInt16(portText), port != 0 else { throw .invalid("port") }
        let addresses = (fields["addr"] ?? "")
            .split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        guard let expText = fields["exp"] else { throw .missing("exp") }
        guard let exp = TimeInterval(expText) else { throw .invalid("exp") }
        return PairingInvitation(
            hostName: name, fingerprint: fingerprint, secret: secret, port: port, addresses: addresses,
            expiresAt: Date(timeIntervalSince1970: exp))
    }
}

/// 邀请链接读不了的原因。
public enum PairingInvitationError: Error, Hashable, Sendable, LocalizedError {
    case notAnInvitation
    case unsupportedVersion(Int)
    case missing(String)
    case invalid(String)

    public var errorDescription: String? {
        switch self {
        case .notAnInvitation: "这不是 Runode 的配对链接"
        case .unsupportedVersion(let version): "配对链接的版本是 \(version)，这个 app 太旧了，请先升级"
        case .missing(let field): "配对链接里缺少 \(field)"
        case .invalid(let field): "配对链接里的 \(field) 不对"
        }
    }
}
