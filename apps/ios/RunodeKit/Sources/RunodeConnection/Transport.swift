import CryptoKit
import Foundation
import Network
import RunodeProtocol
import Synchronization

/// 一条已经握好手的字节流。`TLSChannel` 是真的；测试用假的，不碰网络。
public protocol FrameTransport: AnyObject, Sendable {
    /// 收下一块字节；对面正常关闭时返回 `nil`，出错时抛错。
    func receive() async throws -> Data?
    /// 把字节排进发送队列，按调用的先后写出去；写失败时下一次 `receive` 会报错。
    func send(_ data: Data)
    /// TLS exporter（label `EXPORTER-runode-remote`、不带 context、32 字节），门禁签名用。
    func exporter() throws -> Data
    /// 实际连上的对方地址（不含端口），记下来下次先试。
    var remoteAddress: String? { get }
    /// 断开；之后的 `receive` 报错。
    func close()
}

/// 要试着连的一个地方：Bonjour 找到的服务，或者一个地址加端口。
public struct TransportTarget: Hashable, Sendable {
    public var endpoint: NWEndpoint
    /// 给人看、也用来去重。
    public var label: String

    public init(endpoint: NWEndpoint, label: String) {
        self.endpoint = endpoint
        self.label = label
    }

    /// 一个 IPv4/IPv6 地址或主机名加端口；写法不对时返回 `nil`。
    public static func address(_ address: String, port: UInt16) -> TransportTarget? {
        guard !address.isEmpty, let port = NWEndpoint.Port(rawValue: port) else { return nil }
        return TransportTarget(endpoint: .hostPort(host: NWEndpoint.Host(address), port: port), label: address)
    }
}

/// 打开一条到 `target` 的、指纹对得上的连接。真的实现是 `TLSChannel.open`，测试换成假的。
public typealias TransportOpener =
    @Sendable (_ target: TransportTarget, _ fingerprint: CertificateFingerprint) async throws -> any FrameTransport

/// 只能置一次的标记，回调里保证续体只恢复一次。
final class OnceFlag: Sendable {
    private let claimed = Mutex(false)

    /// 第一次调用返回 true，之后都是 false。
    func claim() -> Bool {
        claimed.withLock { claimed in
            if claimed { return false }
            claimed = true
            return true
        }
    }

    var isSet: Bool { claimed.withLock { $0 } }
}

/// Network.framework 的 TLS 1.3 连接。只在这里用回调和 `DispatchQueue`，对外是 async 的接口。
/// 不走 CA 校验也不校验主机名：验证回调里只比对叶子证书 DER 的 SHA-256 和配对时记下的指纹。
public final class TLSChannel: FrameTransport {
    private let connection: NWConnection
    /// 连接上所有回调都在这条队列上。
    private let queue: DispatchQueue

    private init(connection: NWConnection, queue: DispatchQueue) {
        self.connection = connection
        self.queue = queue
    }

    /// 连上 `target` 并做完 TLS 握手。`timeout` 内没握完手、证书指纹不对、对方不说 TLS 1.3 都报错。
    public static func open(
        to target: TransportTarget, fingerprint: CertificateFingerprint, timeout: Duration = .seconds(6)
    ) async throws -> TLSChannel {
        let queue = DispatchQueue(label: "dev.runode.tls")
        let mismatch = OnceFlag()
        let tls = NWProtocolTLS.Options()
        let security = tls.securityProtocolOptions
        sec_protocol_options_set_min_tls_protocol_version(security, .TLSv13)
        sec_protocol_options_set_max_tls_protocol_version(security, .TLSv13)
        let expected = fingerprint.bytes
        sec_protocol_options_set_verify_block(
            security,
            { _, trust, complete in
                let matches = TLSChannel.leafFingerprint(of: trust) == expected
                if !matches { _ = mismatch.claim() }
                complete(matches)
            }, queue)
        let tcp = NWProtocolTCP.Options()
        tcp.noDelay = true
        tcp.enableKeepalive = true
        tcp.keepaliveIdle = 15
        let connection = NWConnection(to: target.endpoint, using: NWParameters(tls: tls, tcp: tcp))
        let channel = TLSChannel(connection: connection, queue: queue)
        let once = OnceFlag()
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, any Error>) in
                connection.stateUpdateHandler = { state in
                    switch state {
                    case .ready:
                        if once.claim() { continuation.resume() }
                    case .failed(let error), .waiting(let error):
                        // `waiting` 是连不上、等网络变化后再试；这里不等，换下一个地址。
                        if once.claim() {
                            connection.cancel()
                            continuation.resume(
                                throwing: mismatch.isSet
                                    ? LinkFailure.fingerprintMismatch
                                    : LinkFailure.connectionFailed(error.localizedDescription))
                        }
                    case .cancelled:
                        if once.claim() { continuation.resume(throwing: CancellationError()) }
                    default:
                        break
                    }
                }
                connection.start(queue: queue)
                let seconds = Double(timeout.components.seconds) + Double(timeout.components.attoseconds) / 1e18
                queue.asyncAfter(deadline: .now() + seconds) {
                    if once.claim() {
                        connection.cancel()
                        continuation.resume(throwing: LinkFailure.timeout)
                    }
                }
            }
        } onCancel: {
            connection.cancel()
        }
        return channel
    }

    /// 叶子证书 DER 的 SHA-256；拿不到证书时为空。
    private static func leafFingerprint(of trust: sec_trust_t) -> Data {
        let secTrust = sec_trust_copy_ref(trust).takeRetainedValue()
        guard let chain = SecTrustCopyCertificateChain(secTrust) as? [SecCertificate], let leaf = chain.first else {
            return Data()
        }
        let der = SecCertificateCopyData(leaf) as Data
        return Data(SHA256.hash(data: der))
    }

    public func receive() async throws -> Data? {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Data?, any Error>) in
            connection.receive(minimumIncompleteLength: 1, maximumLength: 256 << 10) { data, _, isComplete, error in
                if let data, !data.isEmpty {
                    continuation.resume(returning: data)
                } else if let error {
                    continuation.resume(throwing: LinkFailure.closed(error.localizedDescription))
                } else if isComplete {
                    continuation.resume(returning: nil)
                } else {
                    continuation.resume(returning: Data())
                }
            }
        }
    }

    public func send(_ data: Data) {
        connection.send(content: data, completion: .contentProcessed { _ in })
    }

    public func exporter() throws -> Data {
        guard let metadata = connection.metadata(definition: NWProtocolTLS.definition) as? NWProtocolTLS.Metadata
        else { throw LinkFailure.protocolViolation(String(localized: "拿不到 TLS 的状态")) }
        let label = GateSignature.exporterLabel
        let secret = label.withCString { pointer in
            sec_protocol_metadata_create_secret(
                metadata.securityProtocolMetadata, label.utf8.count, pointer, GateSignature.exporterLength)
        }
        guard let secret else { throw LinkFailure.protocolViolation(String(localized: "导不出 TLS exporter")) }
        let bytes = Data(secret as DispatchData)
        guard bytes.count == GateSignature.exporterLength else {
            throw LinkFailure.protocolViolation(String(localized: "TLS exporter 长度不对"))
        }
        return bytes
    }

    public var remoteAddress: String? {
        guard case .hostPort(let host, _) = connection.currentPath?.remoteEndpoint else { return nil }
        switch host {
        case .name(let name, _): return name
        case .ipv4(let address): return "\(address)"
        case .ipv6(let address): return "\(address)"
        @unknown default: return nil
        }
    }

    public func close() {
        connection.cancel()
    }
}
