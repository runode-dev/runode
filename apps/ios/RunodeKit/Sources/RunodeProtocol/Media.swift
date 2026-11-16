import Foundation

/// 交图片、视频给电脑的这套消息的版本，和电脑那边 `runode_protocol::media::MEDIA_VERSION` 一样。
public let mediaVersion: UInt32 = 1

/// 一个文件最多这么多字节，`len` 超了电脑回 `failed` 后断开。
public let maxMediaBytes = 200 << 20

/// 一帧 `FrameKind.media` 的载荷最多这么多字节。
public let maxMediaFrame = 1 << 20

/// 手机发给电脑的消息。文件的内容另由 `FrameKind.media` 帧带着。
public enum MediaClientMsg: Hashable, Sendable, Encodable {
    /// 过了门禁后的第一条，这条连接就接到电脑收图片、视频的地方。
    case mediaHello(version: UInt32)
    /// 要发一个文件了，随后的 `FrameKind.media` 帧的载荷加起来正好 `len` 字节。
    case media(len: UInt32)

    private enum Keys: String, CodingKey {
        case type, version, len
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        switch self {
        case let .mediaHello(version):
            try c.encode("media_hello", forKey: .type)
            try c.encode(version, forKey: .version)
        case let .media(len):
            try c.encode("media", forKey: .type)
            try c.encode(len, forKey: .len)
        }
    }
}

/// 电脑发给手机的消息。
public enum MediaHostMsg: Hashable, Sendable, Decodable {
    /// 握手后的第一条。
    case ready
    /// 电脑的版本和这个 app 不同，随后断开。
    case incompatible(version: UInt32)
    /// 存好了，`path` 是文件在电脑上的绝对路径。
    case saved(path: String)
    /// 存不了，`reason` 是给人看的英文。
    case failed(reason: String)
    /// 这个 app 不认识的消息，忽略。
    case unknown

    private enum Keys: String, CodingKey {
        case type, version, path, reason
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .type) {
        case "ready": self = .ready
        case "incompatible": self = .incompatible(version: try c.decode(UInt32.self, forKey: .version))
        case "saved": self = .saved(path: try c.decode(String.self, forKey: .path))
        case "failed": self = .failed(reason: try c.decode(String.self, forKey: .reason))
        default: self = .unknown
        }
    }
}
