import Foundation
import RunodeProtocol

/// 断线重连等多久：第 n 次连续失败后等 `base * 2^(n-1)`，最多 `maximum`。
public struct ReconnectPolicy: Hashable, Sendable {
    public var base: Duration
    public var maximum: Duration

    public init(base: Duration = .seconds(1), maximum: Duration = .seconds(15)) {
        self.base = base
        self.maximum = maximum
    }

    public func delay(afterFailures failures: Int) -> Duration {
        guard failures > 0 else { return .zero }
        let factor = 1 << min(failures - 1, 16)
        return min(base * factor, maximum)
    }
}

/// 一台电脑上宿主的连接：找地址、TLS、门禁、`Hello`，之后收发帧，断了自动重连。网络读写、
/// 门禁和重连都在这个 actor 里；Network.framework 的回调只在 `TLSChannel` 里。
public actor HostConnection: HostLink {
    private var machine: MachineRecord
    private let keyStore: any DeviceKeyStore
    private let machines: any MachineStore
    private let discovery: any HostDiscovery
    private var identity: ClientIdentity
    private let policy: ReconnectPolicy
    private let open: TransportOpener

    private var subscribers: [Int: AsyncStream<HostEvent>.Continuation] = [:]
    private var nextSubscriber = 0
    private var state: LinkState = .idle
    /// 门禁和握手都过了的连接；没连上时为空，排着的消息就丢掉。
    private var transport: (any FrameTransport)?
    private var generation: UInt64 = 0
    /// 上层要不要连接：`start` 置真，`stop` 和不能重试的失败置假。
    private var wanted = false
    private var runTask: Task<Void, Never>?
    private var backoffTask: Task<Void, any Error>?
    private var writerTask: Task<Void, Never>?
    private var requestCounter: UInt32 = 0

    /// 发出去的东西先排在这里，由 `writerTask` 按先后写；`send` 因此可以是同步的。
    private nonisolated let outbox: AsyncStream<Outgoing>.Continuation
    private let outgoing: AsyncStream<Outgoing>

    private enum Outgoing: Sendable {
        case message(ClientMsg)
        case input(Data, channel: UInt32, generation: UInt64)
    }

    public init(
        machine: MachineRecord, keyStore: any DeviceKeyStore, machines: any MachineStore,
        discovery: any HostDiscovery, identity: ClientIdentity, policy: ReconnectPolicy = ReconnectPolicy(),
        open: @escaping TransportOpener = { target, fingerprint in
            try await TLSChannel.open(to: target, fingerprint: fingerprint)
        }
    ) {
        self.machine = machine
        self.keyStore = keyStore
        self.machines = machines
        self.discovery = discovery
        self.identity = identity
        self.policy = policy
        self.open = open
        (outgoing, outbox) = AsyncStream.makeStream(of: Outgoing.self)
    }

    deinit {
        outbox.finish()
        for continuation in subscribers.values {
            continuation.finish()
        }
    }

    // MARK: HostLink

    public func events() -> AsyncStream<HostEvent> {
        let (stream, continuation) = AsyncStream.makeStream(of: HostEvent.self)
        let id = nextSubscriber
        nextSubscriber += 1
        subscribers[id] = continuation
        continuation.onTermination = { [weak self] _ in
            Task { await self?.unsubscribe(id) }
        }
        continuation.yield(.state(state))
        if transport != nil {
            continuation.yield(.ready(generation: generation))
        }
        return stream
    }

    public nonisolated func send(_ message: ClientMsg) {
        outbox.yield(.message(message))
    }

    public nonisolated func sendInput(_ data: Data, channel: UInt32, generation: UInt64) {
        outbox.yield(.input(data, channel: channel, generation: generation))
    }

    /// 要连接。上次是不能重试的失败（比如 `unknown_device`：电脑上撤销了这台设备）时不自动再连：
    /// 电脑把这类失败也计入限速，App 每回前台就试一次很快会变成 `rate_limited`。要用户点「重试」
    /// （`reconnectNow`）才再试。
    public func start() {
        if case .failed = state { return }
        wanted = true
        if writerTask == nil {
            let stream = outgoing
            writerTask = Task { [weak self] in
                for await item in stream {
                    await self?.write(item)
                }
            }
        }
        if runTask == nil {
            runTask = Task { await self.run() }
        } else {
            backoffTask?.cancel()
        }
    }

    public func stop() {
        wanted = false
        runTask?.cancel()
        backoffTask?.cancel()
        transport?.close()
    }

    /// 用户要立刻重试：等着重连的马上试，停在不能重试的失败上的也重新开始。
    public func reconnectNow() {
        if case .failed = state {
            state = .idle
        }
        if runTask == nil {
            start()
        } else {
            backoffTask?.cancel()
        }
    }

    /// 下次握手起在 `Hello` 里报新的设备名；这次连接上已经报过的不改。
    public func setDeviceName(_ name: String) {
        identity.deviceName = name
    }

    public func nextRequestId() -> UInt32 {
        requestCounter &+= 1
        if requestCounter == 0 { requestCounter = 1 }
        return requestCounter
    }

    // MARK: 连接

    private func unsubscribe(_ id: Int) {
        subscribers[id] = nil
    }

    private func broadcast(_ event: HostEvent) {
        for continuation in subscribers.values {
            continuation.yield(event)
        }
    }

    private func setState(_ new: LinkState) {
        guard new != state else { return }
        state = new
        broadcast(.state(new))
    }

    /// 连接的主循环：连上、读到断开、等一会儿再连，直到不要了或者遇到不能重试的失败。
    private func run() async {
        var failures = 0
        while wanted, !Task.isCancelled {
            setState(.connecting)
            var reason: String
            do {
                let session = try await establish()
                failures = 0
                generation += 1
                transport = session.transport
                setState(.connected(hostName: session.hostName, address: session.transport.remoteAddress))
                broadcast(.ready(generation: generation))
                reason = await readUntilClosed(session.transport, decoder: session.decoder)
                transport = nil
                session.transport.close()
                // 刚连上又断的（比如宿主交接给新版本）马上重连一次，之后照常退避。
                failures = 1
            } catch let failure as LinkFailure where failure.isFatal {
                wanted = false
                setState(.failed(failure))
                break
            } catch is CancellationError {
                break
            } catch {
                failures += 1
                // 被限速了：直接等最长的退避，不在限速期里接着撞。
                if case .rejected(.rateLimited)? = error as? LinkFailure {
                    failures = max(failures, 16)
                }
                reason = (error as? LinkFailure)?.errorDescription ?? error.localizedDescription
            }
            guard wanted, !Task.isCancelled else { break }
            let delay = policy.delay(afterFailures: failures)
            setState(.waiting(reason: reason, retryAt: .now + TimeInterval(delay.components.seconds)))
            let backoff = Task { try await Task.sleep(for: delay) }
            backoffTask = backoff
            _ = try? await backoff.value
            backoffTask = nil
        }
        transport = nil
        runTask = nil
        if case .failed = state {} else { setState(.idle) }
        // `stop` 之后又 `start` 了：接着跑。
        if wanted { start() }
    }

    private struct Session {
        var transport: any FrameTransport
        var decoder: FrameDecoder
        var hostName: String
    }

    /// 按顺序试各个地址：Bonjour 找到的、上次成功的、二维码里的。不能重试的失败直接报。
    private func establish() async throws -> Session {
        guard let key = try keyStore.key(for: machine.id) else { throw LinkFailure.missingKey }
        let targets = await discovery.targets(for: machine.fingerprint, then: machine.fallbackTargets)
        return try await firstSuccess(of: targets) { target in
            try await handshake(with: target, key: key)
        }
    }

    private func handshake(with target: TransportTarget, key: StoredDeviceKey) async throws -> Session {
        let transport = try await open(target, machine.fingerprint)
        do {
            let outcome = try await GateClient.run(
                over: transport, credential: .auth(deviceId: machine.deviceId, key: key))
            transport.send(try Frame.control(identity.hello).encoded())
            let decoder = try await withTimeout(GateClient.timeout, onTimeout: { transport.close() }) {
                var reader = FrameReader(transport: transport, decoder: outcome.decoder)
                while true {
                    let payload = try await reader.nextControl()
                    switch try JSONDecoder().decode(HostMsg.self, from: payload) {
                    case .welcome: return reader.decoder
                    case .incompatible(_, _, let reason): throw LinkFailure.incompatible(reason)
                    case .goodbye(let reason): throw LinkFailure.closed("宿主断开了连接（\(reason)）")
                    default: continue
                    }
                }
            }
            if let address = transport.remoteAddress, address != machine.lastAddress {
                machine.lastAddress = address
                try? await machines.upsert(machine)
            }
            return Session(transport: transport, decoder: decoder, hostName: outcome.hostName)
        } catch {
            transport.close()
            throw error
        }
    }

    /// 读到连接断开，返回断开的原因。
    private func readUntilClosed(_ transport: any FrameTransport, decoder: FrameDecoder) async -> String {
        var decoder = decoder
        var goodbye: GoodbyeReason?
        do {
            while true {
                while let frame = try decoder.next() {
                    if case .goodbye(let reason)? = handle(frame) {
                        goodbye = reason
                    }
                }
                guard let chunk = try await transport.receive() else {
                    try decoder.finish()
                    break
                }
                decoder.append(chunk)
            }
        } catch let error as FrameError {
            return "收到的数据格式不对（\(error)）"
        } catch {
            if Task.isCancelled { return "已断开" }
            return (error as? LinkFailure)?.errorDescription ?? error.localizedDescription
        }
        switch goodbye {
        case .handoff?: return "电脑上的 Runode 升级了，正在重新连接"
        case .shutdown?: return "电脑上的 Runode 退出了"
        case .error(let message)?: return "电脑断开了连接：\(message)"
        default: return "电脑关闭了连接"
        }
    }

    /// 把一帧交给上层；返回解出来的控制消息。
    @discardableResult
    private func handle(_ frame: Frame) -> HostMsg? {
        switch frame.kind {
        case .control:
            guard let message = try? frame.message(HostMsg.self) else { return nil }
            broadcast(.message(message))
            return message
        case .output, .snapshot:
            broadcast(.frame(frame, generation: generation))
            return nil
        case .input:
            return nil
        }
    }

    private func write(_ item: Outgoing) {
        guard let transport else { return }
        let frame: Frame
        switch item {
        case .message(let message):
            guard let control = try? Frame.control(message) else { return }
            frame = control
        case let .input(data, channel, generation):
            guard generation == self.generation else { return }
            frame = Frame(kind: .input, channel: channel, payload: data)
        }
        guard let bytes = try? frame.encoded() else { return }
        transport.send(bytes)
    }
}
