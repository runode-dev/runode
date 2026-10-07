import Foundation
import Observation
import RunodeConnection
import RunodeProtocol

/// 会话的 agent 状态分成几类：等用户回答的、在干活的、其他。首页按它汇总，卡片按它标徽标、带快速回复。
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

/// 列表里的一节：电脑上的 app 里的一个工作区，或者不在任何窗口里的会话。
public struct SessionSection: Hashable, Sendable, Identifiable {
    public enum ID: Hashable, Sendable {
        /// 第 `window` 个窗口里的第 `index` 个工作区，序号见 `WindowLayout`。
        case workspace(window: UInt32, index: UInt32)
        /// 没有窗口在显示的会话（电脑上的 app 没开着时是所有会话）。
        case background
    }

    public var id: ID
    /// 工作区的名字；后台那一节为空。
    public var name: String?
    /// 工作区的目录。
    public var dir: String?
    /// 开着不止一个窗口时，工作区在第几个窗口。
    public var window: UInt32?
    /// 工作区里按标签、分屏的先后；后台那一节按宿主给的先后。
    public var sessions: [SessionInfo]
    /// 在这个工作区里新开终端时挨着的会话，见 `WorkspaceLayout.anchor`；后台那一节为空。
    public var anchor: SessionId?
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
/// 标题、目录和 agent 状态；用 `Layout` 问电脑上的 app 各个会话在哪个工作区，按工作区分节；用
/// `ReadScreen` 读屏幕底部几行做预览；等回答的会话能直接快速回复（`SendKeys`、`Paste`，不用连上会话）；
/// 能在某个工作区里新开终端、新建工作区（`DirectoryPickerModel` 选目录）、结束会话。
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
    /// `Layout` 的请求编号固定用 0：`HostLink.nextRequestId` 不发 0，回话不会和别的请求混；几次的回话
    /// 都是完整的布局，后到的盖过先到的，也就不用一个个对上。
    static let layoutRequest: UInt32 = 0

    /// 改名以后 `AppModel` 换上新的记录。
    public internal(set) var machine: MachineRecord
    public private(set) var linkState: LinkState = .idle
    public private(set) var sessions: [SessionInfo] = []
    /// 电脑上的 app 的窗口，按窗口的序号；app 没开着窗口时为空。
    public private(set) var windows: [WindowLayout] = []
    /// 每个会话屏幕底部的几行，去掉了空行。
    public private(set) var previews: [SessionId: [String]] = [:]
    /// 连上后收到过一次列表。
    public private(set) var loaded = false
    /// 这台电脑上终端的主题：只看状态的 `Attach` 回话里带着，终端页开着时它收到的 `ThemeApplied`
    /// 也经同一条连接到这里。没收到过时为空。
    public private(set) var theme: AppTheme?
    public private(set) var isSpawning = false
    public var errorMessage: String?
    /// 等用户确认结束的会话。
    public var killTarget: SessionId?
    /// 各个会话目录里能跑的项目命令（Makefile 的目标、package.json 的 scripts），按目录；宿主回过话的
    /// 目录才有，列不出来的目录是空的。
    public private(set) var projectTasks: [String: [TaskSource]] = [:]
    /// 新建工作区时选目录用的，选着的时候才有。
    public private(set) var directoryPicker: DirectoryPickerModel?
    /// 新开会话用的网格尺寸，视图按手机屏幕算好后设进来。
    public var spawnSize = GridSize(cols: 80, rows: 24, cellWidthPx: 16, cellHeightPx: 32)

    @ObservationIgnored public let link: any HostLink
    /// 新开的会话开好了（宿主回了 `Opened` 或 `Spawned`），`AppModel` 据此打开它的终端页。
    @ObservationIgnored public var onSpawned: @MainActor (SessionId) -> Void = { _ in }
    /// `theme` 变了，`AppModel` 据此记下 App 现在用的主题。
    @ObservationIgnored var onThemeChanged: @MainActor () -> Void = {}
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var connected = false
    /// 这次连接上已经发过只看状态的 `Attach` 的会话。
    @ObservationIgnored private var watching: Set<SessionId> = []
    /// 终端页开着的会话。
    @ObservationIgnored private var screens: Set<SessionId> = []
    /// 在等回话的新开终端、新建工作区的请求。
    @ObservationIgnored private var pendingSpawn: PendingSpawn?
    /// 对连接的开、停按调用的先后做：后台、前台来回切得快时不会先开后停。
    @ObservationIgnored private var linkControl: Task<Void, Never>?
    @ObservationIgnored private var throttle: PreviewThrottle
    @ObservationIgnored private let now: () -> ContinuousClock.Instant
    @ObservationIgnored private var quickReplies: [SessionId: QuickReplyModel] = [:]
    /// 每个会话在等的那次读屏幕是哪种，回话按会话对上（`ScreenText` 不带请求编号，节流保证同一个
    /// 会话同时只有一次在等）。
    @ObservationIgnored private var pendingPreviews: [SessionId: PreviewRead] = [:]
    /// 在等回话的列项目命令的请求，按请求编号记着列的目录。
    @ObservationIgnored private var pendingProjectTasks: [UInt32: String] = [:]
    /// 正在列的目录：取请求编号之前就记上，同一个目录不会同时要两次。
    @ObservationIgnored private var listingDirs: Set<String> = []
    /// 电脑上的 runode 太旧，不认识 `ListProjectTasks`：这次连着时不再要。
    @ObservationIgnored private var projectTasksUnsupported = false

    /// 在等回话的新开请求。
    private struct PendingSpawn {
        var req: UInt32
        /// 发着 `Open` 时预先取好的、退回 `Spawn` 用的编号；新建工作区、已经退回去了时为空。
        var fallback: UInt32?
        /// 没开成时提示的前半句。
        var failure: String
    }

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

    /// 按电脑上的工作区分好节的会话：窗口按序号，工作区按侧栏里的先后，最后是不在任何窗口里的会话。
    /// 没有已知会话的工作区不出现（刚开的会话列表里还没有时，等下一次列表）。
    public var sections: [SessionSection] {
        let byId = Dictionary(sessions.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        var placed: Set<SessionId> = []
        var sections: [SessionSection] = []
        let windows = windows.sorted { $0.index < $1.index }
        for window in windows {
            for workspace in window.workspaces {
                let members = workspace.sessions.compactMap { id -> SessionInfo? in
                    guard !placed.contains(id), let session = byId[id] else { return nil }
                    placed.insert(id)
                    return session
                }
                guard !members.isEmpty else { continue }
                sections.append(
                    SessionSection(
                        id: .workspace(window: window.index, index: workspace.index), name: workspace.name,
                        dir: workspace.dir, window: windows.count > 1 ? window.index : nil, sessions: members,
                        anchor: workspace.anchor))
            }
        }
        let rest = sessions.filter { !placed.contains($0.id) }
        if !rest.isEmpty {
            sections.append(SessionSection(id: .background, sessions: rest))
        }
        return sections
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
        link.send(.layout(req: Self.layoutRequest))
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

    /// 新开一个会话，开好后调 `onSpawned`。先请电脑上的 app 开一个新标签（`Open`），电脑上也看得到、
    /// 接着用：`near` 是挨着哪个会话（某个工作区的 `anchor`），为空时开在最前面那个窗口当前的工作区里。
    /// app 没开着窗口（宿主回 `Error`）时退回 `Spawn`，开一个只在后台跑的会话。
    public func spawn(near: SessionId? = nil) async {
        guard connected, !isSpawning else { return }
        isSpawning = true
        let req = await link.nextRequestId()
        let fallback = await link.nextRequestId()
        pendingSpawn = PendingSpawn(req: req, fallback: fallback, failure: "开不了新终端")
        link.send(.open(req: req, placement: .tab, near: near, cwd: nil, focus: false))
    }

    /// 开始新建工作区：从家目录开始选目录。
    public func beginNewWorkspace() {
        guard connected else { return }
        let picker = DirectoryPickerModel(link: link)
        directoryPicker = picker
        Task { await picker.load(nil) }
    }

    public func cancelNewWorkspace() {
        directoryPicker = nil
    }

    /// 在电脑上的 app 里新建目录是 `dir` 的工作区（`OpenWorkspace`），开好后调 `onSpawned` 打开它的
    /// 第一个终端。电脑上不切过去，不打断用户手上的事。app 没开着窗口时没有工作区可建，在 `dir` 里开一个
    /// 只在后台跑的会话（`Spawn`）。开着窗口时建不成就提示，不退回 `Spawn`：多半是目录有问题，后台会话
    /// 也开不成。
    public func createWorkspace(at dir: String) async {
        directoryPicker = nil
        guard connected, !isSpawning else { return }
        isSpawning = true
        let req = await link.nextRequestId()
        if hasDesktopWindow {
            pendingSpawn = PendingSpawn(req: req, fallback: nil, failure: "建不了工作区")
            link.send(.openWorkspace(req: req, dir: dir, focus: false))
        } else {
            pendingSpawn = PendingSpawn(req: req, fallback: nil, failure: "开不了新终端")
            link.send(.spawn(req: req, size: spawnSize, cwd: dir, integration: .detect, start: true))
        }
    }

    /// 电脑上的 app 开着窗口，能在里面建工作区。
    public var hasDesktopWindow: Bool {
        !windows.isEmpty
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

    // MARK: 项目命令

    /// 这个会话目录里能跑的项目命令，还没列过或者目录不知道时为空。
    public func projectTasks(for session: SessionInfo) -> [TaskSource] {
        session.meta.cwd.flatMap { projectTasks[$0] } ?? []
    }

    /// 能在这个会话里跑项目命令：shell 停在提示符上，打进去的命令不会落进别的程序。
    public func canRunProjectTask(in session: SessionInfo) -> Bool {
        connected && session.meta.foregroundIsShell && !session.exited
    }

    /// 在会话 `id` 里跑一条项目命令：像快速回复一样粘贴进去再回车。shell 不在提示符上时不发，返回假。
    @discardableResult
    public func runProjectTask(_ task: ProjectTask, in id: SessionId) async -> Bool {
        guard let session = session(id), canRunProjectTask(in: session) else { return false }
        let paste = await link.nextRequestId()
        let enter = await link.nextRequestId()
        link.send(.paste(req: paste, id: id, text: task.command))
        link.send(.sendKeys(req: enter, id: id, keys: ["enter"]))
        return true
    }

    /// 列 `dir` 里的项目命令：卡片出现、会话换了目录时要一次，列过的不再要；`refreshing` 为真时（下拉
    /// 刷新，Makefile、package.json 可能改过了）列过的也再要。同一个目录同时只等一次回话。
    public func loadProjectTasks(in dir: String, refreshing: Bool = false) async {
        guard connected, !projectTasksUnsupported, !listingDirs.contains(dir),
            refreshing || projectTasks[dir] == nil
        else { return }
        listingDirs.insert(dir)
        let req = await link.nextRequestId()
        pendingProjectTasks[req] = dir
        link.send(.listProjectTasks(req: req, dir: dir))
    }

    /// 下拉刷新：重新列一遍各个会话目录里的项目命令。
    public func refreshProjectTasks() async {
        for dir in Set(sessions.compactMap(\.meta.cwd)).sorted() {
            await loadProjectTasks(in: dir, refreshing: true)
        }
    }

    /// 收到列项目命令的回话（或者它的 `Error`）：记下结果，列不出来的目录记成空的，不再要。
    private func finishProjectTasks(_ req: UInt32, sources: [TaskSource]) {
        guard let dir = pendingProjectTasks.removeValue(forKey: req) else { return }
        listingDirs.remove(dir)
        projectTasks[dir] = sources
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
                pendingProjectTasks = [:]
                listingDirs = []
                projectTasksUnsupported = false
                for reply in quickReplies.values {
                    reply.connectionLost()
                }
                if case .failed = state { isSpawning = false }
            }
        case .ready:
            connected = true
            watching = []
            refresh()
        case .message(let message):
            handle(message)
        case .frame:
            break
        }
    }

    private func setTheme(_ newTheme: AppTheme) {
        guard theme != newTheme else { return }
        theme = newTheme
        onThemeChanged()
    }

    private func handle(_ message: HostMsg) {
        if quickReplies.values.contains(where: { $0.handle(message) }) || directoryPicker?.handle(message) == true {
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
            if let settings = attached.settings { setTheme(AppTheme(settings)) }
        case .themeApplied(_, let settings):
            setTheme(AppTheme(settings))
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
            update(id) { $0.sizeOwner = mine ? "本机" : owner }
        case .layout(Self.layoutRequest, let windows):
            self.windows = windows
        case .projectTasks(let req, _, let sources):
            finishProjectTasks(req, sources: sources)
        case .opened(let req, let id) where req == pendingSpawn?.req,
            .spawned(let req, let id) where req == pendingSpawn?.req:
            pendingSpawn = nil
            isSpawning = false
            refresh()
            onSpawned(id)
        case .error(let req, let id, let message):
            if let req, var pending = pendingSpawn, req == pending.req {
                if let fallback = pending.fallback {
                    // 电脑上的 app 没开着窗口：开一个只在后台跑的会话。
                    pending.req = fallback
                    pending.fallback = nil
                    pendingSpawn = pending
                    link.send(.spawn(req: fallback, size: spawnSize, cwd: nil, integration: .detect, start: true))
                } else {
                    pendingSpawn = nil
                    isSpawning = false
                    errorMessage = "\(pending.failure)：\(message)"
                }
            } else if let req, pendingProjectTasks[req] != nil {
                finishProjectTasks(req, sources: [])
            } else if req == Self.layoutRequest {
                // 电脑上没有 app 的界面连着宿主：所有会话都在后台。
                windows = []
            } else if req == nil, message == HostMsg.unknownMessage, let pending = pendingSpawn,
                pending.fallback == nil
            {
                // 电脑上的 runode 太旧，不认识 `OpenWorkspace`，回的 `Error` 不带编号。
                pendingSpawn = nil
                isSpawning = false
                errorMessage = "\(pending.failure)：电脑上的 runode 版本太旧，先升级它。"
            } else if req == nil, message == HostMsg.unknownMessage, !pendingProjectTasks.isEmpty {
                // 电脑上的 runode 太旧，不认识 `ListProjectTasks`：不列了，卡片上不出现项目命令。
                pendingProjectTasks = [:]
                listingDirs = []
                projectTasksUnsupported = true
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

    private func update(_ id: SessionId, _ change: (inout SessionInfo) -> Void) {
        guard let index = sessions.firstIndex(where: { $0.id == id }) else { return }
        change(&sessions[index])
    }
}
