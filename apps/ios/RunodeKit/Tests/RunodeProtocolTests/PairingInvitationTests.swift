import Foundation
import Testing

@testable import RunodeProtocol

@Suite struct PairingInvitationTests {
    let fingerprint = Data((0..<32).map { UInt8($0) })
    let secret = Data(repeating: 0xAB, count: 32)

    func link(
        v: String = "1", name: String = "Ethan%20%E7%9A%84%20MacBook%20Pro", fp: String? = nil,
        secret: String? = nil, port: String = "7866", addr: String = "192.168.1.20,fd7a:115c:a1e0::1",
        exp: String = "1900000000"
    ) -> String {
        let fp = fp ?? Base64URL.encode(fingerprint)
        let secret = secret ?? Base64URL.encode(self.secret)
        return "runode://pair?v=\(v)&name=\(name)&fp=\(fp)&secret=\(secret)&port=\(port)&addr=\(addr)&exp=\(exp)"
    }

    @Test func parsesAFullLink() throws {
        let invitation = try PairingInvitation.parse("  \(link())\n")
        #expect(invitation.hostName == "Ethan 的 MacBook Pro")
        #expect(invitation.fingerprint.bytes == fingerprint)
        #expect(invitation.secret == secret)
        #expect(invitation.port == 7866)
        #expect(invitation.addresses == ["192.168.1.20", "fd7a:115c:a1e0::1"])
        #expect(invitation.expiresAt == Date(timeIntervalSince1970: 1_900_000_000))
        #expect(!invitation.isExpired(at: Date(timeIntervalSince1970: 1_800_000_000)))
        #expect(invitation.isExpired(at: Date(timeIntervalSince1970: 1_900_000_000)))
    }

    @Test func acceptsAHexFingerprintToo() throws {
        let hex = fingerprint.map { String(format: "%02x", $0) }.joined()
        #expect(try PairingInvitation.parse(link(fp: hex)).fingerprint.bytes == fingerprint)
    }

    @Test func emptyAddressListIsAllowed() throws {
        #expect(try PairingInvitation.parse(link(addr: "")).addresses.isEmpty)
    }

    @Test func rejectsBadLinks() {
        #expect(throws: PairingInvitationError.notAnInvitation) { try PairingInvitation.parse("https://example.com") }
        #expect(throws: PairingInvitationError.notAnInvitation) { try PairingInvitation.parse("runode://open?v=1") }
        #expect(throws: PairingInvitationError.unsupportedVersion(2)) { try PairingInvitation.parse(link(v: "2")) }
        #expect(throws: PairingInvitationError.invalid("fp")) { try PairingInvitation.parse(link(fp: "AAAA")) }
        #expect(throws: PairingInvitationError.invalid("secret")) { try PairingInvitation.parse(link(secret: "AQ")) }
        #expect(throws: PairingInvitationError.invalid("port")) { try PairingInvitation.parse(link(port: "70000")) }
        #expect(throws: PairingInvitationError.missing("exp")) {
            try PairingInvitation.parse(link().replacingOccurrences(of: "&exp=1900000000", with: ""))
        }
    }
}
