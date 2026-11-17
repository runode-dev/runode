import Foundation
import RunodeProtocol
import Synchronization

/// 把图片、视频交给一台电脑：找地址、TLS、门禁、`mediaHello`，等到电脑的 `ready`。之后一个个 `send`，
/// 每个拿回它在电脑上的路径；一次只发一个，等上一个回话再发下一个。不要了就 `close`。
public final class MediaConnection: Sendable {
    private let transport: any FrameTransport
    /// 已经收到、还没切成帧的字节，一个个发的时候接着用。
    private let decoder: Mutex<FrameDecoder>

    private init(transport: any FrameTransport, decoder: FrameDecoder) {
        self.transport = transport
        self.decoder = Mutex(decoder)
    }

    /// 连上 `machine` 收图片、视频的地方。
    public static func open(
        machine: MachineRecord, keyStore: any DeviceKeyStore, discovery: any HostDiscovery,
        open: @escaping TransportOpener = { target, fingerprint in
            try await TLSChannel.open(to: target, fingerprint: fingerprint)
        },
        tunnel: @escaping TunnelOpener = { node, fingerprint, secret in
            try await IrohDialer.shared.open(node: node, fingerprint: fingerprint, secret: secret)
        }
    ) async throws -> MediaConnection {
        let deviceId = machine.deviceId
        return try await dial(
            machine, keyStore: keyStore, discovery: discovery, open: open, tunnel: tunnel,
            handshake: { transport, key in try await handshake(over: transport, deviceId: deviceId, key: key) },
            close: { $0.close() })
    }

    private static func handshake(
        over transport: any FrameTransport, deviceId: String, key: StoredDeviceKey
    ) async throws -> MediaConnection {
        let outcome = try await GateClient.run(over: transport, credential: .auth(deviceId: deviceId, key: key))
        transport.send(try Frame.control(MediaClientMsg.mediaHello(version: mediaVersion)).encoded())
        let decoder = try await withTimeout(GateClient.timeout, onTimeout: { transport.close() }) {
            var reader = FrameReader(transport: transport, decoder: outcome.decoder)
            switch try JSONDecoder().decode(MediaHostMsg.self, from: try await reader.nextControl()) {
            case .ready:
                return reader.decoder
            case .incompatible(let version):
                throw LinkFailure.incompatible(String(localized: "电脑收图片和视频的方式是第 \(version) 版，和这个 app 不一样"))
            default:
                throw LinkFailure.protocolViolation(String(localized: "电脑没有先报能不能收图片和视频"))
            }
        }
        return MediaConnection(transport: transport, decoder: decoder)
    }

    /// 一帧最多带这么多字节，不超过 `maxMediaFrame`：一大块写下去时别的东西插不进来，分成小块。
    static let chunkSize = 256 << 10

    /// 一块交给网络栈、发完后等电脑存好，各自最多等这么久：电脑合盖睡着时连接不断，不设上限就一直等下去。
    static let sendTimeout: Duration = .seconds(120)

    /// 每发出一块报一次：已经发出的字节数、总字节数。
    public typealias Progress = @Sendable (_ sent: Int, _ total: Int) -> Void

    /// 发一个文件的内容（PNG、JPEG、GIF、WebP、MP4 或 MOV），返回它在电脑上的绝对路径。空的或超过
    /// `maxMediaBytes` 时报 `MediaTooLarge`，什么都不发；电脑存不了时报 `MediaFailure`；超时或出了别的错连接就
    /// 不能再用了。
    public func send(_ data: Data, progress: Progress? = nil) async throws -> String {
        try await upload(length: data.count, progress: progress) { offset, count in
            let start = data.startIndex + offset
            return data[start..<(start + count)]
        }
    }

    /// 从文件里一块块读着发，不整个读进内存，返回它在电脑上的绝对路径。读不出文件时报文件的错，空的或超过
    /// `maxMediaBytes` 时报 `MediaTooLarge`，都什么都不发；电脑存不了时报 `MediaFailure`；发到一半文件变短了、
    /// 超时或出了别的错，连接就不能再用了。
    public func send(file: URL, progress: Progress? = nil) async throws -> String {
        let size = try FileManager.default.attributesOfItem(atPath: file.path(percentEncoded: false))[.size] as? Int ?? 0
        let handle = try FileHandle(forReadingFrom: file)
        defer { try? handle.close() }
        return try await upload(length: size, progress: progress) { _, count in
            guard let chunk = try handle.read(upToCount: count), chunk.count == count else {
                throw CocoaError(.fileReadCorruptFile, userInfo: [NSURLErrorKey: file])
            }
            return chunk
        }
    }

    /// 报长度，按 `chunkSize` 分块逐块交给网络栈，再等电脑回话。`read` 给出从 `offset` 起的 `count` 个字节。
    private func upload(
        length: Int, progress: Progress?, read: (_ offset: Int, _ count: Int) throws -> Data
    ) async throws -> String {
        guard length <= maxMediaBytes, let header = UInt32(exactly: length), header > 0 else {
            throw MediaTooLarge(bytes: length)
        }
        let transport = transport
        do {
            transport.send(try Frame.control(MediaClientMsg.media(len: header)).encoded())
            var offset = 0
            while offset < length {
                let count = min(Self.chunkSize, length - offset)
                let frame = try Frame(kind: .media, channel: 0, payload: read(offset, count)).encoded()
                try await withTimeout(Self.sendTimeout, onTimeout: { transport.close() }) {
                    try await transport.sendAndWait(frame)
                }
                offset += count
                progress?(offset, length)
            }
        } catch {
            // 长度报出去了、内容没发全，电脑那边接不上了。
            transport.close()
            throw error
        }
        let start = decoder.withLock { $0 }
        let (path, rest) = try await withTimeout(Self.sendTimeout, onTimeout: { transport.close() }) {
            var reader = FrameReader(transport: transport, decoder: start)
            while true {
                switch try JSONDecoder().decode(MediaHostMsg.self, from: try await reader.nextControl()) {
                case .saved(let path): return (Result<String, MediaFailure>.success(path), reader.decoder)
                case .failed(let reason): return (.failure(MediaFailure(reason: reason)), reader.decoder)
                default: continue
                }
            }
        }
        decoder.withLock { $0 = rest }
        return try path.get()
    }

    public func close() {
        transport.close()
    }
}

/// 电脑没能存下文件。
public struct MediaFailure: LocalizedError, Hashable, Sendable {
    /// 电脑给的原因，英文。
    public var reason: String

    public init(reason: String) {
        self.reason = reason
    }

    public var errorDescription: String? { String(localized: "电脑没能收下这个文件：\(reason)") }
}

/// 文件是空的，或者超过 `maxMediaBytes`，没有发。
public struct MediaTooLarge: LocalizedError, Hashable, Sendable {
    public var bytes: Int

    public init(bytes: Int) {
        self.bytes = bytes
    }

    public var errorDescription: String? {
        bytes == 0
            ? String(localized: "文件是空的，没有发送")
            : String(localized: "文件太大，超过了 \(maxMediaBytes >> 20) MB，没有发送")
    }
}
