import Foundation
import Testing

@testable import RunodeProtocol

/// Mac 那边放的门禁样例目录：`crates/protocol/tests/fixtures/remote`。还没有时相关测试跳过。
let remoteFixtureDirectory: URL = URL(filePath: #filePath)
    .deletingLastPathComponent()  // RunodeProtocolTests
    .deletingLastPathComponent()  // Tests
    .deletingLastPathComponent()  // RunodeKit
    .deletingLastPathComponent()  // ios
    .deletingLastPathComponent()  // apps
    .deletingLastPathComponent()  // 仓库根
    .appending(path: "crates/protocol/tests/fixtures/remote")

func remoteFixtures() -> [URL] {
    let files = (try? FileManager.default.contentsOfDirectory(at: remoteFixtureDirectory, includingPropertiesForKeys: nil))
    return (files ?? []).filter { $0.pathExtension == "json" }.sorted { $0.path < $1.path }
}

@Suite struct GateTests {
    @Test func base64urlHasNoPadding() {
        #expect(Base64URL.encode(Data([0xFB, 0xFF])) == "-_8")
        #expect(Base64URL.decode("-_8") == Data([0xFB, 0xFF]))
        #expect(Base64URL.decode("-_8=") == Data([0xFB, 0xFF]))
        #expect(Base64URL.decode("+/8") == nil)
        #expect(Base64URL.decode("a") == nil)
        let bytes = Data((0..<32).map { UInt8($0) })
        #expect(Base64URL.decode(Base64URL.encode(bytes)) == bytes)
        #expect(Base64URL.encode(bytes).count == 43)
    }

    @Test func signedBytesAreConcatenatedInOrder() {
        let nonce = Data(repeating: 0x11, count: 32)
        let exporter = Data(repeating: 0x22, count: 32)
        let auth = GateSignature.payload(nonce: nonce, exporter: exporter, purpose: .auth)
        #expect(auth.count == 16 + 32 + 32 + 4)
        #expect(auth.prefix(16) == Data("runode-remote-v1".utf8))
        #expect(auth[16..<48] == nonce)
        #expect(auth[48..<80] == exporter)
        #expect(auth.suffix(4) == Data("auth".utf8))
        let pair = GateSignature.payload(nonce: nonce, exporter: exporter, purpose: .pair)
        #expect(pair.suffix(4) == Data("pair".utf8))
        #expect(GateSignature.exporterLabel == "EXPORTER-runode-remote")
    }

    @Test func challengeAndVerdictsDecode() throws {
        let nonce = Data(repeating: 7, count: 32)
        let challenge = Data(
            #"{"type":"remote_challenge","version":1,"nonce":"\#(Base64URL.encode(nonce))","host_name":"Ethan 的 MacBook Pro"}"#
                .utf8)
        #expect(
            try JSONDecoder().decode(GateMessage.self, from: challenge)
                == .challenge(version: 1, nonce: nonce, hostName: "Ethan 的 MacBook Pro"))
        let accepted = Data(#"{"type":"remote_accepted","device_id":"00112233445566778899aabbccddeeff"}"#.utf8)
        #expect(
            try JSONDecoder().decode(GateMessage.self, from: accepted)
                == .accepted(deviceId: "00112233445566778899aabbccddeeff"))
        for (kind, reason) in [
            ("unknown_device", GateRejection.unknownDevice), ("bad_signature", .badSignature),
            ("pairing_invalid", .pairingInvalid), ("rate_limited", .rateLimited), ("disabled", .disabled),
            ("solar_flare", .unknown("solar_flare")),
        ] {
            let rejected = Data(#"{"type":"remote_rejected","reason":{"kind":"\#(kind)"}}"#.utf8)
            #expect(try JSONDecoder().decode(GateMessage.self, from: rejected) == .rejected(reason))
        }
    }

    @Test func repliesEncodeWithBase64url() throws {
        let auth = GateMessage.auth(deviceId: "00112233445566778899aabbccddeeff", signature: Data([0xFB, 0xFF]))
        let expected = Data(#"{"type":"remote_auth","device_id":"00112233445566778899aabbccddeeff","signature":"-_8"}"#.utf8)
        #expect(try sameJSON(JSONEncoder().encode(auth), expected))
        let pair = GateMessage.pair(
            secret: Data([1]), deviceName: "Ethan 的 iPhone", publicKey: Data([2]), signature: Data([3]))
        let pairExpected = Data(
            #"{"type":"remote_pair","secret":"AQ","device_name":"Ethan 的 iPhone","public_key":"Ag","signature":"Aw"}"#.utf8)
        #expect(try sameJSON(JSONEncoder().encode(pair), pairExpected))
    }

    /// Mac 那边的门禁消息样例（`remote_*.json`）：每个都能读；认识的读出来再写回去和原文一样，
    /// 以后才有的拒绝原因读成 `unknown`。
    @Test(.enabled(if: !remoteFixtures().isEmpty, "Mac 侧还没放门禁样例"))
    func macGateMessagesRoundTrip() throws {
        for url in remoteFixtures() where url.lastPathComponent.hasPrefix("remote_") {
            let data = try Data(contentsOf: url)
            let message = try JSONDecoder().decode(GateMessage.self, from: data)
            switch message {
            case .unknown(let type):
                Issue.record("\(url.lastPathComponent): unknown gate message \(type)")
            case .rejected(.unknown(let kind)):
                #expect(url.lastPathComponent.contains("future"), "\(url.lastPathComponent): \(kind)")
            default:
                #expect(try sameJSON(JSONEncoder().encode(message), data), "\(url.lastPathComponent)")
            }
        }
    }

    /// Mac 那边被签字节串的样例（`signed_bytes_*.json`）和这里拼出来的一样。
    @Test(.enabled(if: !remoteFixtures().isEmpty, "Mac 侧还没放门禁样例"))
    func macSignedBytesMatch() throws {
        struct Sample: Decodable {
            var nonce: String
            var exporter: String
            var purpose: String
            var signed: String
        }
        let files = remoteFixtures().filter { $0.lastPathComponent.hasPrefix("signed_bytes_") }
        #expect(!files.isEmpty)
        for url in files {
            let sample = try JSONDecoder().decode(Sample.self, from: Data(contentsOf: url))
            let purpose = try #require(GatePurpose(rawValue: sample.purpose))
            let payload = GateSignature.payload(
                nonce: try #require(Base64URL.decode(sample.nonce)),
                exporter: try #require(Base64URL.decode(sample.exporter)), purpose: purpose)
            #expect(Base64URL.encode(payload) == sample.signed, "\(url.lastPathComponent)")
        }
    }

    /// Mac 那边配对链接的样例（`pair_uri.json`）解出来的各项和它列的一样。
    @Test(.enabled(if: FileManager.default.fileExists(atPath: remoteFixtureDirectory.appending(path: "pair_uri.json").path)))
    func macPairingLinkParses() throws {
        struct Sample: Decodable {
            var uri: String
            var host_name: String
            var fingerprint: String
            var secret: String
            var port: UInt16
            var addrs: [String]
            var expires_at: TimeInterval
        }
        let sample = try JSONDecoder().decode(
            Sample.self, from: Data(contentsOf: remoteFixtureDirectory.appending(path: "pair_uri.json")))
        let invitation = try PairingInvitation.parse(sample.uri)
        #expect(invitation.hostName == sample.host_name)
        #expect(invitation.fingerprint.description == sample.fingerprint)
        #expect(Base64URL.encode(invitation.secret) == sample.secret)
        #expect(invitation.port == sample.port)
        #expect(invitation.addresses == sample.addrs)
        #expect(invitation.expiresAt == Date(timeIntervalSince1970: sample.expires_at))
    }
}
