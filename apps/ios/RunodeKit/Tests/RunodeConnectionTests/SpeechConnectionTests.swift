import CryptoKit
import Foundation
import RunodeProtocol
import Testing

@testable import RunodeConnection

/// 交给电脑识别的连接：门禁后先发 `speech_hello`，等电脑的 `ready`，录音按 16 位 PCM 发，`finish` 后等文字。
@Suite struct SpeechConnectionTests {
    let machine = MachineRecord.sample()
    let keys = MemoryDeviceKeyStore()
    let supply = TransportSupply()

    init() throws {
        try keys.save(StoredDeviceKey(kind: .software, data: P256.Signing.PrivateKey().rawRepresentation), for: machine.id)
    }

    /// 扮演电脑走完门禁，收下 `speech_hello` 后回 `reply`。
    func admit(_ host: inout FakeHost, reply: String = #"{"type":"ready","installed":true}"#) async throws {
        host.challenge()
        _ = try #require(try await host.readControl())
        host.send(.accepted(deviceId: machine.deviceId))
        let hello = try #require(try await host.readControl())
        #expect(hello["type"] as? String == "speech_hello")
        #expect(hello["version"] as? Int == Int(speechVersion))
        host.send(json: reply)
    }

    func open() async throws -> SpeechConnection {
        try await SpeechConnection.open(machine: machine, keyStore: keys, discovery: NoDiscovery(), open: supply.opener)
    }

    @Test func sendsAudioThenReturnsTheText() async throws {
        let transport = FakeTransport()
        supply.add(transport)
        var host = FakeHost(transport)
        async let opened = open()
        try await admit(&host)
        let connection = try await opened
        #expect(connection.installed)

        connection.send([0, 1, -1, 2])
        let audio = try #require(try await host.readFrame())
        #expect(audio.kind == .audio && audio.channel == 0)
        // 超出范围的样本截到 ±1。
        #expect(audio.payload == Data([0, 0, 0xff, 0x7f, 0x01, 0x80, 0xff, 0x7f]))
        let finishing = Task { try await connection.finish() }
        #expect(try await host.readControl()?["type"] as? String == "finish")
        host.send(json: #"{"type":"text","text":"你好"}"#)
        #expect(try await finishing.value == "你好")
    }

    @Test func aFailureCarriesTheReason() async throws {
        let transport = FakeTransport()
        supply.add(transport)
        var host = FakeHost(transport)
        async let opened = open()
        try await admit(&host, reply: #"{"type":"ready","installed":false}"#)
        let connection = try await opened
        #expect(!connection.installed)
        let finishing = Task { try await connection.finish() }
        _ = try await host.readControl()
        host.send(json: #"{"type":"failed","reason":"no model"}"#)
        await #expect(throws: SpeechFailure(reason: "no model")) { _ = try await finishing.value }
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
