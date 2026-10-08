import Foundation
import RunodeProtocol

/// 门禁时出示的凭据。
public enum GateCredential: Sendable {
    /// 已配对过：宿主发的 `device_id` 加这台电脑的设备私钥。
    case auth(deviceId: String, key: StoredDeviceKey)
    /// 第一次配对：二维码里的口令、报给电脑的设备名、新生成的设备私钥。
    case pair(secret: Data, deviceName: String, key: StoredDeviceKey)
}

/// 门禁的结果。
public struct GateOutcome: Sendable {
    /// 宿主给的设备编号。
    public var deviceId: String
    /// `remote_challenge` 里报的主机名。
    public var hostName: String
    /// 门禁之后剩下、还没切成帧的字节；`Hello` 接着它读。
    public var decoder: FrameDecoder
}

/// 一边读帧一边攒着没切完的字节。
struct FrameReader {
    let transport: any FrameTransport
    var decoder: FrameDecoder

    /// 下一帧；对面关了连接时报错。
    mutating func next() async throws -> Frame {
        while true {
            if let frame = try decoder.next() {
                return frame
            }
            guard let chunk = try await transport.receive() else {
                try decoder.finish()
                throw LinkFailure.closed(String(localized: "电脑关闭了连接"))
            }
            decoder.append(chunk)
        }
    }

    /// 下一条控制帧的载荷；门禁和握手阶段宿主不发别的帧。
    mutating func nextControl() async throws -> Data {
        let frame = try await next()
        guard frame.kind == .control, frame.channel == 0 else {
            throw LinkFailure.protocolViolation(String(localized: "还没登录就收到了类型 \(frame.kind.rawValue) 的帧"))
        }
        return frame.payload
    }
}

/// 走一遍门禁：等宿主的 `remote_challenge`，签好名回 `remote_auth` 或 `remote_pair`，等宿主放行。
/// 规格要求门禁阶段 10 秒内走完，单帧载荷不超过 16 KiB；超时时关掉连接。
public enum GateClient {
    public static let timeout: Duration = .seconds(10)

    public static func run(
        over transport: any FrameTransport, credential: GateCredential, timeout: Duration = GateClient.timeout
    ) async throws -> GateOutcome {
        try await withTimeout(timeout, onTimeout: { transport.close() }) {
            var reader = FrameReader(transport: transport, decoder: FrameDecoder(maxPayload: Frame.gateMaxPayload))
            let challenge = try JSONDecoder().decode(GateMessage.self, from: try await reader.nextControl())
            guard case let .challenge(version, nonce, hostName) = challenge else {
                throw LinkFailure.protocolViolation(String(localized: "第一条消息不是 remote_challenge"))
            }
            guard version == gateVersion else { throw LinkFailure.gateVersion(version) }
            guard nonce.count == GateSignature.nonceLength else {
                throw LinkFailure.protocolViolation(String(localized: "nonce 长度不对"))
            }
            let exporter = try transport.exporter()
            let reply: GateMessage
            switch credential {
            case let .auth(deviceId, key):
                let payload = GateSignature.payload(nonce: nonce, exporter: exporter, purpose: .auth)
                reply = .auth(deviceId: deviceId, signature: try key.signature(for: payload))
            case let .pair(secret, deviceName, key):
                let payload = GateSignature.payload(nonce: nonce, exporter: exporter, purpose: .pair)
                reply = .pair(
                    secret: secret, deviceName: deviceName, publicKey: try key.publicKeyX963,
                    signature: try key.signature(for: payload))
            }
            transport.send(try Frame.control(reply).encoded())
            let verdict = try JSONDecoder().decode(GateMessage.self, from: try await reader.nextControl())
            switch verdict {
            case .accepted(let deviceId):
                var decoder = reader.decoder
                decoder.maxPayload = Frame.maxPayload
                return GateOutcome(deviceId: deviceId, hostName: hostName, decoder: decoder)
            case .rejected(let reason):
                throw LinkFailure.rejected(reason)
            default:
                throw LinkFailure.protocolViolation(String(localized: "门禁的回应不对"))
            }
        }
    }
}

/// 在 `duration` 内做完 `operation`，否则调 `onTimeout`（一般是关掉连接，让卡着的读返回）并报
/// `LinkFailure.timeout`。
func withTimeout<T: Sendable>(
    _ duration: Duration, onTimeout: @escaping @Sendable () -> Void,
    _ operation: @escaping @Sendable () async throws -> T
) async throws -> T {
    let timedOut = OnceFlag()
    do {
        return try await withThrowingTaskGroup(of: T.self) { group in
            group.addTask { try await operation() }
            group.addTask {
                try await Task.sleep(for: duration)
                _ = timedOut.claim()
                onTimeout()
                throw LinkFailure.timeout
            }
            defer { group.cancelAll() }
            guard let result = try await group.next() else { throw CancellationError() }
            return result
        }
    } catch {
        // 超时关掉连接后，卡着的读报的是「连接断开」，这里统一报超时。
        if timedOut.isSet { throw LinkFailure.timeout }
        throw error
    }
}
