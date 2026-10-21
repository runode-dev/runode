import Foundation
import IrohLib
import Network
import RunodeProtocol
import Synchronization

/// 局域网和跨网络一起试时，iroh 晚这么久才拨：在同一个局域网里时直连先过门禁，不白占中继。
let tunnelDelay: Duration = .milliseconds(500)

/// 经 iroh 按电脑的公钥（`MachineRecord.node`）开一条连接；真的实现是 `IrohDialer.open`，测试换成假的。
public typealias TunnelOpener =
    @Sendable (_ node: String, _ fingerprint: CertificateFingerprint) async throws -> any FrameTransport

/// 跨网络连电脑：iroh 按电脑的公钥拨号，先打洞直连，打不通走中继。连接上开一条双向流，流上跑的和局域网的
/// TCP 一样是 TLS 加门禁。Network.framework 的 TLS 只能开在真的网络连接上，所以在本机回环地址上起一个只接
/// 一条连接的代理接到流上，`TLSChannel` 连它。整个 app 共用一个 iroh 端点，第一次拨号时建。
public actor IrohDialer {
    public static let shared = IrohDialer()
    /// 正在建或者建好了的端点；几台电脑同时拨号时共用同一次建。
    private var binding: Task<Endpoint, any Error>?

    public func open(node: String, fingerprint: CertificateFingerprint) async throws -> any FrameTransport {
        let endpoint = try await bound()
        let id: EndpointId
        do {
            id = try EndpointId.fromString(s: node)
        } catch {
            throw LinkFailure.protocolViolation(String(localized: "电脑报的跨网络地址不对"))
        }
        let connection: Connection
        let stream: BiStream
        do {
            connection = try await endpoint.connect(
                addr: EndpointAddr(id: id, relayUrl: nil, addresses: []), alpn: Data(tunnelALPN.utf8))
            stream = try await connection.openBi()
        } catch {
            throw LinkFailure.connectionFailed(error.localizedDescription)
        }
        let proxy: LoopbackProxy
        do {
            proxy = try await LoopbackProxy.start(stream: stream)
        } catch {
            try? connection.close(errorCode: 0, reason: Data())
            throw error
        }
        do {
            let tls = try await TLSChannel.open(to: proxy.target, fingerprint: fingerprint)
            return TunnelledTransport(tls: tls, proxy: proxy, connection: connection)
        } catch {
            proxy.close()
            try? connection.close(errorCode: 0, reason: Data())
            throw error
        }
    }

    private func bound() async throws -> Endpoint {
        if let binding, let endpoint = try? await binding.value, !endpoint.isClosed() { return endpoint }
        let task = Task {
            let builder = EndpointBuilder()
            builder.applyN0()
            return try await builder.bind()
        }
        binding = task
        do {
            return try await task.value
        } catch {
            binding = nil
            throw LinkFailure.connectionFailed(error.localizedDescription)
        }
    }
}

/// 经 iroh 的 TLS 连接。对方地址不报：报的是本机代理，不是电脑的，不能记成下次先试的地址。
final class TunnelledTransport: FrameTransport {
    private let tls: TLSChannel
    private let proxy: LoopbackProxy
    private let connection: Connection

    init(tls: TLSChannel, proxy: LoopbackProxy, connection: Connection) {
        self.tls = tls
        self.proxy = proxy
        self.connection = connection
    }

    func receive() async throws -> Data? { try await tls.receive() }
    func send(_ data: Data) { tls.send(data) }
    func exporter() throws -> Data { try tls.exporter() }
    var remoteAddress: String? { nil }

    func close() {
        tls.close()
        proxy.close()
        try? connection.close(errorCode: 0, reason: Data())
    }
}

/// 本机回环地址上只接一条连接的 TCP 代理，两个方向搬到 iroh 的双向流上，两边都说完为止。
final class LoopbackProxy: Sendable {
    let target: TransportTarget
    private let listener: NWListener
    private let accepted = Mutex<NWConnection?>(nil)
    private let pumps = Mutex<[Task<Void, Never>]>([])

    private init(target: TransportTarget, listener: NWListener) {
        self.target = target
        self.listener = listener
    }

    static func start(stream: BiStream) async throws -> LoopbackProxy {
        let parameters = NWParameters.tcp
        parameters.requiredLocalEndpoint = .hostPort(host: .ipv4(.loopback), port: .any)
        let listener = try NWListener(using: parameters)
        let queue = DispatchQueue(label: "dev.runode.tunnel")
        let once = OnceFlag()
        let port: NWEndpoint.Port = try await withCheckedThrowingContinuation { continuation in
            listener.stateUpdateHandler = { state in
                switch state {
                case .ready:
                    if let port = listener.port, once.claim() { continuation.resume(returning: port) }
                case .failed(let error):
                    if once.claim() { continuation.resume(throwing: LinkFailure.connectionFailed(error.localizedDescription)) }
                case .cancelled:
                    if once.claim() { continuation.resume(throwing: CancellationError()) }
                default:
                    break
                }
            }
            listener.newConnectionHandler = { _ in }
            listener.start(queue: queue)
        }
        let target = TransportTarget(endpoint: .hostPort(host: .ipv4(.loopback), port: port), label: "iroh")
        let proxy = LoopbackProxy(target: target, listener: listener)
        listener.newConnectionHandler = { [weak proxy] connection in
            guard let proxy, proxy.accepted.withLock({ $0 == nil }) else { return connection.cancel() }
            proxy.accepted.withLock { $0 = connection }
            listener.cancel()
            connection.start(queue: queue)
            proxy.pump(connection, stream)
        }
        return proxy
    }

    private func pump(_ connection: NWConnection, _ stream: BiStream) {
        let up = Task {
            let send = stream.send()
            while let data = await Self.receive(connection) {
                guard (try? await send.writeAll(buf: data)) != nil else { break }
            }
            try? await send.finish()
        }
        let down = Task {
            let recv = stream.recv()
            while let data = try? await recv.read(sizeLimit: 64 << 10), !data.isEmpty {
                guard await Self.send(connection, data) else { break }
            }
            connection.send(content: nil, contentContext: .finalMessage, isComplete: true, completion: .idempotent)
        }
        pumps.withLock { $0 = [up, down] }
    }

    /// 下一块字节；对方说完或者出错时为空。
    private static func receive(_ connection: NWConnection) async -> Data? {
        await withCheckedContinuation { continuation in
            connection.receive(minimumIncompleteLength: 1, maximumLength: 64 << 10) { data, _, isComplete, error in
                if let data, !data.isEmpty {
                    continuation.resume(returning: data)
                } else if isComplete || error != nil {
                    continuation.resume(returning: nil)
                } else {
                    continuation.resume(returning: Data())
                }
            }
        }
    }

    /// 写完这块再返回，慢的一方压住快的一方；写失败时为假。
    private static func send(_ connection: NWConnection, _ data: Data) async -> Bool {
        await withCheckedContinuation { continuation in
            connection.send(content: data, completion: .contentProcessed { error in continuation.resume(returning: error == nil) })
        }
    }

    func close() {
        listener.cancel()
        accepted.withLock { $0?.cancel() }
        pumps.withLock { $0.forEach { $0.cancel() } }
    }
}
