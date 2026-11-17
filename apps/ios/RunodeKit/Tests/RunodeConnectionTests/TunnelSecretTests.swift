import CryptoKit
import Foundation
import Synchronization
import Testing

@testable import RunodeConnection

/// 每台电脑一把 iroh 端点私钥，跟着设备私钥存：缺了补上，读不了不覆盖，拨号用存着的那把。
@Suite(.timeLimit(.minutes(1))) struct TunnelSecretTests {
    let machine = MachineRecord.sample()
    let keys = MemoryDeviceKeyStore()

    init() throws {
        try keys.save(StoredDeviceKey(kind: .software, data: P256.Signing.PrivateKey().rawRepresentation), for: machine.id)
    }

    @Test func keysStoredWithoutATunnelSecretDecode() throws {
        let key = try JSONDecoder().decode(StoredDeviceKey.self, from: Data(#"{"kind":"software","data":"AQ=="}"#.utf8))
        #expect(key == StoredDeviceKey(kind: .software, data: Data([1])))
        #expect(key.tunnel == nil)
    }

    @Test func generatedKeysCarryATunnelSecret() throws {
        #expect(try StoredDeviceKey.generate().tunnel?.count == 32)
    }

    @Test func aMissingSecretIsGeneratedOnceAndStored() throws {
        let secret = try keys.tunnelSecret(for: machine.id)
        #expect(secret.count == 32)
        #expect(try keys.key(for: machine.id)?.tunnel == secret)
        #expect(try keys.tunnelSecret(for: machine.id) == secret)
    }

    @Test func concurrentCallersShareOneSecret() async throws {
        let keys = keys
        let id = machine.id
        let secrets = try await withThrowingTaskGroup(of: Data.self) { group in
            for _ in 0..<32 {
                group.addTask { try keys.tunnelSecret(for: id) }
            }
            return try await group.reduce(into: Set<Data>()) { $0.insert($1) }
        }
        #expect(secrets.count == 1)
        #expect(try keys.key(for: id)?.tunnel == secrets.first)
    }

    @Test func noDeviceKeyMeansNoSecret() {
        #expect(throws: LinkFailure.missingKey) { try MemoryDeviceKeyStore().tunnelSecret(for: UUID()) }
    }

    /// 钥匙串读不了时不当成没有：生成新的存回去会把设备私钥一起盖掉。
    @Test func aFailedReadIsNotOverwritten() {
        let store = FlakyKeyStore(failReads: true)
        #expect(throws: LinkFailure.keychain("locked")) { try store.tunnelSecret(for: machine.id) }
        #expect(store.saves.withLock { $0 } == 0)
    }

    /// 存不进去时照常连，用一把临时的；下次拨号还用这把，不另建端点。删掉电脑时交出它、不再记着。
    @Test func aFailedSaveKeepsOneTemporarySecret() throws {
        let store = FlakyKeyStore(failReads: false)
        let secret = try store.tunnelSecret(for: machine.id)
        #expect(secret.count == 32)
        #expect(try store.tunnelSecret(for: machine.id) == secret)
        #expect(store.saves.withLock { $0 } == 2, "each dial tries to store it again")
        #expect(store.deleteKeyAndTunnel(for: machine.id) == [secret])
        #expect(try store.tunnelSecret(for: machine.id) != secret)
        _ = store.deleteKeyAndTunnel(for: machine.id)
    }

    @Test func deletingTheKeyHandsBackItsSecret() throws {
        let secret = try keys.tunnelSecret(for: machine.id)
        #expect(keys.deleteKeyAndTunnel(for: machine.id) == [secret])
        #expect(try keys.key(for: machine.id) == nil)
        #expect(keys.deleteKeyAndTunnel(for: machine.id).isEmpty)
    }

    /// 拨号时补上缺的私钥，交给 iroh 的就是存下的那把。
    @Test func dialPassesTheStoredSecret() async throws {
        var remote = machine
        remote.node = HostConnectionTests.node
        let secrets = Mutex<[Data]>([])
        let address = try await dial(
            remote, keyStore: keys, discovery: NoDiscovery(), open: TransportSupply().opener,
            tunnel: { _, _, secret in
                secrets.withLock { $0.append(secret) }
                return FakeTransport(remoteAddress: "iroh")
            },
            handshake: { transport, _ in transport.remoteAddress ?? "" }, close: { _ in })
        #expect(address == "iroh")
        let stored = try #require(try keys.key(for: machine.id)?.tunnel)
        #expect(secrets.withLock { $0 } == [stored])
    }
}

/// 读或者存总是失败的钥匙串。
private final class FlakyKeyStore: DeviceKeyStore {
    let failReads: Bool
    let saves = Mutex(0)

    init(failReads: Bool) {
        self.failReads = failReads
    }

    func save(_ key: StoredDeviceKey, for machine: UUID) throws {
        saves.withLock { $0 += 1 }
        throw LinkFailure.keychain("read-only")
    }

    func key(for machine: UUID) throws -> StoredDeviceKey? {
        if failReads { throw LinkFailure.keychain("locked") }
        return StoredDeviceKey(kind: .software, data: Data([1]))
    }

    func deleteKey(for machine: UUID) throws {}
}
