import Foundation
import Observation
import RunodeConnection
import RunodeProtocol

/// 会话列表里的分组：等用户回答的放最上面，接着是在干活的，最后是其他。
public enum SessionGroup: Hashable, Sendable, CaseIterable {
    /// agent 停下来等回答（`AgentState::Blocked`）。
    case waiting
    /// agent 在干活。
    case working
    case other

    public static func of(_ session: SessionInfo) -> SessionGroup {
        guard !session.exited, let agent = session.meta.agent else { return .other }
        switch agent.state {
        case .blocked: return .waiting
        case .working: return .working
        default: return .other
        }
    }
}

/// 列表里的一组会话，组内按宿主给的先后。
public struct SessionSection: Hashable, Sendable, Identifiable {
    public var group: SessionGroup
    public var sessions: [SessionInfo]
    public var id: SessionGroup { group }
}

/// 每个会话最多多久读一次屏幕做预览。
struct PreviewThrottle {
    enum Decision: Equatable {
        /// 现在就读。
        case now
        /// 过这么久再读（已经替它排好了）。
        case later(Duration)
        /// 已经排着一次了，不用再排。
        case skip
    }

    var interval: Duration
    private var last: [SessionId: ContinuousClock.Instant] = [:]
    private var scheduled: Set<SessionId> = []

    init(interval: Duration) {
        self.interval = interval
    }

    mutating func request(_ id: SessionId, at now: ContinuousClock.Instant) -> Decision {
        if scheduled.contains(id) { return .skip }
        if let last = last[id], now - last < interval {
            scheduled.insert(id)
            return .later(interval - (now - last))
        }
        last[id] = now
        return .now
    }

    /// 排着的那次到点了。
    mutating func fire(_ id: SessionId, at now: ContinuousClock.Instant) {
        scheduled.remove(id)
        last[id] = now
    }

    mutating func reset() {
        last = [:]
        scheduled = []
    }
}

/// 一台电脑的会话列表：连上后 `ListSessions`，给每个会话发只看状态（`MetaOnly`）的 `Attach` 收实时的
/// 标题、目录和 agent 状态；用 `ReadScreen` 读屏幕底部几行做预览；等回答的会话能直接快速回复
/// （`SendKeys`、`Paste`，不用连上会话）；能新开、结束会话。
///
/// 一条连接上一个会话只能有一个订阅，后来的 `Attach` 换掉先前的。所以终端页开着的会话（`screens`）
/// 列表不再给它发只看状态的 `Attach`，终端页关掉时再改回只看状态。
@Observable
@MainActor
public final class SessionListModel {
    /// 预览读多少行：读回来以后去掉空行再取最后几行。
    static let previewReadLines: UInt32 = 12
    /// 预览显示几行。
    public static let previewLines = 3

    /// 改名以后 `AppModel` 换上新的记录。
    public internal(set) var machine: MachineRecord
    public private(set) var linkState: LinkState = .idle
    public private(set) var sessions: [SessionInfo] = []
    /// 每个会话屏幕底部的几行，去掉了空行。
    public private(set) var previews: [SessionId: [String]] = [:]
    /// 连上后收到过一次列表。
    public private(set) var loaded = false
    public private(set) var isSpawning = false
    public var errorMessage: String?
    /// 等用户确认结束的会话。
    public var killTarget: SessionId?
    /// 新开会话用的网格尺寸，视图按手机屏幕算好后设进来。
    public var spawnSize = GridSize(cols: 80, rows: 24, cellWidthPx: 16, cellHeightPx: 32)

    @ObservationIgnored public let link: any HostLink
    /// 新开的会话开好了（宿主回了 `Spawned`），`AppModel` 据此打开它的终端页。
    @ObservationIgnored public var onSpawned: @MainActor (SessionId) -> Void = { _ in }
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var connected = false
    /// 这次连接上已经发过只看状态的 `Attach` 的会话。
    @ObservationIgnored private var watching: Set<SessionId> = []
    /// 终端页开着的会话。
    @ObservationIgnored private var screens: Set<SessionId> = []
    @ObservationIgnored private var pendingSpawn: UInt32?
    /// 对连接的开、停按调用的先后做：后台、前台来回切得快时不会先开后停。
    @ObservationIgnored private var linkControl: Task<Void, Never>?
    @ObservationIgnored private var throttle: PreviewThrottle
    @ObservationIgnored private let now: () -> ContinuousClock.Instant
    @ObservationIgnored private var quickReplies: [SessionId: QuickReplyModel] = [:]
    /// 每个会话在等的那次读屏幕是哪种，回话按会话对上（`ScreenText` 不带请求编号，节流保证同一个
    /// 会话同时只有一次在等）。
    @ObservationIgnored private var pendingPreviews: [SessionId: PreviewRead] = [:]

    /// 预览怎么读屏幕。
    enum PreviewRead: Hashable {
        /// shell 在提示符上等输入：读最近一条命令的输出（`command: 1`），宿主按 shell 集成标出的提示符
        /// 切，完全不含提示符。
        case lastCommand
        /// 读屏幕底部几行；`atPrompt` 为真时去掉最后一个有字的行（光标所在的提示符）。没有 shell 集成、
        /// 上一条命令没有输出时的回退，也是 agent 在跑、前台不是 shell 时的读法。
        case bottomLines(atPrompt: Bool)
    }

    public init(
        machine: MachineRecord, link: any HostLink, previewInterval: Duration = .seconds(1),
        now: @escaping () -> ContinuousClock.Instant = { .now }
    ) {
        self.machine = machine
        self.link = link
        self.throttle = PreviewThrottle(interval: previewInterval)
        self.now = now
    }

    /// 分好组的会话：等回答、干活中、其他，空的组不出现。
    public var sections: [SessionSection] {
        SessionGroup.allCases.compactMap { group in
            let members = sessions.filter { SessionGroup.of($0) == group }
            return members.isEmpty ? nil : SessionSection(group: group, sessions: members)
        }
    }

    /// 订阅连接的事件并要求连接。
    public func start() {
        guard task == nil else { return }
        let link = self.link
        let previous = linkControl
        let subscribed = Task {
            await previous?.value
            let events = await link.events()
            await link.start()
            return events
        }
        linkControl = Task { _ = await subscribed.value }
        task = Task { [weak self] in
            for await event in await subscribed.value {
                guard let self else { return }
                self.handle(event)
            }
        }
    }

    /// 离开这台电脑或者 App 进了后台：停止订阅，断开连接。
    public func stop() {
        task?.cancel()
        task = nil
        let link = self.link
        let previous = linkControl
        linkControl = Task {
            await previous?.value
            await link.stop()
        }
    }

    public func refresh() {
        guard connected else { return }
        link.send(.listSessions)
    }

    /// 看着列表的时候每隔几秒重新要一次列表：别处新开的会话宿主不会主动告诉这条连接。在干活、在等
    /// 回答的会话顺带刷新预览，它们的屏幕变得快，状态却不一定变。
    public func keepRefreshing(every interval: Duration = .seconds(5)) async {
        refreshPreviews()
        while !Task.isCancelled {
            try? await Task.sleep(for: interval)
            refresh()
            for session in sessions where SessionGroup.of(session) != .other {
                requestPreview(session.id)
            }
        }
    }

    /// 进入列表时：所有会话的预览都刷新一次。
    public func refreshPreviews() {
        for session in sessions {
            requestPreview(session.id)
        }
    }

    public func reconnect() {
        let link = self.link
        Task { await link.reconnectNow() }
    }

    /// 新开一个会话；宿主回 `Spawned` 后调 `onSpawned`。
    public func spawn() async {
        guard connected, !isSpawning else { return }
        isSpawning = true
        let req = await link.nextRequestId()
        pendingSpawn = req
        link.send(.spawn(req: req, size: spawnSize, cwd: nil, integration: .detect, start: true))
    }

    public var isConfirmingKill: Bool {
        get { killTarget != nil }
        set { if !newValue { killTarget = nil } }
    }

    /// 结束会话（视图先让用户确认）。
    public func kill(_ id: SessionId) {
        link.send(.kill(id: id))
        sessions.removeAll { $0.id == id }
        watching.remove(id)
        previews[id] = nil
        quickReplies[id] = nil
    }

    /// 这个会话的快速回复，第一次要时建好。送到以后刷新它的预览。
    public func quickReply(for id: SessionId) -> QuickReplyModel {
        if let existing = quickReplies[id] { return existing }
        let model = QuickReplyModel(sessionId: id, link: link) { [weak self] id in
            self?.requestPreview(id)
        }
        quickReplies[id] = model
        return model
    }

    /// 终端页开始看这个会话。
    public func screenOpened(_ id: SessionId) {
        screens.insert(id)
        watching.remove(id)
    }

    /// 终端页不看了：改回只看状态，不放手这个会话的状态更新，也让出尺寸。
    public func screenClosed(_ id: SessionId) {
        screens.remove(id)
        guard connected, sessions.contains(where: { $0.id == id }) else { return }
        watching.insert(id)
        link.send(.attach(id: id, size: nil, mode: .metaOnly))
        requestPreview(id)
    }

    public func session(_ id: SessionId) -> SessionInfo? {
        sessions.first { $0.id == id }
    }

    // MARK: 预览

    /// 刷新一个会话的预览：每个会话最多每 `previewInterval` 读一次屏幕，来得太勤的合并成到点时的一次。
    func requestPreview(_ id: SessionId) {
        guard connected, sessions.contains(where: { $0.id == id }) else { return }
        switch throttle.request(id, at: now()) {
        case .now:
            sendPreviewRead(id)
        case .later(let delay):
            Task { [weak self] in
                try? await Task.sleep(for: delay)
                guard let self else { return }
                self.throttle.fire(id, at: self.now())
                self.sendPreviewRead(id)
            }
        case .skip:
            break
        }
    }

    /// 按会话现在的样子选读法发出去：shell 空闲时读最近一条命令的输出，否则读屏幕底部。
    private func sendPreviewRead(_ id: SessionId) {
        guard connected, let session = session(id) else { return }
        if session.meta.foregroundIsShell && !session.exited {
            send(.lastCommand, for: id)
        } else {
            send(.bottomLines(atPrompt: false), for: id)
        }
    }

    private func send(_ read: PreviewRead, for id: SessionId) {
        pendingPreviews[id] = read
        switch read {
        case .lastCommand:
            link.send(.readScreen(id: id, lines: nil, command: 1))
        case .bottomLines:
            link.send(.readScreen(id: id, lines: Self.previewReadLines))
        }
    }

    /// 读最近一条命令的输出没成（没有 shell 集成、还没跑过命令、上一条命令没有输出）：马上改读屏幕
    /// 底部，去掉提示符那一行。这是同一次刷新的后半，不另算节流。
    private func fallBackToBottomLines(_ id: SessionId) {
        send(.bottomLines(atPrompt: true), for: id)
    }

    // MARK: 事件

    func handle(_ event: HostEvent) {
        switch event {
        case .state(let state):
            linkState = state
            if !state.isConnected {
                connected = false
                watching = []
                throttle.reset()
                pendingPreviews = [:]
                for reply in quickReplies.values {
                    reply.connectionLost()
                }
                if case .failed = state { isSpawning = false }
            }
        case .ready:
            connected = true
            watching = []
            link.send(.listSessions)
        case .message(let message):
            handle(message)
        case .frame:
            break
        }
    }

    private func handle(_ message: HostMsg) {
        if quickReplies.values.contains(where: { $0.handle(message) }) {
            return
        }
        switch message {
        case .sessionList(let list):
            let known = Set(sessions.map(\.id))
            sessions = list
            loaded = true
            let present = Set(list.map(\.id))
            previews = previews.filter { present.contains($0.key) }
            for session in list where !screens.contains(session.id) && !watching.contains(session.id) {
                watching.insert(session.id)
                link.send(.attach(id: session.id, size: nil, mode: .metaOnly))
            }
            for session in list where !known.contains(session.id) || previews[session.id] == nil {
                requestPreview(session.id)
            }
        case .attached(let attached):
            update(attached.id) { $0.meta = attached.meta }
        case .meta(let id, let meta):
            update(id) { $0.meta = meta }
            requestPreview(id)
        case .bell(let id):
            requestPreview(id)
        case .screenText(let id, let text, _):
            guard session(id) != nil else { return }
            let read = pendingPreviews.removeValue(forKey: id) ?? .bottomLines(atPrompt: false)
            switch read {
            case .lastCommand:
                let lines = Presentation.previewLines(text, limit: Self.previewLines)
                if lines.isEmpty {
                    fallBackToBottomLines(id)
                } else {
                    previews[id] = lines
                }
            case .bottomLines(let atPrompt):
                previews[id] = Presentation.previewLines(text, limit: Self.previewLines, atPrompt: atPrompt)
            }
        case .exited(let id, _):
            update(id) { $0.exited = true }
        case .sizeOwner(let id, let mine, let owner):
            update(id) { $0.sizeOwner = mine ? machineLocalName : owner }
        case .spawned(let req, let id) where req == pendingSpawn:
            pendingSpawn = nil
            isSpawning = false
            link.send(.listSessions)
            onSpawned(id)
        case .error(let req, let id, let message):
            if let req, req == pendingSpawn {
                pendingSpawn = nil
                isSpawning = false
                errorMessage = "开不了新终端：\(message)"
            } else if let id, message.hasPrefix("no session") {
                // 宿主说没有这个会话：别处已经结束了它。读屏幕超时这类错误不算。
                watching.remove(id)
                sessions.removeAll { $0.id == id }
                previews[id] = nil
                pendingPreviews[id] = nil
            } else if let id, req == nil, let read = pendingPreviews.removeValue(forKey: id), read == .lastCommand {
                // 读最近一条命令的输出出错（多半是没有 shell 集成）：退回读屏幕底部。
                fallBackToBottomLines(id)
            }
        default:
            break
        }
    }

    /// 尺寸归这部手机时列表里显示的名字。
    private var machineLocalName: String { "本机" }

    private func update(_ id: SessionId, _ change: (inout SessionInfo) -> Void) {
        guard let index = sessions.firstIndex(where: { $0.id == id }) else { return }
        change(&sessions[index])
    }
}
