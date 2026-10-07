import Foundation
import Observation
import RunodeActivity
import RunodeConnection
import RunodeProtocol

/// 导航栈里的一页。
public enum Route: Hashable, Sendable {
    /// 一台电脑的会话列表。
    case machine(UUID)
    /// 一个会话的终端页。
    case terminal(machine: UUID, session: SessionId)
    /// 一个会话所在仓库的 Git 页。
    case git(machine: UUID, session: SessionId)

    /// 这一页属于哪台电脑。
    public var machine: UUID {
        switch self {
        case .machine(let machine), .terminal(let machine, _), .git(let machine, _): machine
        }
    }
}

/// App 用到的外部依赖，由 App 入口组装，测试换成假的。
public struct AppDependencies {
    public var store: any MachineStore
    public var keyStore: any DeviceKeyStore
    public var pairing: any Pairing
    /// 给一台电脑建连接。
    public var makeLink: @MainActor (MachineRecord) -> any HostLink
    /// 系统给的设备名；用户在设置里起了名字时报那个。
    public var deviceName: String
    /// 上次打开的终端记在哪里。这三份不给时都记在临时的 `UserDefaults` 里（测试、演示模式）。
    public var recents: DefaultsStore<RecentTerminal>
    /// 设置存在哪里。
    public var preferences: DefaultsStore<AppPreferences>
    /// 上次用的主题记在哪里，App 刚启动、电脑还没连上时先用它，用浅色主题的不会先闪一下默认的深色。
    public var themes: DefaultsStore<AppTheme>
    /// 灵动岛和锁屏上 Live Activity 的系统接口；默认为空，什么都不显示（测试、演示模式）。
    public var agentActivity: (any AgentActivityDriver)?
    /// 进后台以后多要一会儿运行时间的系统接口；默认要不到，一进后台就断开（测试、演示模式）。
    public var backgroundTime: any BackgroundTime

    @MainActor
    public init(
        store: any MachineStore, keyStore: any DeviceKeyStore, pairing: any Pairing,
        makeLink: @escaping @MainActor (MachineRecord) -> any HostLink, deviceName: String,
        recents: DefaultsStore<RecentTerminal> = DefaultsStore("recentTerminal", defaults: .temporary()),
        preferences: DefaultsStore<AppPreferences> = DefaultsStore("preferences", defaults: .temporary()),
        themes: DefaultsStore<AppTheme> = DefaultsStore("theme", defaults: .temporary())
    ) {
        self.store = store
        self.keyStore = keyStore
        self.pairing = pairing
        self.makeLink = makeLink
        self.deviceName = deviceName
        self.recents = recents
        self.preferences = preferences
        self.themes = themes
        agentActivity = nil
        backgroundTime = NoBackgroundTime()
    }
}

/// 整个 App 的状态：导航栈、各页的视图模型、每台电脑一条连接。配对过的每台电脑在 App 处于前台
/// 时都连着，首页据此显示各台的状态、等回答的会话和统计；App 进后台后向系统多要一会儿时间
/// （`BackgroundTime`），这段时间里连接照常、灵动岛上的 agent 状态照常更新，时间到了全部断开，
/// 回到前台再连上、各页重新 `Attach`。灵动岛和锁屏上的 agent 状态由 `AgentActivityModel` 管。
@Observable
@MainActor
public final class AppModel {
    public var path: [Route] = [] {
        didSet { pathChanged() }
    }
    /// 配对页开着时的视图模型。
    public var pairing: PairingModel?
    /// 设置页开着。
    public var showingSettings = false
    /// 在设置页里点了「配对新电脑」：设置页关掉以后再打开配对页，两个页面不叠着弹。
    public var pairsAfterSettings = false
    public let machineList: MachineListModel
    public let settings: SettingsModel
    /// 上次打开的终端，首页的「上次打开」用它。
    public private(set) var recent: RecentTerminal?
    /// 上次用的主题，电脑还没连上时用它。
    private var savedTheme: AppTheme?

    @ObservationIgnored private let dependencies: AppDependencies
    @ObservationIgnored private var sessionLists: [UUID: SessionListModel] = [:]
    @ObservationIgnored private var terminals: [Route: TerminalModel] = [:]
    @ObservationIgnored private var gits: [Route: GitModel] = [:]
    /// App 在前台。
    @ObservationIgnored private var active = true
    /// 各台电脑的连接开着：在前台，或者进了后台、多要的时间还没用完。
    @ObservationIgnored private var linksRunning = true
    @ObservationIgnored private let agentActivity: AgentActivityModel

    public init(dependencies: AppDependencies) {
        self.dependencies = dependencies
        machineList = MachineListModel(store: dependencies.store, keyStore: dependencies.keyStore)
        let settings = SettingsModel(store: dependencies.preferences, systemDeviceName: dependencies.deviceName)
        self.settings = settings
        agentActivity = AgentActivityModel(
            driver: dependencies.agentActivity, enabled: settings.preferences.showsAgentActivity)
        recent = dependencies.recents.load()
        savedTheme = dependencies.themes.load()
        machineList.willDelete = { [weak self] id in self?.forget(machine: id) }
        machineList.didLoad = { [weak self] in self?.syncConnections() }
        settings.deviceNameDidChange = { [weak self] name in self?.deviceNameChanged(name) }
        settings.agentActivityDidChange = { [weak self] enabled in self?.agentActivity.enabled = enabled }
    }

    /// 打开一个终端页：在这台电脑的会话列表上时压在它上面，别处（首页、别的电脑）打开时连同它的
    /// 会话列表一起换上，返回时先回到列表。
    public func openTerminal(machine: UUID, session: SessionId) {
        let route = Route.terminal(machine: machine, session: session)
        if path.last == .machine(machine) {
            path.append(route)
        } else {
            path = [.machine(machine), route]
        }
    }

    /// 打开一个会话所在仓库的 Git 页，压在当前页上面：从终端页打开时返回回到终端，从会话列表打开时
    /// 回到列表；别的电脑上的页面先换成这台电脑的会话列表。
    public func openGit(machine: UUID, session: SessionId) {
        let route = Route.git(machine: machine, session: session)
        if path.last == route { return }
        if path.first == .machine(machine) {
            path.append(route)
        } else {
            path = [.machine(machine), route]
        }
    }

    public func git(machine machineId: UUID, session: SessionId) -> GitModel? {
        let route = Route.git(machine: machineId, session: session)
        if let existing = gits[route] { return existing }
        guard let list = sessionList(for: machineId) else { return nil }
        let model = GitModel(sessionId: session, link: list.link)
        gits[route] = model
        model.open()
        return model
    }

    /// 打开配对页；`link` 是从别处（比如系统打开的 `runode://pair` 链接）带来的配对链接。
    public func startPairing(link: String? = nil) {
        let model = PairingModel(pairing: dependencies.pairing, deviceName: settings.deviceName) {
            [weak self] machine in
            await self?.machineList.add(machine)
        }
        if let link {
            model.linkText = link
        }
        pairing = model
        if link != nil {
            Task { await model.submitLink() }
        }
    }

    public func sessionList(for machineId: UUID) -> SessionListModel? {
        if let existing = sessionLists[machineId] { return existing }
        guard let machine = machineList.machine(machineId) else { return nil }
        let model = SessionListModel(machine: machine, link: dependencies.makeLink(machine))
        model.onSpawned = { [weak self] session in self?.openTerminal(machine: machineId, session: session) }
        model.onThemeChanged = { [weak self] in self?.saveTheme() }
        model.onSessionsChanged = { [weak self] in self?.syncAgentActivity() }
        sessionLists[machineId] = model
        if linksRunning { model.start() }
        return model
    }

    public func terminal(machine machineId: UUID, session: SessionId) -> TerminalModel? {
        let route = Route.terminal(machine: machineId, session: session)
        if let existing = terminals[route] { return existing }
        guard let list = sessionList(for: machineId) else { return nil }
        let info = list.session(session)
        // 列表里知道这个会话有没有前端在决定尺寸（电脑上的窗口在显示它）；不在列表里时连上再看。
        let hint: SizeOwnerHint =
            switch info {
            case .some(let info): info.sizeOwner == nil ? .none : .someone
            case nil: .unknown
            }
        let model = TerminalModel(
            sessionId: session, title: info?.meta.displayTitle ?? "终端", agent: info?.meta.agent,
            link: list.link, ownerHint: hint, sizePreference: settings.preferences.defaultSize,
            onOpen: { [weak list] id in list?.screenOpened(id) },
            onClose: { [weak list] id in list?.screenClosed(id) })
        terminals[route] = model
        if linksRunning { model.open() }
        return model
    }

    /// 终端页以外的界面用的主题：上次打开的终端所在的那台电脑的，它还没报主题时用配对列表里第一台报了
    /// 的，都没有时用上次记下的；从没连上过电脑时用 runode 默认的深色主题，和终端页没设主题时一样。
    /// 几台电脑主题不同时，进出会话列表也不换主题，免得翻页时整个界面变色。
    public var theme: AppTheme {
        reportedTheme ?? savedTheme ?? AppTheme(.default)
    }

    /// 电脑报来的主题，按 `theme` 说的先后挑；还没有电脑报过时为空。
    private var reportedTheme: AppTheme? {
        let order = (recent.map { [$0.machine] } ?? []) + machineList.machines.map(\.id)
        for id in order {
            if let theme = sessionLists[id]?.theme { return theme }
        }
        return nil
    }

    private func saveTheme() {
        guard let theme = reportedTheme, theme != savedTheme else { return }
        savedTheme = theme
        dependencies.themes.save(theme)
    }

    /// App 回到前台或进了后台。后台里 iOS 会挂起 App，连接迟早被断，所以进后台时向系统多要一会儿时间，
    /// 让灵动岛上的 agent 状态多跟一阵，时间快到了（要不到时马上）主动断开干净；回来时重连。在多要的
    /// 时间里就回来了的，连接一直开着，不用重连。
    public func setActive(_ isActive: Bool) {
        guard isActive != active else { return }
        active = isActive
        agentActivity.setForeground(isActive)
        if isActive {
            dependencies.backgroundTime.end()
            resumeLinks()
        } else {
            let extended = dependencies.backgroundTime.begin { [weak self] in
                self?.suspendLinks()
                await self?.agentActivity.settle()
            }
            if !extended { suspendLinks() }
        }
    }

    /// 断开所有连接，灵动岛上的 agent 状态改成已暂停。时间到期的那一刻 App 已经回到前台了的不断。
    private func suspendLinks() {
        guard linksRunning, !active else { return }
        linksRunning = false
        for list in sessionLists.values {
            list.stop()
        }
        agentActivity.suspend()
    }

    private func resumeLinks() {
        agentActivity.resume()
        guard !linksRunning else { return }
        linksRunning = true
        for list in sessionLists.values {
            list.start()
        }
    }

    /// 按各台电脑现在的会话列表更新灵动岛上的 agent 状态：只算连着的电脑。还有电脑没连上、或者连上了
    /// 还没收到列表时告诉它还没定下来，没有 agent 也先不结束。
    private func syncAgentActivity() {
        let lists = machineList.machines.compactMap { sessionLists[$0.id] }
        let content = AgentActivityContent(
            machines: lists.filter(\.linkState.isConnected).map { ($0.machine.id, $0.machine.name, $0.sessions) })
        let settled = lists.allSatisfy { list in
            switch list.linkState {
            case .idle, .connecting: false
            case .connected: list.loaded
            case .waiting, .failed: true
            }
        }
        agentActivity.refresh(content, settled: settled)
    }

    /// 导航栈变了：退出去的终端页、Git 页关掉，新打开的终端记作「上次打开」。会话列表的连接不跟导航栈走。
    private func pathChanged() {
        let live = Set(path)
        for (route, model) in terminals where !live.contains(route) {
            model.close()
            terminals[route] = nil
        }
        for (route, model) in gits where !live.contains(route) {
            model.close()
            gits[route] = nil
        }
        if case .terminal(let machine, let session)? = path.last {
            remember(machine: machine, session: session)
        }
    }

    /// 配对过的电脑变了（配对、改名、删除之后重新读了列表）：每台都有一条连接，删掉的断开，改了
    /// 名字的换上新记录。
    private func syncConnections() {
        let machines = machineList.machines
        let present = Set(machines.map(\.id))
        for (id, list) in sessionLists where !present.contains(id) {
            list.stop()
            sessionLists[id] = nil
        }
        for machine in machines {
            if let list = sessionLists[machine.id] {
                list.machine = machine
            } else {
                _ = sessionList(for: machine.id)
            }
        }
        syncAgentActivity()
    }

    private func remember(machine: UUID, session: SessionId) {
        let info = sessionLists[machine]?.session(session)
        let previous = recent?.machine == machine && recent?.session == session ? recent : nil
        let entry = RecentTerminal(
            machine: machine, session: session,
            title: info.map(Presentation.sessionTitle) ?? previous?.title ?? "终端",
            directory: info?.meta.cwd ?? previous?.directory)
        guard entry != recent else { return }
        recent = entry
        dependencies.recents.save(entry)
        saveTheme()
    }

    /// 设置页关掉了：在里面点过「配对新电脑」的，这时打开配对页。
    public func settingsDismissed() {
        guard pairsAfterSettings else { return }
        pairsAfterSettings = false
        startPairing()
    }

    /// 设备名改了：各台电脑的连接下次连上时报新名字。新建的连接由 `AppDependencies.makeLink` 按存着的
    /// 设置取名字。
    private func deviceNameChanged(_ name: String) {
        for list in sessionLists.values {
            let link = list.link
            Task { await link.setDeviceName(name) }
        }
    }

    /// 要删掉的电脑：先退出它的页面、断开连接。
    private func forget(machine id: UUID) {
        path.removeAll { $0.machine == id }
        sessionLists[id]?.stop()
        sessionLists[id] = nil
    }
}
