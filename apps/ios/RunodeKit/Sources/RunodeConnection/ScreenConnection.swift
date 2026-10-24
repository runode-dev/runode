import Foundation
import RunodeProtocol

/// 远程桌面的连接上收到的东西。
public enum ScreenEvent: Sendable {
    case message(ScreenHostMsg)
    case video(VideoFrame)
}

/// 一台电脑的远程桌面：找地址、TLS、门禁、`ScreenHello`，等到电脑的 `Ready`。断了不自动重连，由页面
/// 重新打开。
public final class ScreenConnection: Sendable {
    /// 电脑握手后报的各块屏幕，主屏排第一，至少一块。
    public let displays: [ScreenDisplay]
    /// 电脑有没有屏幕录制的授权；没有时 `Start` 只换来 `Stopped`。
    public let screenCapture: Bool
    /// 电脑有没有注入键鼠的授权；没有时输入被系统悄悄丢掉。
    public let postEvent: Bool
    private let transport: any FrameTransport
    /// `Ready` 之后已经收到、还没切成帧的字节。
    private let decoder: FrameDecoder

    private init(transport: any FrameTransport, decoder: FrameDecoder, ready: Ready) {
        self.transport = transport
        self.decoder = decoder
        displays = ready.displays
        screenCapture = ready.screenCapture
        postEvent = ready.postEvent
    }

    private struct Ready: Sendable {
        var displays: [ScreenDisplay]
        var screenCapture: Bool
        var postEvent: Bool
    }

    /// 连上 `machine` 的远程桌面。
    public static func open(
        machine: MachineRecord, keyStore: any DeviceKeyStore, discovery: any HostDiscovery,
        open: @escaping TransportOpener = { target, fingerprint in
            try await TLSChannel.open(to: target, fingerprint: fingerprint)
        },
        tunnel: @escaping TunnelOpener = { node, fingerprint in
            try await IrohDialer.shared.open(node: node, fingerprint: fingerprint)
        }
    ) async throws -> ScreenConnection {
        let deviceId = machine.deviceId
        return try await dial(
            machine, keyStore: keyStore, discovery: discovery, open: open, tunnel: tunnel,
            handshake: { transport, key in try await handshake(over: transport, deviceId: deviceId, key: key) },
            close: { $0.close() })
    }

    private static func handshake(
        over transport: any FrameTransport, deviceId: String, key: StoredDeviceKey
    ) async throws -> ScreenConnection {
        let outcome = try await GateClient.run(over: transport, credential: .auth(deviceId: deviceId, key: key))
        transport.send(try Frame.control(ScreenClientMsg.screenHello(version: screenVersion)).encoded())
        let (decoder, ready) = try await withTimeout(GateClient.timeout, onTimeout: { transport.close() }) {
            var reader = FrameReader(transport: transport, decoder: outcome.decoder)
            switch try JSONDecoder().decode(ScreenHostMsg.self, from: try await reader.nextControl()) {
            case let .ready(displays, screenCapture, postEvent) where !displays.isEmpty:
                return (reader.decoder, Ready(displays: displays, screenCapture: screenCapture, postEvent: postEvent))
            case .incompatible(let version):
                throw LinkFailure.incompatible(String(localized: "电脑的远程桌面是第 \(version) 版，和这个 app 不一样"))
            default:
                throw LinkFailure.protocolViolation(String(localized: "电脑没有先报屏幕的情况"))
            }
        }
        return ScreenConnection(transport: transport, decoder: decoder, ready: ready)
    }

    /// 电脑发来的消息和画面，到连接断开为止；只能取一次。流结束时连接已经关了。取一个才读一个：
    /// 页面处理不过来时不再往下读，由 TCP 压住电脑，不在手机上越积越多。
    public func events() -> ScreenEvents {
        ScreenEvents(transport: transport, decoder: decoder)
    }

    fileprivate static func event(from frame: Frame) throws -> ScreenEvent {
        switch frame.kind {
        case .control:
            return .message((try? frame.message(ScreenHostMsg.self)) ?? .unknown)
        case .video:
            guard let video = VideoFrame(payload: frame.payload) else {
                throw LinkFailure.protocolViolation(String(localized: "画面的格式不对"))
            }
            return .video(video)
        case .output, .input, .snapshot, .audio:
            throw LinkFailure.protocolViolation(String(localized: "远程桌面上收到了类型 \(frame.kind.rawValue) 的帧"))
        }
    }

    public func send(_ message: ScreenClientMsg) {
        guard let frame = try? Frame.control(message).encoded() else { return }
        transport.send(frame)
    }

    public func close() {
        transport.close()
    }
}

public struct ScreenEvents: AsyncSequence, Sendable {
    fileprivate let transport: any FrameTransport
    fileprivate let decoder: FrameDecoder

    public func makeAsyncIterator() -> Iterator {
        Iterator(transport: transport, decoder: decoder)
    }

    public struct Iterator: AsyncIteratorProtocol, Sendable {
        fileprivate let transport: any FrameTransport
        fileprivate var decoder: FrameDecoder
        fileprivate var finished = false

        public mutating func next() async throws -> ScreenEvent? {
            guard !finished else { return nil }
            do {
                while true {
                    if let frame = try decoder.next() { return try ScreenConnection.event(from: frame) }
                    guard let chunk = try await transport.receive() else {
                        try decoder.finish()
                        throw LinkFailure.closed(String(localized: "电脑关闭了远程桌面"))
                    }
                    decoder.append(chunk)
                }
            } catch {
                finished = true
                transport.close()
                throw error
            }
        }
    }
}
