import CryptoKit
import Foundation
import Network
import RunodeProtocol
import Security
import Synchronization
import Testing

@testable import RunodeConnection

/// 证书固定：`TLSChannel` 只认配对时记下的叶子证书指纹，这是挡住冒充电脑的唯一一道防线。对着本机起一个
/// 用自签证书的 TLS 1.3 服务端真连一次。
@Suite struct TLSChannelTests {
    /// 测试用的自签证书和私钥（P-256，CN=runode-test，口令 `runode`），只在这里用。
    static let identityP12 = """
        MIIDIgIBAzCCAugGCSqGSIb3DQEHAaCCAtkEggLVMIIC0TCCAccGCSqGSIb3DQEHBqCCAbgwggG0AgEAMIIBrQYJKoZIhvcNAQcB
        MBwGCiqGSIb3DQEMAQYwDgQIqUkzFaDJ8TcCAggAgIIBgG2gQbpvOXpaXKctHO+zNqC6LNkcemmdV8VUO8hkSVP9rJvay3MHfuTp
        TsaSHVzIUau70PZgPVNinnLw8mzqdviOvzPYsr38oNH0SlZTEKkyzdviIVsgkzOdJLf43cSW4ghkK48xbBcp7HgEYGlfXoEF0zmR
        EoqTSurwMUaioXlhINNKfXwWAQZsbtCDOhnjfgDDdrq61Md6dUt9n3EiU53IrVRIcKNisFeXhxA8vYhsBFvTTJaKQ1TmWOWDXHU7
        sjb3nELrbvJhoZMjl8Sg760xZdIeW77TxqW6cDR8h0fFYN9k6lkndVtJ1KkEg92b019yFz4XcQZgEa4Kke+Pfp19YWOnqL0u/SMo
        LJ2p66fY+ioma4RoUs6TOhwiYTsJsfkAYZQm+PWCddSRVAZ9Hn83iHqgeKIyuSYWFIedocKDImX9gfhVwfYzHL2ILzh7mrxspEEX
        o5Y7M9SS3YBFyaJIwRtnOOChzdCEP/XGm9vHbC1fx2ImYGdO8v7woxx8cjCCAQIGCSqGSIb3DQEHAaCB9ASB8TCB7jCB6wYLKoZI
        hvcNAQwKAQKggbQwgbEwHAYKKoZIhvcNAQwBAzAOBAg6XsZH02uv4wICCAAEgZDj5qiJeEjyDxf/neVGPRB0c6jNLt2t2y1RG5GY
        B9SRwMWgkSHtyC6mTlC+1QwajACvWIQQBadzKs5HhaUGAuxNBoTLtLWuVKvzgJnYnp3IO3J02FvLY/u9NlTrr7mP7JWGebd/Wufb
        gNVM0sPFFAXVgmCmTiZfwjqWejmNfNIDj8TZvAiOzCw97iAAWHMJkO0xJTAjBgkqhkiG9w0BCRUxFgQUAIxhEyWMXQcwJq9sMWOZ
        O32Wl/YwMTAhMAkGBSsOAwIaBQAEFHMqBVx29ttekGsdD076izFXL6hqBAg+UnLTwYqh0wICCAA=
        """

    /// 本机上的 TLS 服务端：接下连接、收着不回话，测试结束时关掉。
    final class Server: Sendable {
        let listener: NWListener
        let fingerprint: CertificateFingerprint
        private let accepted = Accepted()

        /// 接下的连接，关服务端时一起关。
        private final class Accepted: Sendable {
            let connections = Mutex<[NWConnection]>([])
        }

        init() async throws {
            let p12 = try #require(Data(base64Encoded: Self.compact(TLSChannelTests.identityP12)))
            var items: CFArray?
            let options: [String: Any] = [kSecImportExportPassphrase as String: "runode", kSecImportToMemoryOnly as String: true]
            let status = SecPKCS12Import(p12 as CFData, options as CFDictionary, &items)
            try #require(status == errSecSuccess, "SecPKCS12Import: \(status)")
            let first = try #require((items as? [[String: Any]])?.first)
            let identity = first[kSecImportItemIdentity as String] as! SecIdentity
            var certificate: SecCertificate?
            SecIdentityCopyCertificate(identity, &certificate)
            let der = SecCertificateCopyData(try #require(certificate)) as Data
            fingerprint = try #require(CertificateFingerprint(bytes: Data(SHA256.hash(data: der))))

            let tls = NWProtocolTLS.Options()
            sec_protocol_options_set_local_identity(tls.securityProtocolOptions, try #require(sec_identity_create(identity)))
            listener = try NWListener(using: NWParameters(tls: tls), on: .any)
            let queue = DispatchQueue(label: "dev.runode.tls-test")
            listener.newConnectionHandler = { [accepted] connection in
                accepted.connections.withLock { $0.append(connection) }
                connection.start(queue: queue)
            }
            let once = OnceFlag()
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, any Error>) in
                listener.stateUpdateHandler = { state in
                    switch state {
                    case .ready:
                        if once.claim() { continuation.resume() }
                    case .failed(let error):
                        if once.claim() { continuation.resume(throwing: error) }
                    default:
                        break
                    }
                }
                listener.start(queue: queue)
            }
        }

        private static func compact(_ text: String) -> String {
            text.filter { !$0.isWhitespace }
        }

        var target: TransportTarget {
            .address("127.0.0.1", port: listener.port!.rawValue)!
        }

        func stop() {
            listener.cancel()
            for connection in accepted.connections.withLock({ $0 }) {
                connection.cancel()
            }
        }
    }

    @Test func connectsWhenTheFingerprintMatches() async throws {
        let server = try await Server()
        defer { server.stop() }
        let channel = try await TLSChannel.open(to: server.target, fingerprint: server.fingerprint)
        #expect(channel.remoteAddress == "127.0.0.1")
        #expect(try channel.exporter().count == GateSignature.exporterLength)
        channel.close()
    }

    @Test func refusesAnotherCertificate() async throws {
        let server = try await Server()
        defer { server.stop() }
        var other = server.fingerprint.bytes
        other[0] ^= 0xFF
        let wrong = try #require(CertificateFingerprint(bytes: other))
        await #expect(throws: LinkFailure.fingerprintMismatch) {
            try await TLSChannel.open(to: server.target, fingerprint: wrong)
        }
    }
}
