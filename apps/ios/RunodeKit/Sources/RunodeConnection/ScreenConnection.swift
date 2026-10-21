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

    /// 连上 `machine` 的远程桌面。Bonjour 找到的地址、记着的地址和跨网络的 iroh（晚 `tunnelDelay` 才拨）
    /// 同时试，谁先握完手用谁。
    public static func open(
        machine: MachineRecord, keyStore: any DeviceKeyStore, discovery: any HostDiscovery,
        open: @escaping TransportOpener = { target, fingerprint in
            try await TLSChannel.open(to: target, fingerprint: fingerprint)
        },
        tunnel: @escaping TunnelOpener = { node, fingerprint in
            try await IrohDialer.shared.open(node: node, fingerprint: fingerprint)
        }
    ) async throws -> ScreenConnection {
        guard let key = try keyStore.key(for: machine.id) else { throw LinkFailure.missingKey }
        let attempt: @Sendable (@Sendable () async throws -> any FrameTransport) async throws -> ScreenConnection = {
            connect in
            let transport = try await connect()
            return try await withTaskCancellationHandler {
                do {
                    return try await handshake(over: transport, deviceId: machine.deviceId, key: key)
                } catch {
                    transport.close()
                    throw error
                }
            } onCancel: {
                transport.close()
            }
        }
        let fallback = machine.fallbackTargets
        let fingerprint = machine.fingerprint
        return try await withThrowingTaskGroup(of: ScreenConnection.self) { group in
            group.addTask {
                try await firstSuccess(of: fallback) { target in try await attempt { try await open(target, fingerprint) } }
            }
            group.addTask {
                guard let found = await discovery.locate(fingerprint, timeout: .seconds(2)) else {
                    throw LinkFailure.noAddress
                }
                return try await attempt { try await open(found, fingerprint) }
            }
            if let node = machine.node {
                group.addTask {
                    try await Task.sleep(for: tunnelDelay)
                    return try await attempt { try await tunnel(node, fingerprint) }
                }
            }
            var winner: ScreenConnection?
            var failure: (any Error)?
            // 有了结果也把另一条等完：它可能也连上了，要关掉。
            while let result = await group.nextResult() {
                switch result {
                case .success(let connection) where winner == nil:
                    winner = connection
                    group.cancelAll()
                case .success(let connection):
                    connection.close()
                case .failure(let error):
                    if failure == nil || (failure as? LinkFailure) == .noAddress { failure = error }
                }
            }
            if let winner { return winner }
            throw failure ?? LinkFailure.noAddress
        }
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

    /// 电脑发来的消息和画面，到连接断开为止；只能取一次。流结束时连接已经关了。
    public func events() -> AsyncThrowingStream<ScreenEvent, any Error> {
        let transport = transport
        let initial = decoder
        return AsyncThrowingStream { continuation in
            let task = Task {
                var decoder = initial
                do {
                    while true {
                        while let frame = try decoder.next() {
                            continuation.yield(try Self.event(from: frame))
                        }
                        guard let chunk = try await transport.receive() else {
                            try decoder.finish()
                            throw LinkFailure.closed(String(localized: "电脑关闭了远程桌面"))
                        }
                        decoder.append(chunk)
                    }
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in
                task.cancel()
                transport.close()
            }
        }
    }

    private static func event(from frame: Frame) throws -> ScreenEvent {
        switch frame.kind {
        case .control:
            return .message((try? frame.message(ScreenHostMsg.self)) ?? .unknown)
        case .video:
            guard let video = VideoFrame(payload: frame.payload) else {
                throw LinkFailure.protocolViolation(String(localized: "画面的格式不对"))
            }
            return .video(video)
        case .output, .input, .snapshot:
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
