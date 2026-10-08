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
    /// 灵动岛和锁屏上 Live Activity 的系统接口；默认为空，不登记推送、不收起过时的（测试、演示模式）。
    public var liveActivities: (any LiveActivities)?
    /// 登记推送时报给电脑的 App 身份，App 入口从 Info.plist 读；为空时不登记推送。
    public var pushIdentity: PushIdentity?

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
        liveActivities = nil
        pushIdentity = nil
    }
}

/// 整个 App 的状态：导航栈、各页的视图模型、每台电脑一条连接。配对过的每台电脑在 App 处于前台
/// 时都连着，首页据此显示各台的状态、等回答的会话和统计；App 进后台时全部断开，回到前台再连上、
/// 各页重新 `Attach`。
///
/// agent 等回答时灵动岛和锁屏上的提醒由电脑经推送起、更新和收起，手机不用连着：每台电脑连上时由
/// `PushRegistration` 登记推送。App 在前台、连着某台电脑时，本地还留着它上面已经不等回答的会话的
/// 提醒（电脑没收起来）就在这里收起。
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
    /// 向各台电脑登记推送，设置页按它显示哪台电脑不支持。
    public let push: PushRegistration
    /// 上次打开的终端，首页的「上次打开」用它。
    public private(set) var recent: RecentTerminal?
    /// 上次用的主题，电脑还没连上时用它。
    private var savedTheme: AppTheme?

    @ObservationIgnored private let dependencies: AppDependencies
    @ObservationIgnored private var sessionLists: [UUID: SessionListModel] = [:]
    @ObservationIgnored private var terminals: [Route: TerminalModel] = [:]
    @ObservationIgnored private var gits: [Route: GitModel] = [:]
    @ObservationIgnored private var active = true
    /// 系统交来、要等电脑列表读进来才能打开的会话链接（冷启动时）。
    @ObservationIgnored private var pendingLink: SessionLink?
    /// 正在收起的提醒，收起之前列表又变了也不再叫一次。
    @ObservationIgnored private var endingActivities: Set<AgentActivityAttributes> = []

    public init(dependencies: AppDependencies) {
        self.dependencies = dependencies
        machineList = MachineListModel(store: dependencies.store, keyStore: dependencies.keyStore)
        let settings = SettingsModel(store: dependencies.preferences, systemDeviceName: dependencies.deviceName)
        self.settings = settings
        push = PushRegistration(
            system: dependencies.liveActivities, identity: dependencies.pushIdentity,
            enabled: settings.preferences.alertsBlockedAgents)
        recent = dependencies.recents.load()
        savedTheme = dependencies.themes.load()
        machineList.willDelete = { [weak self] id in self?.forget(machine: id) }
        machineList.didLoad = { [weak self] in self?.syncConnections() }
        settings.deviceNameDidChange = { [weak self] name in self?.deviceNameChanged(name) }
        settings.alertsDidChange = { [weak self] enabled in self?.push.enabled = enabled }
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

    /// 系统交来的 `runode://` 链接：`runode://pair?…` 打开配对页，`runode://session?…`（Live Activity 的
    /// 卡片）打开那台电脑上那个会话的终端页。别的不认。
    public func open(url: URL) {
        guard url.scheme?.lowercased() == "runode" else { return }
        switch url.host?.lowercased() {
        case "pair":
            startPairing(link: url.absoluteString)
        case SessionLink.host:
            guard let link = SessionLink(url: url) else { return }
            pendingLink = link
            openPendingLink()
        default:
            break
        }
    }

    /// 打开记下的会话链接。电脑列表还没读进来时先留着，读进来以后（`syncConnections`）再开；不认识的
    /// 电脑、写法不对的会话编号就算了。
    private func openPendingLink() {
        guard machineList.loaded, let link = pendingLink else { return }
        pendingLink = nil
        guard machineList.machine(link.machine) != nil, let session = SessionId(link.session) else { return }
        showingSettings = false
        pairsAfterSettings = false
        openTerminal(machine: link.machine, session: session)
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
        model.onSessionsChanged = { [weak self, weak model] in
            if let model { self?.endAnsweredActivities(on: model) }
        }
        model.onLinkEvent = { [weak self, weak model] event in
            guard let self, let model else { return }
            self.linkEvent(event, on: model)
        }
        sessionLists[machineId] = model
        if active { model.start() }
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
            sessionId: session, title: info?.meta.displayTitle ?? String(localized: "终端"), agent: info?.meta.agent,
            link: list.link, ownerHint: hint, sizePreference: settings.preferences.defaultSize,
            onOpen: { [weak list] id in list?.screenOpened(id) },
            onClose: { [weak list] id in list?.screenClosed(id) })
        terminals[route] = model
        if active { model.open() }
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

    /// App 回到前台或进了后台。后台里 iOS 会挂起 App，连接迟早被断，主动断开干净；回来时重连。断开时
    /// 会话列表不再收事件，不会转来断开的状态，这里直接告诉推送登记别等回话了，回来连上时再登记。
    public func setActive(_ isActive: Bool) {
        guard isActive != active else { return }
        active = isActive
        for list in sessionLists.values {
            if isActive {
                list.start()
            } else {
                list.stop()
                push.disconnected(list.machine.id)
            }
        }
    }

    /// 一台电脑的连接事件：连上时登记推送，断开时不再等它的回话，消息里有登记的回话。
    private func linkEvent(_ event: HostEvent, on list: SessionListModel) {
        let id = list.machine.id
        switch event {
        case .ready:
            push.connected(list.machine, link: list.link)
        case .state(let state) where !state.isConnected:
            push.disconnected(id)
        case .message(let message):
            push.handle(message, from: id)
        default:
            break
        }
    }

    /// 前台、连着这台电脑、这次连上以后收到过列表时，收起本地还留着的、这台电脑上已经不等回答
    /// （或者没了）的会话的提醒。电脑在会话答完时会自己收起，这是它没收成时的兜底。
    private func endAnsweredActivities(on list: SessionListModel) {
        guard active, list.linkState.isConnected, list.listCurrent, let system = dependencies.liveActivities else {
            return
        }
        for shown in system.shown where UUID(uuidString: shown.machine) == list.machine.id {
            let session = SessionId(shown.session).flatMap(list.session)
            guard session.map(SessionGroup.of) != .waiting, !endingActivities.contains(shown) else { continue }
            endingActivities.insert(shown)
            Task { [weak self] in
                await system.end(machine: shown.machine, session: shown.session)
                self?.endingActivities.remove(shown)
            }
        }
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
            push.forget(id)
        }
        for machine in machines {
            if let list = sessionLists[machine.id] {
                list.machine = machine
            } else {
                _ = sessionList(for: machine.id)
            }
            push.renamed(machine)
        }
        openPendingLink()
    }

    private func remember(machine: UUID, session: SessionId) {
        let info = sessionLists[machine]?.session(session)
        let previous = recent?.machine == machine && recent?.session == session ? recent : nil
        let entry = RecentTerminal(
            machine: machine, session: session,
            title: info.map(Presentation.sessionTitle) ?? previous?.title ?? String(localized: "终端"),
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
        push.forget(id)
        path.removeAll { $0.machine == id }
        sessionLists[id]?.stop()
        sessionLists[id] = nil
    }
}
