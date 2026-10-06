import CryptoKit
import Foundation
import RunodeProtocol
import Testing

@testable import RunodeConnection

@Suite struct GateClientTests {
    let key = StoredDeviceKey(kind: .software, data: P256.Signing.PrivateKey().rawRepresentation)

    /// 验 `remote_auth`/`remote_pair` 里的签名：签的是 nonce、exporter 和用途拼起来的字节串。
    func verify(_ signature: String, purpose: GatePurpose, transport: FakeTransport) throws -> Bool {
        let publicKey = try P256.Signing.PublicKey(x963Representation: key.publicKeyX963)
        let der = try #require(Base64URL.decode(signature))
        let payload = GateSignature.payload(nonce: FakeHost.nonce, exporter: transport.exporterBytes, purpose: purpose)
        return publicKey.isValidSignature(try P256.Signing.ECDSASignature(derRepresentation: der), for: payload)
    }

    @Test func logsInWithASignedAuth() async throws {
        let transport = FakeTransport()
        var host = FakeHost(transport)
        host.challenge()
        async let outcome = GateClient.run(
            over: transport, credential: .auth(deviceId: "00112233445566778899aabbccddeeff", key: key))
        let auth = try #require(try await host.readControl())
        #expect(auth["type"] as? String == "remote_auth")
        #expect(auth["device_id"] as? String == "00112233445566778899aabbccddeeff")
        #expect(try verify(auth["signature"] as! String, purpose: .auth, transport: transport))
        host.send(.accepted(deviceId: "00112233445566778899aabbccddeeff"))
        let result = try await outcome
        #expect(result.deviceId == "00112233445566778899aabbccddeeff")
        #expect(result.hostName == "Ethan 的 MacBook Pro")
        // 门禁过后放开单帧上限，之后照常读。
        #expect(result.decoder.maxPayload == Frame.maxPayload)
    }

    @Test func pairsWithAPublicKey() async throws {
        let transport = FakeTransport()
        var host = FakeHost(transport)
        host.challenge()
        let secret = Data(repeating: 0xAB, count: 32)
        async let outcome = GateClient.run(
            over: transport, credential: .pair(secret: secret, deviceName: "Ethan 的 iPhone", key: key))
        let pair = try #require(try await host.readControl())
        #expect(pair["type"] as? String == "remote_pair")
        #expect(pair["device_name"] as? String == "Ethan 的 iPhone")
        #expect(Base64URL.decode(pair["secret"] as! String) == secret)
        let publicKey = try #require(Base64URL.decode(pair["public_key"] as! String))
        #expect(publicKey.count == 65)
        #expect(publicKey.first == 0x04)
        #expect(try verify(pair["signature"] as! String, purpose: .pair, transport: transport))
        host.send(.accepted(deviceId: "ffeeddccbbaa99887766554433221100"))
        #expect(try await outcome.deviceId == "ffeeddccbbaa99887766554433221100")
    }

    @Test func rejectionIsReported() async throws {
        let transport = FakeTransport()
        var host = FakeHost(transport)
        host.challenge()
        let key = key
        let outcome = Task {
            try await GateClient.run(
                over: transport, credential: .pair(secret: Data(count: 32), deviceName: "x", key: key))
        }
        _ = try await host.readControl()
        host.send(.rejected(.pairingInvalid))
        await #expect(throws: LinkFailure.rejected(.pairingInvalid)) { try await outcome.value }
    }

    @Test func aNewerGateVersionAsksForAnUpgrade() async throws {
        let transport = FakeTransport()
        FakeHost(transport).challenge(version: 2)
        await #expect(throws: LinkFailure.gateVersion(2)) {
            try await GateClient.run(over: transport, credential: .auth(deviceId: "x", key: key))
        }
    }

    @Test func aSilentHostTimesOut() async throws {
        let transport = FakeTransport()
        await #expect(throws: LinkFailure.timeout) {
            try await GateClient.run(
                over: transport, credential: .auth(deviceId: "x", key: key), timeout: .milliseconds(100))
        }
    }

    @Test func oversizedGateFramesAreRefused() async throws {
        let transport = FakeTransport()
        FakeHost(transport).send(frame: Frame(kind: .control, channel: 0, payload: Data(count: 17 * 1024)))
        await #expect(throws: FrameError.tooLong(17 * 1024)) {
            try await GateClient.run(over: transport, credential: .auth(deviceId: "x", key: key))
        }
    }
}

@Suite struct DeviceKeyTests {
    @Test func softwareKeysSignVerifiably() throws {
        let key = StoredDeviceKey(kind: .software, data: P256.Signing.PrivateKey().rawRepresentation)
        let payload = Data("runode".utf8)
        let signature = try P256.Signing.ECDSASignature(derRepresentation: key.signature(for: payload))
        let publicKey = try P256.Signing.PublicKey(x963Representation: key.publicKeyX963)
        #expect(publicKey.isValidSignature(signature, for: payload))
        #expect(try key.publicKeyX963.count == 65)
    }

    /// Rust 那边用 OpenSSL 签的样例（`signature_p256.json`），CryptoKit 验得过：两边对 DER 签名和
    /// X9.63 公钥的理解一样。
    @Test(.enabled(if: FileManager.default.fileExists(atPath: signatureFixture.path)))
    func macSignatureFixtureVerifies() throws {
        struct Sample: Decodable {
            var public_key: String
            var signed: String
            var signature: String
        }
        let sample = try JSONDecoder().decode(Sample.self, from: Data(contentsOf: signatureFixture))
        let publicKey = try P256.Signing.PublicKey(x963Representation: try #require(Base64URL.decode(sample.public_key)))
        let signature = try P256.Signing.ECDSASignature(derRepresentation: try #require(Base64URL.decode(sample.signature)))
        #expect(publicKey.isValidSignature(signature, for: try #require(Base64URL.decode(sample.signed))))
    }
}

let signatureFixture = URL(filePath: #filePath)
    .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    .appending(path: "crates/protocol/tests/fixtures/remote/signature_p256.json")
