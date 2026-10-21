import CryptoKit
import Foundation
import RunodeProtocol
import Testing

@testable import RunodeConnection

/// 远程桌面的连接：门禁后先发 `screen_hello`，等电脑的 `ready`，之后收消息和画面、发消息。
@Suite struct ScreenConnectionTests {
    let machine = MachineRecord.sample()
    let keys = MemoryDeviceKeyStore()
    let supply = TransportSupply()

    init() throws {
        try keys.save(StoredDeviceKey(kind: .software, data: P256.Signing.PrivateKey().rawRepresentation), for: machine.id)
    }

    static let ready =
        #"{"type":"ready","displays":[{"id":1,"width":1728,"height":1117,"pixel_width":3456,"pixel_height":2234}],"screen_capture":true,"post_event":false}"#

    /// 扮演电脑走完门禁，收下 `screen_hello` 后回 `reply`。
    func admit(_ host: inout FakeHost, reply: String = ScreenConnectionTests.ready) async throws {
        host.challenge()
        _ = try #require(try await host.readControl())
        host.send(.accepted(deviceId: machine.deviceId))
        let hello = try #require(try await host.readControl())
        #expect(hello["type"] as? String == "screen_hello")
        #expect(hello["version"] as? Int == Int(screenVersion))
        host.send(json: reply)
    }

    func open() async throws -> ScreenConnection {
        try await ScreenConnection.open(machine: machine, keyStore: keys, discovery: NoDiscovery(), open: supply.opener)
    }

    @Test func handshakesThenStreamsMessagesAndVideo() async throws {
        let transport = FakeTransport()
        supply.add(transport)
        var host = FakeHost(transport)
        async let opened = open()
        try await admit(&host)
        let connection = try await opened
        #expect(connection.displays == [ScreenDisplay(id: 1, width: 1728, height: 1117, pixelWidth: 3456, pixelHeight: 2234)])
        #expect(connection.screenCapture && !connection.postEvent)

        connection.send(.start(display: nil, width: 1280, height: 720, fps: 60, bitrate: 8_000_000))
        #expect(try await host.readControl()?["type"] as? String == "start")
        host.send(json: #"{"type":"started","display":1,"width":1280,"height":720}"#)
        let video = VideoFrame(data: Data([0, 0, 0, 1, 0x26]), keyframe: true, parameterSets: [Data([0x40])], ptsMicros: 7)
        var payload = Data([1])
        withUnsafeBytes(of: Int64(7).littleEndian) { payload.append(contentsOf: $0) }
        payload.append(contentsOf: [1, 1, 0, 0, 0, 0x40, 0, 0, 0, 1, 0x26])
        host.send(frame: Frame(kind: .video, channel: 0, payload: payload))

        var events = connection.events().makeAsyncIterator()
        guard case .message(.started(1, 1280, 720))? = try await events.next() else { Issue.record("no started"); return }
        guard case .video(let received)? = try await events.next() else { Issue.record("no video"); return }
        #expect(received == video)
        transport.close()
        await #expect(throws: LinkFailure.self) { _ = try await events.next() }
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
