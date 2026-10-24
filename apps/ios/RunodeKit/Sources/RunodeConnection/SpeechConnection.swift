import Foundation
import RunodeProtocol

/// 交给一台电脑识别的一段录音：找地址、TLS、门禁、`speechHello`，等到电脑的 `ready`。边录边 `send`，
/// 录完 `finish` 拿文字；一条连接识别一段话，不要了就 `close`。
public final class SpeechConnection: Sendable {
    /// 电脑上下好了它设置的识别模型；没有时识别只会失败。
    public let installed: Bool
    private let transport: any FrameTransport
    /// `ready` 之后已经收到、还没切成帧的字节。
    private let decoder: FrameDecoder

    private init(transport: any FrameTransport, decoder: FrameDecoder, installed: Bool) {
        self.transport = transport
        self.decoder = decoder
        self.installed = installed
    }

    /// 连上 `machine` 的识别。
    public static func open(
        machine: MachineRecord, keyStore: any DeviceKeyStore, discovery: any HostDiscovery,
        open: @escaping TransportOpener = { target, fingerprint in
            try await TLSChannel.open(to: target, fingerprint: fingerprint)
        },
        tunnel: @escaping TunnelOpener = { node, fingerprint in
            try await IrohDialer.shared.open(node: node, fingerprint: fingerprint)
        }
    ) async throws -> SpeechConnection {
        let deviceId = machine.deviceId
        return try await dial(
            machine, keyStore: keyStore, discovery: discovery, open: open, tunnel: tunnel,
            handshake: { transport, key in try await handshake(over: transport, deviceId: deviceId, key: key) },
            close: { $0.close() })
    }

    private static func handshake(
        over transport: any FrameTransport, deviceId: String, key: StoredDeviceKey
    ) async throws -> SpeechConnection {
        let outcome = try await GateClient.run(over: transport, credential: .auth(deviceId: deviceId, key: key))
        transport.send(try Frame.control(SpeechClientMsg.speechHello(version: speechVersion)).encoded())
        let (decoder, installed) = try await withTimeout(GateClient.timeout, onTimeout: { transport.close() }) {
            var reader = FrameReader(transport: transport, decoder: outcome.decoder)
            switch try JSONDecoder().decode(SpeechHostMsg.self, from: try await reader.nextControl()) {
            case .ready(let installed):
                return (reader.decoder, installed)
            case .incompatible(let version):
                throw LinkFailure.incompatible(String(localized: "电脑的语音识别是第 \(version) 版，和这个 app 不一样"))
            default:
                throw LinkFailure.protocolViolation(String(localized: "电脑没有先报识别的情况"))
            }
        }
        return SpeechConnection(transport: transport, decoder: decoder, installed: installed)
    }

    /// 发一段录音（`speechSampleRate` 的单声道，-1 到 1）。
    public func send(_ samples: [Float]) {
        var payload = Data(capacity: samples.count * 2)
        for sample in samples {
            let value = Int16(max(-1, min(1, sample)) * Float(Int16.max))
            withUnsafeBytes(of: value.littleEndian) { payload.append(contentsOf: $0) }
        }
        guard let frame = try? Frame(kind: .audio, channel: 0, payload: payload).encoded() else { return }
        transport.send(frame)
    }

    /// 等电脑识别完最多这么久：电脑可能先在本机识别、出错再交给云端，最后还要润色，每步都要等网络；
    /// 电脑合盖睡着时连接不断，不设上限就一直等下去。
    static let finishTimeout: Duration = .seconds(180)

    /// 录完了：等电脑识别完，返回文字；没听出话来时是空的。之后连接关掉。
    public func finish() async throws -> String {
        defer { transport.close() }
        transport.send(try Frame.control(SpeechClientMsg.finish).encoded())
        let (transport, decoder) = (transport, decoder)
        return try await withTimeout(Self.finishTimeout, onTimeout: { transport.close() }) {
            var reader = FrameReader(transport: transport, decoder: decoder)
            while true {
                switch try JSONDecoder().decode(SpeechHostMsg.self, from: try await reader.nextControl()) {
                case .text(let text): return text
                case .failed(let reason): throw SpeechFailure(reason: reason)
                default: continue
                }
            }
        }
    }

    public func close() {
        transport.close()
    }
}

/// 电脑回了识别失败。
public struct SpeechFailure: LocalizedError, Hashable, Sendable {
    /// 电脑给的原因，英文。
    public var reason: String

    public var errorDescription: String? { String(localized: "电脑没能识别：\(reason)") }
}
