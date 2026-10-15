import Foundation
import RunodeConnection
import RunodeProtocol
import RunodeTerminal
import Synchronization

@testable import RunodeFeatures

/// 假的连接：记下发出去的东西，事件由测试推。
final class FakeLink: HostLink {
    private struct State {
        var sent: [ClientMsg] = []
        var keepAwakes = 0
        var inputs: [(data: Data, channel: UInt32, generation: UInt64)] = []
        var subscribers: [AsyncStream<HostEvent>.Continuation] = []
        var starts = 0
        var stops = 0
        var reconnects = 0
        var request: UInt32 = 0
        var deviceNames: [String] = []
        var focused: [Bool] = []
    }

    private let state = Mutex(State())

    var sent: [ClientMsg] { state.withLock { $0.sent } }
    /// 发过几次 `keepAwake`；它们按时发，不记进 `sent`。
    var keepAwakes: Int { state.withLock { $0.keepAwakes } }
    var inputs: [(data: Data, channel: UInt32, generation: UInt64)] { state.withLock { $0.inputs } }
    var starts: Int { state.withLock { $0.starts } }
    var stops: Int { state.withLock { $0.stops } }
    var subscriberCount: Int { state.withLock { $0.subscribers.count } }
    var deviceNames: [String] { state.withLock { $0.deviceNames } }
    /// 每次 `setFocused` 设的值。
    var focused: [Bool] { state.withLock { $0.focused } }

    func clearSent() {
        state.withLock { $0.sent.removeAll() }
    }

    func emit(_ event: HostEvent) {
        for continuation in state.withLock({ $0.subscribers }) {
            continuation.yield(event)
        }
    }

    func events() async -> AsyncStream<HostEvent> {
        let (stream, continuation) = AsyncStream.makeStream(of: HostEvent.self)
        state.withLock { $0.subscribers.append(continuation) }
        return stream
    }

    func send(_ message: ClientMsg) {
        state.withLock {
            if message == .keepAwake { $0.keepAwakes += 1 } else { $0.sent.append(message) }
        }
    }

    func sendInput(_ data: Data, channel: UInt32, generation: UInt64) {
        state.withLock { $0.inputs.append((data, channel, generation)) }
    }

    func start() async { state.withLock { $0.starts += 1 } }
    func stop() async { state.withLock { $0.stops += 1 } }
    func reconnectNow() async { state.withLock { $0.reconnects += 1 } }
    func setDeviceName(_ name: String) async { state.withLock { $0.deviceNames.append(name) } }
    func setFocused(_ focused: Bool) { state.withLock { $0.focused.append(focused) } }

    func nextRequestId() async -> UInt32 {
        state.withLock {
            $0.request += 1
            return $0.request
        }
    }
}

/// 取请求编号时停住、等测试 `release` 才给的连接，用来测取编号的那段时间里连接断了的情形。别的都交给
/// 里面的 `FakeLink`。
final class HeldRequestLink: HostLink {
    let inner = FakeLink()
    private let held = Mutex<(released: Bool, waiters: [CheckedContinuation<Void, Never>])>((false, []))

    /// 正停在取编号上的调用有几个。
    var waiting: Int { held.withLock { $0.waiters.count } }

    /// 放行停着的，以后取编号不再停。
    func release() {
        let waiters = held.withLock {
            $0.released = true
            defer { $0.waiters = [] }
            return $0.waiters
        }
        for waiter in waiters {
            waiter.resume()
        }
    }

    /// 以后取编号又停住，直到再 `release`。
    func hold() {
        held.withLock { $0.released = false }
    }

    func nextRequestId() async -> UInt32 {
        await withCheckedContinuation { continuation in
            let released = held.withLock {
                if !$0.released { $0.waiters.append(continuation) }
                return $0.released
            }
            if released { continuation.resume() }
        }
        return await inner.nextRequestId()
    }

    func events() async -> AsyncStream<HostEvent> { await inner.events() }
    func send(_ message: ClientMsg) { inner.send(message) }
    func sendInput(_ data: Data, channel: UInt32, generation: UInt64) {
        inner.sendInput(data, channel: channel, generation: generation)
    }
    func start() async { await inner.start() }
    func stop() async { await inner.stop() }
    func reconnectNow() async { await inner.reconnectNow() }
    func setDeviceName(_ name: String) async { await inner.setDeviceName(name) }
    func setFocused(_ focused: Bool) { inner.setFocused(focused) }
}

/// 记下视图模型让它做的事的假显示。
@MainActor
final class FakeDisplay: TerminalDisplay {
    var resets = 0
    var changes = 0
    var bells = 0
    /// 每次尺寸方式变了时报的「是不是适配手机」。
    var sizeModes: [Bool] = []
    var keyboardRequests = 0
    var settings: TermSettings?
    weak var terminal: VTerminal?

    func terminalDidReset(_ terminal: VTerminal?, settings: TermSettings) {
        resets += 1
        self.terminal = terminal
        self.settings = settings
    }

    func terminalContentDidChange() { changes += 1 }
    func terminalSettingsDidChange(_ settings: TermSettings) { self.settings = settings }
    func terminalDidRingBell() { bells += 1 }
    func terminalSizeModeDidChange(fitsPhone: Bool) { sizeModes.append(fitsPhone) }
    func terminalShowKeyboard() { keyboardRequests += 1 }
}

/// 假的配对：按预先给的结果回。
struct FakePairing: Pairing {
    var result: @Sendable (PairingInvitation) throws -> MachineRecord

    func pair(with invitation: PairingInvitation, deviceName: String) async throws -> MachineRecord {
        try result(invitation)
    }
}

/// 等 `gate` 里来一个值才配好的假配对；被撤掉时照样配好，看调用方还存不存。像真的配对一样把私钥存进
/// `keys`，`discard` 时删掉。
struct GatedPairing: Pairing {
    let gate: AsyncStream<Void>
    var keys = MemoryDeviceKeyStore()
    /// 配好的电脑，每次都是它。
    var machine = machineRecord()

    func pair(with invitation: PairingInvitation, deviceName: String) async throws -> MachineRecord {
        for await _ in gate { break }
        try keys.save(StoredDeviceKey(kind: .software, data: Data([1])), for: machine.id)
        return machine
    }

    func discard(_ machine: MachineRecord) {
        try? keys.deleteKey(for: machine.id)
    }
}

let sessionA = SessionId("0123456789abcdef0011223344556677")!
let sessionB = SessionId("ffffffffffffffff0000000000000000")!
let sessionC = SessionId("cccccccccccccccc0000000000000000")!
let smallGrid = GridSize(cols: 20, rows: 4, cellWidthPx: 8, cellHeightPx: 16)

func machineRecord(name: String = "MacBook", fingerprintByte: UInt8 = 1) -> MachineRecord {
    MachineRecord(
        name: name, hostName: "Ethan 的 MacBook Pro",
        fingerprint: CertificateFingerprint(bytes: Data(repeating: fingerprintByte, count: 32))!, port: 7866,
        addresses: ["192.168.1.20"], lastAddress: nil, deviceId: "00112233445566778899aabbccddeeff")
}

/// 等到条件成立，最多等一秒。视图模型在自己的任务里消费事件，测试据此等它处理完。
@MainActor
func eventually(_ condition: @MainActor () -> Bool) async -> Bool {
    for _ in 0..<200 {
        if condition() { return true }
        try? await Task.sleep(for: .milliseconds(5))
    }
    return condition()
}
