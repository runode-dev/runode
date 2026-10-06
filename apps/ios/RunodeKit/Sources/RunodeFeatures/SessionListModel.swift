import Foundation
import Observation
import RunodeConnection
import RunodeProtocol

/// 一台 Mac 的会话列表：连上后 `ListSessions`，给每个会话发只看状态（`MetaOnly`）的 `Attach` 收实时的
/// 标题、目录和 agent 状态；能新开、结束会话。
///
/// 一条连接上一个会话只能有一个订阅，后来的 `Attach` 换掉先前的。所以终端页开着的会话（`screens`）
/// 列表不再给它发只看状态的 `Attach`，终端页关掉时再改回只看状态。
@Observable
@MainActor
public final class SessionListModel {
    public let machine: MachineRecord
    public private(set) var linkState: LinkState = .idle
    public private(set) var sessions: [SessionInfo] = []
    /// 连上后收到过一次列表。
    public private(set) var loaded = false
    public private(set) var isSpawning = false
    /// 新开好的会话，视图据此打开它的终端页，打开后清掉。
    public var spawnedSession: SessionId?
    public var errorMessage: String?
    /// 等用户确认结束的会话。
    public var killTarget: SessionId?
    /// 新开会话用的网格尺寸，视图按手机屏幕算好后设进来。
    public var spawnSize = GridSize(cols: 80, rows: 24, cellWidthPx: 16, cellHeightPx: 32)

    @ObservationIgnored public let link: any HostLink
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var connected = false
    /// 这次连接上已经发过只看状态的 `Attach` 的会话。
    @ObservationIgnored private var watching: Set<SessionId> = []
    /// 终端页开着的会话。
    @ObservationIgnored private var screens: Set<SessionId> = []
    @ObservationIgnored private var pendingSpawn: UInt32?
    /// 对连接的开、停按调用的先后做：后台、前台来回切得快时不会先开后停。
    @ObservationIgnored private var linkControl: Task<Void, Never>?

    public init(machine: MachineRecord, link: any HostLink) {
        self.machine = machine
        self.link = link
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

    /// 离开这台 Mac 或者 App 进了后台：停止订阅，断开连接。
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

    /// 看着列表的时候每隔几秒重新要一次：别处新开的会话宿主不会主动告诉这条连接。
    public func keepRefreshing(every interval: Duration = .seconds(5)) async {
        while !Task.isCancelled {
            try? await Task.sleep(for: interval)
            refresh()
        }
    }

    public func reconnect() {
        let link = self.link
        Task { await link.reconnectNow() }
    }

    /// 新开一个会话；宿主回 `Spawned` 后 `spawnedSession` 被设上。
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
    }

    public func session(_ id: SessionId) -> SessionInfo? {
        sessions.first { $0.id == id }
    }

    // MARK: 事件

    func handle(_ event: HostEvent) {
        switch event {
        case .state(let state):
            linkState = state
            if !state.isConnected {
                connected = false
                watching = []
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
        switch message {
        case .sessionList(let list):
            sessions = list
            loaded = true
            for session in list where !screens.contains(session.id) && !watching.contains(session.id) {
                watching.insert(session.id)
                link.send(.attach(id: session.id, size: nil, mode: .metaOnly))
            }
        case .attached(let attached):
            update(attached.id) { $0.meta = attached.meta }
        case .meta(let id, let meta):
            update(id) { $0.meta = meta }
        case .exited(let id, _):
            update(id) { $0.exited = true }
        case .sizeOwner(let id, let mine, let owner):
            update(id) { $0.sizeOwner = mine ? machineLocalName : owner }
        case .spawned(let req, let id) where req == pendingSpawn:
            pendingSpawn = nil
            isSpawning = false
            spawnedSession = id
            link.send(.listSessions)
        case .error(let req, let id, let message):
            if let req, req == pendingSpawn {
                pendingSpawn = nil
                isSpawning = false
                errorMessage = "开不了新终端：\(message)"
            } else if let id, watching.contains(id) {
                // 只看状态的订阅说没有这个会话：别处已经结束了它。
                watching.remove(id)
                sessions.removeAll { $0.id == id }
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
