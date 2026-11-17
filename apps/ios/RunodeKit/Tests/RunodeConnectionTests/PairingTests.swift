import CryptoKit
import Foundation
import RunodeProtocol
import Synchronization
import Testing

@testable import RunodeConnection

/// 配对：配好了却不要的，为它存的私钥删掉。
@Suite struct PairingTests {
    @Test(.timeLimit(.minutes(1))) func discardingAPairingDeletesItsKey() async throws {
        let keys = MemoryDeviceKeyStore()
        let machine = MachineRecord.sample()
        let other = MachineRecord.sample()
        try keys.save(StoredDeviceKey(kind: .software, data: Data([1]), tunnel: Data(repeating: 5, count: 32)), for: machine.id)
        try keys.save(StoredDeviceKey(kind: .software, data: Data([2])), for: other.id)
        let (closed, closing) = AsyncStream.makeStream(of: Data.self)
        RemotePairing(
            keyStore: keys, discovery: NoDiscovery(),
            closeTunnel: { secret in
                closing.yield(secret)
                closing.finish()
            }
        ).discard(machine)
        #expect(try keys.key(for: machine.id) == nil)
        #expect(try keys.key(for: other.id) != nil, "other machines keep their keys")
        var iterator = closed.makeAsyncIterator()
        #expect(await iterator.next() == Data(repeating: 5, count: 32), "its tunnel endpoint is closed")
    }

    /// 经 iroh 配对时用的端点私钥，就是配对成功后跟着设备私钥存下的那把。
    @Test(.timeLimit(.minutes(1))) func pairingOverTheTunnelStoresItsSecret() async throws {
        let keys = MemoryDeviceKeyStore()
        let tunnelled = FakeTransport(remoteAddress: nil)
        let secrets = Mutex<[Data]>([])
        let pairing = RemotePairing(
            keyStore: keys, discovery: NoDiscovery(), open: TransportSupply().opener,
            tunnel: { _, _, secret in
                secrets.withLock { $0.append(secret) }
                return tunnelled
            },
            generateKey: {
                StoredDeviceKey(
                    kind: .software, data: P256.Signing.PrivateKey().rawRepresentation,
                    tunnel: Data(repeating: 3, count: 32))
            })
        let paired = Task { try await pairing.pair(with: Self.invitation, deviceName: "测试 iPhone") }
        var host = FakeHost(tunnelled)
        host.challenge()
        let request = try #require(try await host.readControl())
        #expect(request["type"] as? String == "remote_pair")
        host.send(.accepted(deviceId: "00112233445566778899aabbccddeeff"))
        let record = try await paired.value
        #expect(secrets.withLock { $0 } == [Data(repeating: 3, count: 32)])
        #expect(try keys.key(for: record.id)?.tunnel == Data(repeating: 3, count: 32))
    }

    /// 经 iroh 配对被门禁拒绝：没有记录认领这把端点私钥，它的端点关掉。
    @Test(.timeLimit(.minutes(1))) func aRefusedTunnelPairingClosesItsEndpoint() async throws {
        let keys = MemoryDeviceKeyStore()
        let tunnelled = FakeTransport(remoteAddress: nil)
        let used = Mutex<[Data]>([])
        let closed = Mutex<[Data]>([])
        let pairing = RemotePairing(
            keyStore: keys, discovery: NoDiscovery(), open: TransportSupply().opener,
            tunnel: { _, _, secret in
                used.withLock { $0.append(secret) }
                return tunnelled
            },
            closeTunnel: { secret in closed.withLock { $0.append(secret) } },
            generateKey: Self.softwareKey)
        let paired = Task { try await pairing.pair(with: Self.invitation, deviceName: "测试 iPhone") }
        var host = FakeHost(tunnelled)
        host.challenge()
        _ = try await host.readControl()
        host.send(.rejected(.pairingInvalid))
        await #expect(throws: LinkFailure.rejected(.pairingInvalid)) { try await paired.value }
        #expect(used.withLock { $0.count } == 1)
        #expect(closed.withLock { $0 } == used.withLock { $0 })
    }

    /// 拨 iroh 时调用方不要了：这次建的端点同样关掉。
    @Test(.timeLimit(.minutes(1))) func aCancelledTunnelPairingClosesItsEndpoint() async throws {
        let keys = MemoryDeviceKeyStore()
        let (dialing, dialed) = AsyncStream.makeStream(of: Data.self)
        let closed = Mutex<[Data]>([])
        let pairing = RemotePairing(
            keyStore: keys, discovery: NoDiscovery(), open: TransportSupply().opener,
            tunnel: { _, _, secret in
                dialed.yield(secret)
                try await Task.sleep(for: .seconds(60))
                throw LinkFailure.noAddress
            },
            closeTunnel: { secret in closed.withLock { $0.append(secret) } },
            generateKey: Self.softwareKey)
        let paired = Task { try await pairing.pair(with: Self.invitation, deviceName: "测试 iPhone") }
        var iterator = dialing.makeAsyncIterator()
        let secret = try #require(await iterator.next())
        paired.cancel()
        await #expect(throws: CancellationError.self) { try await paired.value }
        #expect(closed.withLock { $0 } == [secret])
    }

    @Sendable static func softwareKey() -> StoredDeviceKey {
        StoredDeviceKey(
            kind: .software, data: P256.Signing.PrivateKey().rawRepresentation, tunnel: Data(repeating: 4, count: 32))
    }

    static var invitation: PairingInvitation {
        PairingInvitation(
            hostName: "MacBook", fingerprint: CertificateFingerprint(bytes: Data(repeating: 1, count: 32))!,
            secret: Data(repeating: 2, count: 32), port: 7866, addresses: ["192.168.1.20"],
            expiresAt: .now.addingTimeInterval(600), node: HostConnectionTests.node)
    }
}
