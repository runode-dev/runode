import Foundation
import RunodeProtocol
import Synchronization

/// 断线重连等多久：第 n 次连续失败后等 `base * 2^(n-1)`，最多 `maximum`；用户没在看的电脑
/// （`HostLink.setFocused`）最多 `idleMaximum`。
public struct ReconnectPolicy: Hashable, Sendable {
    public var base: Duration
    public var maximum: Duration
    public var idleMaximum: Duration

    public init(base: Duration = .seconds(1), maximum: Duration = .seconds(15), idleMaximum: Duration = .seconds(60)) {
        self.base = base
        self.maximum = maximum
        self.idleMaximum = idleMaximum
    }

    public func delay(afterFailures failures: Int, focused: Bool = true) -> Duration {
        guard failures > 0 else { return .zero }
        let factor = 1 << min(failures - 1, 16)
        return min(base * factor, focused ? maximum : max(maximum, idleMaximum))
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

    private var subscribers: [Int: Subscriber] = [:]
    private var nextSubscriber = 0
    private var state: LinkState = .idle
    /// 门禁和握手都过了的连接；没连上时为空，排着的消息就丢掉。
    private var transport: (any FrameTransport)?
    /// 这次连上的宿主认得 `ClientMsg.keepAwake`；不认得时不发，见 `write`。
    private var hostKeepsAwake = false
    private var generation: UInt64 = 0
    /// 一个订阅者转出去、还没取走的输出和快照最多这么多字节，再多就先不读，见 `readUntilClosed`。
    private static let backlogLimit = 256 << 10
    /// 上层要不要连接：`start` 置真，`stop` 和不能重试的失败置假。
    private var wanted = false
    private var runTask: Task<Void, Never>?
    private var backoffTask: Task<Void, any Error>?
    private var writerTask: Task<Void, Never>?
    private var requestCounter: UInt32 = 0
    /// 用户在看这台电脑，见 `setFocused`。不经 actor，`AppModel` 在主线程上按导航栈同步设。
    private nonisolated let focused = Mutex(true)

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
        for subscriber in subscribers.values {
            subscriber.continuation.finish()
        }
    }

    // MARK: HostLink

    /// 给出去的流在订阅者取走一帧输出时从它的 `Backlog` 里减掉，读的一方据此知道它积压了多少。
    public func events() -> AsyncStream<HostEvent> {
        let (inner, continuation) = AsyncStream.makeStream(of: HostEvent.self)
        let id = nextSubscriber
        nextSubscriber += 1
        let backlog = Backlog()
        subscribers[id] = Subscriber(continuation: continuation, backlog: backlog)
        continuation.onTermination = { [weak self] _ in
            Task { await self?.unsubscribe(id) }
        }
        continuation.yield(.state(state))
        if transport != nil {
            continuation.yield(.ready(generation: generation))
        }
        let source = EventSource(inner.makeAsyncIterator())
        return AsyncStream {
            let event = await source.next()
            if case .frame(let frame, _)? = event {
                backlog.bytes.withLock { $0 -= frame.payload.count }
            }
            return event
        } onCancel: { [weak self] in
            Task { await self?.unsubscribe(id) }
        }
    }

    public nonisolated func send(_ message: ClientMsg) {
        outbox.yield(.message(message))
    }

    public nonisolated func sendInput(_ data: Data, channel: UInt32, generation: UInt64) {
        outbox.yield(.input(data, channel: channel, generation: generation))
    }

    /// 要连接。上次是不能重试的失败（比如 `unknown_device`：电脑上撤销了这台设备）时不自动再连：
    /// 再试也一样，其中签名不对、口令作废电脑还会计入限速。要用户点「重试」（`reconnectNow`）才再试。
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

    /// 不在看的电脑连不上时最多等 `ReconnectPolicy.idleMaximum` 再试：首页上连着所有配对过的电脑，关着的
    /// 那几台每 15 秒找一遍 Bonjour、挨个试地址太费电。变回在看时，还要等比 `maximum` 久的马上重试。
    public nonisolated func setFocused(_ focused: Bool) {
        let gained = self.focused.withLock { old in
            defer { old = focused }
            return focused && !old
        }
        if gained {
            Task { await self.focusGained() }
        }
    }

    private func focusGained() {
        guard focused.withLock({ $0 }), case .waiting(_, let retryAt) = state,
            Duration.seconds(retryAt.timeIntervalSinceNow) > policy.maximum
        else { return }
        backoffTask?.cancel()
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
        let bytes = if case .frame(let frame, _) = event { frame.payload.count } else { 0 }
        for subscriber in subscribers.values {
            if bytes > 0 { subscriber.backlog.bytes.withLock { $0 += bytes } }
            subscriber.continuation.yield(event)
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
                let connectedAt = ContinuousClock.now
                generation += 1
                transport = session.transport
                hostKeepsAwake = session.keepAwake
                setState(.connected(hostName: session.hostName, address: session.transport.remoteAddress))
                broadcast(.ready(generation: generation))
                reason = await readUntilClosed(session.transport, decoder: session.decoder)
                transport = nil
                session.transport.close()
                // 连了一阵才断的（比如宿主交接给新版本）马上重连，重连失败了从第一档退避起；一连上就断的接着
                // 退避，免得宿主过了门禁就关时来回撞。
                failures = ContinuousClock.now - connectedAt > .seconds(10) ? 0 : failures + 1
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
            let delay = policy.delay(afterFailures: failures, focused: focused.withLock { $0 })
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
        var keepAwake: Bool
    }

    /// Bonjour 找到的地址和记着的地址（上次成功的、二维码里的，按顺序试）同时连，谁先过门禁用谁：组播
    /// 不通的 Wi-Fi 上 Bonjour 要等满超时，记着的地址不跟着等。晚到的那条也连上了就关掉。不能重试的
    /// 失败直接报；都失败时报先到的那个不是「没有地址」的错，有限速的拒绝时报它。
    private func establish() async throws -> Session {
        guard let key = try keyStore.key(for: machine.id) else { throw LinkFailure.missingKey }
        let discovery = discovery
        let fingerprint = machine.fingerprint
        let fallback = machine.fallbackTargets
        return try await withThrowingTaskGroup(of: Session.self) { group in
            group.addTask {
                try await firstSuccess(of: fallback) { target in try await self.handshake(with: target, key: key) }
            }
            group.addTask {
                guard let found = await discovery.locate(fingerprint, timeout: .seconds(2)) else {
                    throw LinkFailure.noAddress
                }
                return try await self.handshake(with: found, key: key)
            }
            var winner: Session?
            var failure: (any Error)?
            var fatal: LinkFailure?
            // 有了结果也把另一条等完：它可能已经连上了，要关掉，不能留着一条没人读的连接。
            while let result = await group.nextResult() {
                switch result {
                case .success(let session) where winner == nil && fatal == nil:
                    winner = session
                    group.cancelAll()
                case .success(let session):
                    session.transport.close()
                case .failure(let error as LinkFailure) where error.isFatal && winner == nil:
                    fatal = fatal ?? error
                    group.cancelAll()
                case .failure(let error):
                    // 限速的拒绝要传上去，`run` 才会等最长的退避。
                    if failure == nil || (failure as? LinkFailure) == .noAddress
                        || (error as? LinkFailure) == .rejected(.rateLimited)
                    {
                        failure = error
                    }
                }
            }
            if let fatal { throw fatal }
            if let winner { return winner }
            throw failure ?? LinkFailure.noAddress
        }
    }

    private func handshake(with target: TransportTarget, key: StoredDeviceKey) async throws -> Session {
        let transport = try await open(target, machine.fingerprint)
        // 握手中途 `stop`：超时任务跟着取消、不会关连接，卡着的读不响应取消，这里关掉连接让它报错。
        return try await withTaskCancellationHandler {
            try await admit(over: transport, key: key)
        } onCancel: {
            transport.close()
        }
    }

    /// 在开好的连接上走门禁、`Hello` 和 `Welcome`。
    private func admit(over transport: any FrameTransport, key: StoredDeviceKey) async throws -> Session {
        do {
            let outcome = try await GateClient.run(
                over: transport, credential: .auth(deviceId: machine.deviceId, key: key))
            transport.send(try Frame.control(identity.hello).encoded())
            let (decoder, keepAwake) = try await withTimeout(GateClient.timeout, onTimeout: { transport.close() }) {
                var reader = FrameReader(transport: transport, decoder: outcome.decoder)
                while true {
                    let payload = try await reader.nextControl()
                    switch try JSONDecoder().decode(HostMsg.self, from: payload) {
                    case .welcome(let keepAwake): return (reader.decoder, keepAwake)
                    case .incompatible(_, _, let reason): throw LinkFailure.incompatible(reason)
                    case .goodbye(let reason): throw LinkFailure.closed(String(localized: "宿主断开了连接（\(String(describing: reason))）"))
                    default: continue
                    }
                }
            }
            if let address = transport.remoteAddress, address != machine.lastAddress {
                machine.lastAddress = address
                // 只改地址：`machine` 是开连接时拷的，整条写回会盖掉之后改的名字、写回已删的记录。
                try? await machines.updateLastAddress(id: machine.id, address)
            }
            return Session(transport: transport, decoder: decoder, hostName: outcome.hostName, keepAwake: keepAwake)
        } catch {
            transport.close()
            throw error
        }
    }

    /// 读到连接断开，返回断开的原因。
    ///
    /// 订阅者的事件流不限长，读多少就攒多少：手机喂 VT 跟不上时输出全堆在内存里，画面越落越后，宿主
    /// 那边却看不到积压，不会让会话改发 `Resync`。所以有订阅者积压过了 `backlogLimit` 时先不读，等它
    /// 取走，积压留在 TCP 和宿主那边。
    ///
    /// 刷屏时宿主转来的输出帧很碎（一帧常常只有一两行），订阅者每取一帧都有固定开销；所以一次读到的
    /// 帧里，同一通道上连着的输出（或快照）先拼成一帧再交出去。
    private func readUntilClosed(_ transport: any FrameTransport, decoder: FrameDecoder) async -> String {
        var decoder = decoder
        var goodbye: GoodbyeReason?
        do {
            while true {
                var held: Frame?
                while let frame = try decoder.next() {
                    if frame.kind != .control, frame.kind == held?.kind, frame.channel == held?.channel {
                        held?.payload.append(frame.payload)
                        continue
                    }
                    if let previous = held { handle(previous) }
                    held = nil
                    if frame.kind == .output || frame.kind == .snapshot {
                        held = frame
                    } else if case .goodbye(let reason)? = handle(frame) {
                        goodbye = reason
                    }
                }
                if let previous = held { handle(previous) }
                // shortcut: 积压时每 5 毫秒看一次，不等订阅者通知；积压本来就要几十毫秒才消化得掉。
                while subscribers.values.contains(where: { $0.backlog.bytes.withLock { $0 } > Self.backlogLimit }),
                    !Task.isCancelled
                {
                    try? await Task.sleep(for: .milliseconds(5))
                }
                guard let chunk = try await transport.receive() else {
                    try decoder.finish()
                    break
                }
                decoder.append(chunk)
            }
        } catch let error as FrameError {
            return String(localized: "收到的数据格式不对（\(String(describing: error))）")
        } catch {
            if Task.isCancelled { return String(localized: "已断开") }
            return (error as? LinkFailure)?.errorDescription ?? error.localizedDescription
        }
        switch goodbye {
        case .handoff?: return String(localized: "电脑上的 Runode 升级了，正在重新连接")
        case .shutdown?: return String(localized: "电脑上的 Runode 退出了")
        case .error(let message)?: return String(localized: "电脑断开了连接：\(message)")
        default: return String(localized: "电脑关闭了连接")
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
            // 旧宿主回的 `Error` 不带编号，会被等着回话的页面当成自己的。
            if message == .keepAwake, !hostKeepsAwake { return }
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

/// 一个订阅者转出去、还没取走的输出和快照字节数。
private final class Backlog: Sendable {
    let bytes = Mutex(0)
}

private struct Subscriber {
    let continuation: AsyncStream<HostEvent>.Continuation
    let backlog: Backlog
}

/// `events` 给出去的流从里层的流取事件：只有订阅者那一个消费方按先后调 `next`，不会同时取。
private final class EventSource: @unchecked Sendable {
    private var iterator: AsyncStream<HostEvent>.Iterator

    init(_ iterator: AsyncStream<HostEvent>.Iterator) {
        self.iterator = iterator
    }

    func next() async -> HostEvent? {
        await iterator.next()
    }
}
