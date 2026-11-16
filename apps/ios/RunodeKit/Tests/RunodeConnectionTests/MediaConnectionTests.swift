import CryptoKit
import Foundation
import RunodeProtocol
import Synchronization
import Testing

@testable import RunodeConnection

/// 交图片、视频的连接：门禁后先发 `media_hello`，等电脑的 `ready`；每个文件先报长度，再分块发 `media` 帧，
/// 等回话。
@Suite struct MediaConnectionTests {
    let machine = MachineRecord.sample()
    let keys = MemoryDeviceKeyStore()
    let supply = TransportSupply()

    init() throws {
        try keys.save(StoredDeviceKey(kind: .software, data: P256.Signing.PrivateKey().rawRepresentation), for: machine.id)
    }

    /// 扮演电脑走完门禁，收下 `media_hello` 后回 `reply`。
    func admit(_ host: inout FakeHost, reply: String = #"{"type":"ready"}"#) async throws {
        host.challenge()
        _ = try #require(try await host.readControl())
        host.send(.accepted(deviceId: machine.deviceId))
        let hello = try #require(try await host.readControl())
        #expect(hello["type"] as? String == "media_hello")
        #expect(hello["version"] as? Int == Int(mediaVersion))
        host.send(json: reply)
    }

    func open() async throws -> MediaConnection {
        try await MediaConnection.open(machine: machine, keyStore: keys, discovery: NoDiscovery(), open: supply.opener)
    }

    /// 连上，交回连接和扮演电脑的一方。
    func connect() async throws -> (MediaConnection, FakeHost) {
        let transport = FakeTransport()
        supply.add(transport)
        var host = FakeHost(transport)
        async let opened = open()
        try await admit(&host)
        return (try await opened, host)
    }

    /// 收下一个文件：先是报长度的控制消息，再是加起来正好那么长的 `media` 帧。
    func receiveMedia(_ host: inout FakeHost) async throws -> (data: Data, frames: Int) {
        let header = try #require(try await host.readControl())
        #expect(header["type"] as? String == "media")
        let length = try #require(header["len"] as? Int)
        var data = Data()
        var frames = 0
        while data.count < length {
            let frame = try #require(try await host.readFrame())
            #expect(frame.kind == .media && frame.channel == 0)
            #expect(frame.payload.count <= maxMediaFrame)
            data.append(frame.payload)
            frames += 1
        }
        #expect(data.count == length)
        return (data, frames)
    }

    func bytes(_ count: Int) -> Data {
        Data((0..<count).map { UInt8(truncatingIfNeeded: $0 * 7) })
    }

    @Test func chunksFitInAFrame() {
        #expect(MediaConnection.chunkSize <= maxMediaFrame)
    }

    @Test func sendsDataInChunksAndReturnsThePaths() async throws {
        let (connection, admitted) = try await connect()
        var host = admitted
        let big = bytes(MediaConnection.chunkSize * 2 + 10)
        let first = Task { try await connection.send(big) }
        let received = try await receiveMedia(&host)
        #expect(received.data == big)
        #expect(received.frames == 3)
        host.send(json: #"{"type":"saved","path":"/tmp/截图 1.png"}"#)
        #expect(try await first.value == "/tmp/截图 1.png")

        // 同一条连接接着发下一个。
        let second = Task { try await connection.send(Data([1, 2, 3])) }
        #expect(try await receiveMedia(&host).data == Data([1, 2, 3]))
        host.send(json: #"{"type":"saved","path":"/tmp/b.png"}"#)
        #expect(try await second.value == "/tmp/b.png")
    }

    /// 从文件发：内容和文件一样，按块报进度，最后一次是全部。
    @Test func streamsAFileAndReportsProgress() async throws {
        let (connection, admitted) = try await connect()
        var host = admitted
        let content = bytes(MediaConnection.chunkSize * 3 + 1)
        let file = FileManager.default.temporaryDirectory.appending(path: "\(UUID().uuidString).mp4")
        try content.write(to: file)
        defer { try? FileManager.default.removeItem(at: file) }

        let reports = Mutex<[[Int]]>([])
        let sending = Task {
            try await connection.send(file: file) { sent, total in reports.withLock { $0.append([sent, total]) } }
        }
        let received = try await receiveMedia(&host)
        #expect(received.data == content)
        #expect(received.frames == 4)
        host.send(json: #"{"type":"saved","path":"/tmp/clip.mp4"}"#)
        #expect(try await sending.value == "/tmp/clip.mp4")
        let total = content.count
        let chunk = MediaConnection.chunkSize
        #expect(reports.withLock { $0 } == [[chunk, total], [chunk * 2, total], [chunk * 3, total], [total, total]])
    }

    /// 空的和超过上限的在本地就拦下，报 `MediaTooLarge`，什么都不发，连接接着能用。
    @Test func emptyOrOversizedIsRejectedLocally() async throws {
        let (connection, admitted) = try await connect()
        var host = admitted
        await #expect(throws: MediaTooLarge(bytes: 0)) { _ = try await connection.send(Data()) }
        let empty = FileManager.default.temporaryDirectory.appending(path: "\(UUID().uuidString).mp4")
        try Data().write(to: empty)
        defer { try? FileManager.default.removeItem(at: empty) }
        await #expect(throws: MediaTooLarge(bytes: 0)) { _ = try await connection.send(file: empty) }
        #expect(MediaTooLarge(bytes: maxMediaBytes + 1).errorDescription != MediaFailure(reason: "x").errorDescription)

        let sending = Task { try await connection.send(Data([9])) }
        #expect(try await receiveMedia(&host).data == Data([9]))
        host.send(json: #"{"type":"saved","path":"/tmp/a.png"}"#)
        #expect(try await sending.value == "/tmp/a.png")
    }

    @Test func aFailureCarriesTheReason() async throws {
        let (connection, admitted) = try await connect()
        var host = admitted
        let sending = Task { try await connection.send(Data([1])) }
        _ = try await receiveMedia(&host)
        host.send(json: #"{"type":"failed","reason":"not an image or video"}"#)
        await #expect(throws: MediaFailure(reason: "not an image or video")) { _ = try await sending.value }
    }

    @Test func aDifferentVersionIsIncompatible() async throws {
        let transport = FakeTransport()
        supply.add(transport)
        var host = FakeHost(transport)
        let opened = Task { try await open() }
        try await admit(&host, reply: #"{"type":"incompatible","version":9}"#)
        await #expect(throws: LinkFailure.self) { _ = try await opened.value }
    }
}
