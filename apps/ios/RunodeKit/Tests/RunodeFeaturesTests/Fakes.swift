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
        var inputs: [(data: Data, channel: UInt32, generation: UInt64)] = []
        var subscribers: [AsyncStream<HostEvent>.Continuation] = []
        var starts = 0
        var stops = 0
        var reconnects = 0
        var request: UInt32 = 0
        var deviceNames: [String] = []
    }

    private let state = Mutex(State())

    var sent: [ClientMsg] { state.withLock { $0.sent } }
    var inputs: [(data: Data, channel: UInt32, generation: UInt64)] { state.withLock { $0.inputs } }
    var starts: Int { state.withLock { $0.starts } }
    var stops: Int { state.withLock { $0.stops } }
    var subscriberCount: Int { state.withLock { $0.subscribers.count } }
    var deviceNames: [String] { state.withLock { $0.deviceNames } }

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
        state.withLock { $0.sent.append(message) }
    }

    func sendInput(_ data: Data, channel: UInt32, generation: UInt64) {
        state.withLock { $0.inputs.append((data, channel, generation)) }
    }

    func start() async { state.withLock { $0.starts += 1 } }
    func stop() async { state.withLock { $0.stops += 1 } }
    func reconnectNow() async { state.withLock { $0.reconnects += 1 } }
    func setDeviceName(_ name: String) async { state.withLock { $0.deviceNames.append(name) } }

    func nextRequestId() async -> UInt32 {
        state.withLock {
            $0.request += 1
            return $0.request
        }
    }
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
