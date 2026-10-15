import Foundation
import Testing

@testable import RunodeConnection

/// 配对：配好了却不要的，为它存的私钥删掉。
@Suite struct PairingTests {
    @Test func discardingAPairingDeletesItsKey() throws {
        let keys = MemoryDeviceKeyStore()
        let machine = MachineRecord.sample()
        let other = MachineRecord.sample()
        try keys.save(StoredDeviceKey(kind: .software, data: Data([1])), for: machine.id)
        try keys.save(StoredDeviceKey(kind: .software, data: Data([2])), for: other.id)
        RemotePairing(keyStore: keys, discovery: NoDiscovery()).discard(machine)
        #expect(try keys.key(for: machine.id) == nil)
        #expect(try keys.key(for: other.id) != nil, "other machines keep their keys")
    }
}
