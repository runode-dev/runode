import Foundation
import RunodeProtocol
import Synchronization

@testable import RunodeConnection

/// 一个方向上的字节块队列：一边 `push`，另一边 `pop` 等着拿。
final class ChunkQueue: Sendable {
    private struct State {
        var chunks: [Data] = []
        var closed = false
        var waiter: CheckedContinuation<Data?, Never>?
    }

    private let state = Mutex(State())

    func push(_ data: Data) {
        let waiter = state.withLock { state -> CheckedContinuation<Data?, Never>? in
            guard !state.closed else { return nil }
            if let waiter = state.waiter {
                state.waiter = nil
                return waiter
            }
            state.chunks.append(data)
            return nil
        }
        waiter?.resume(returning: data)
    }

    func close() {
        let waiter = state.withLock { state -> CheckedContinuation<Data?, Never>? in
            state.closed = true
            defer { state.waiter = nil }
            return state.waiter
        }
        waiter?.resume(returning: nil)
    }

    func pop() async -> Data? {
        await withCheckedContinuation { continuation in
            let immediate = state.withLock { state -> Data?? in
                if !state.chunks.isEmpty { return .some(state.chunks.removeFirst()) }
                if state.closed { return .some(nil) }
                state.waiter = continuation
                return .none
            }
            if let immediate {
                continuation.resume(returning: immediate)
            }
        }
    }
}

/// 不碰网络的连接：宿主那头由测试脚本扮演。
final class FakeTransport: FrameTransport {
    /// 宿主发给客户端的。
    let inbound = ChunkQueue()
    /// 客户端发给宿主的。
    let outbound = ChunkQueue()
    let exporterBytes = Data(repeating: 0x5A, count: 32)
    let remoteAddress: String?

    init(remoteAddress: String? = "192.168.1.20") {
        self.remoteAddress = remoteAddress
    }

    func receive() async throws -> Data? {
        await inbound.pop()
    }

    func send(_ data: Data) {
        outbound.push(data)
    }

    func exporter() throws -> Data {
        exporterBytes
    }

    func close() {
        inbound.close()
        outbound.close()
    }
}

/// 扮演宿主：读客户端发来的帧，回消息。
struct FakeHost {
    let transport: FakeTransport
    var decoder = FrameDecoder()

    init(_ transport: FakeTransport) {
        self.transport = transport
    }

    mutating func readFrame() async throws -> Frame? {
        while true {
            if let frame = try decoder.next() { return frame }
            guard let chunk = await transport.outbound.pop() else { return nil }
            decoder.append(chunk)
        }
    }

    /// 下一条控制消息的 JSON 对象。
    mutating func readControl() async throws -> [String: Any]? {
        while let frame = try await readFrame() {
            if frame.kind == .control {
                return try JSONSerialization.jsonObject(with: frame.payload) as? [String: Any]
            }
        }
        return nil
    }

    func send(json: String) {
        let frame = Frame(kind: .control, channel: 0, payload: Data(json.utf8))
        transport.inbound.push(try! frame.encoded())
    }

    func send(_ message: GateMessage) {
        transport.inbound.push(try! Frame.control(message).encoded())
    }

    func send(frame: Frame) {
        transport.inbound.push(try! frame.encoded())
    }

    static let nonce = Data((0..<32).map { UInt8($0) })

    func challenge(version: UInt32 = 1) {
        send(.challenge(version: version, nonce: Self.nonce, hostName: "Ethan 的 MacBook Pro"))
    }

    static let welcome =
        #"{"type":"welcome","protocol":4,"build":"0.1.0+abc","host_pid":1,"snapshot_format":1,"standalone":true,"handoff":1}"#
}

final class InMemoryKeyStore: DeviceKeyStore {
    private let keys = Mutex<[UUID: StoredDeviceKey]>([:])

    func save(_ key: StoredDeviceKey, for machine: UUID) throws {
        keys.withLock { $0[machine] = key }
    }

    func key(for machine: UUID) throws -> StoredDeviceKey? {
        keys.withLock { $0[machine] }
    }

    func deleteKey(for machine: UUID) throws {
        _ = keys.withLock { $0.removeValue(forKey: machine) }
    }
}

actor InMemoryMachineStore: MachineStore {
    var machines: [MachineRecord] = []

    func all() -> [MachineRecord] { machines }

    func upsert(_ machine: MachineRecord) {
        machines.removeAll { $0.id == machine.id }
        machines.append(machine)
    }

    func remove(id: UUID) {
        machines.removeAll { $0.id == id }
    }
}

/// 交出预先准备好的连接，记下被要了几次、要的是哪个地址。
final class TransportSupply: Sendable {
    private let state = Mutex<(transports: [FakeTransport], targets: [String])>(([], []))

    func add(_ transport: FakeTransport) {
        state.withLock { $0.transports.append(transport) }
    }

    var requestedTargets: [String] { state.withLock { $0.targets } }

    var opener: TransportOpener {
        { target, _ in
            let next = self.state.withLock { state -> FakeTransport? in
                state.targets.append(target.label)
                return state.transports.isEmpty ? nil : state.transports.removeFirst()
            }
            guard let next else { throw LinkFailure.connectionFailed("no more fake transports") }
            return next
        }
    }
}

extension MachineRecord {
    static func sample(id: UUID = UUID()) -> MachineRecord {
        MachineRecord(
            id: id, name: "MacBook", hostName: "Ethan 的 MacBook Pro",
            fingerprint: CertificateFingerprint(bytes: Data(repeating: 1, count: 32))!, port: 7866,
            addresses: ["192.168.1.20", "fd7a::1"], lastAddress: nil, deviceId: "00112233445566778899aabbccddeeff")
    }
}
