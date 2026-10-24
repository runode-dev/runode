import Foundation

/// 交给电脑识别的这套消息和录音格式的版本，和电脑那边 `runode_protocol::speech::SPEECH_VERSION` 一样。
public let speechVersion: UInt32 = 1

/// `FrameKind.audio` 的采样率：载荷是这个采样率的单声道 16 位小端 PCM。
public let speechSampleRate = 16_000

/// 一段录音最长这么多秒，超了电脑回 `failed`。
public let maxSpeechSeconds = 120

/// 手机发给电脑的识别消息。
public enum SpeechClientMsg: Hashable, Sendable, Encodable {
    /// 过了门禁后的第一条，这条连接就接到电脑的识别上。
    case speechHello(version: UInt32)
    /// 录完了，识别之前发去的录音。
    case finish

    private enum Keys: String, CodingKey {
        case type, version
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        switch self {
        case let .speechHello(version):
            try c.encode("speech_hello", forKey: .type)
            try c.encode(version, forKey: .version)
        case .finish:
            try c.encode("finish", forKey: .type)
        }
    }
}

/// 电脑发给手机的识别消息。
public enum SpeechHostMsg: Hashable, Sendable, Decodable {
    /// 握手后的第一条。`installed` 为假时电脑还没下载它设置的识别模型，识别只会失败。
    case ready(installed: Bool)
    /// 电脑的版本和这个 app 不同，随后断开。
    case incompatible(version: UInt32)
    /// 识别出的文字；没听出话来时是空的。
    case text(String)
    /// 识别不了，`reason` 是给人看的英文。
    case failed(reason: String)
    /// 这个 app 不认识的消息，忽略。
    case unknown

    private enum Keys: String, CodingKey {
        case type, installed, version, text, reason
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .type) {
        case "ready": self = .ready(installed: try c.decode(Bool.self, forKey: .installed))
        case "incompatible": self = .incompatible(version: try c.decode(UInt32.self, forKey: .version))
        case "text": self = .text(try c.decode(String.self, forKey: .text))
        case "failed": self = .failed(reason: try c.decode(String.self, forKey: .reason))
        default: self = .unknown
        }
    }
}
