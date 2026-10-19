import Foundation
import Observation
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
    /// 通知的系统接口；默认为空，不登记推送、不清通知（测试、演示模式）。
    public var notifications: (any PushNotifications)?
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
        notifications = nil
        pushIdentity = nil
    }
}

/// 整个 App 的状态：导航栈、各页的视图模型、每台电脑一条连接。配对过的每台电脑在 App 处于前台
/// 时都连着，首页据此显示各台的状态、等回答的会话和统计；App 进后台时全部断开，回到前台再连上、
/// 各页重新 `Attach`。
///
/// agent 等回答时由电脑经 APNs 推通知，手机不用连着：每台电脑连上时由 `PushRegistration` 登记推送。
/// 已经不等回答的会话的通知由 `clearAnsweredNotifications` 清掉。
@Observable
@MainActor
public final class AppModel {
    public var path: [Route] = [] {
        didSet { pathChanged() }
    }
    /// 配对页开着时的视图模型。换掉、关掉时撤掉它正在跑的配对。
    public var pairing: PairingModel? {
        didSet { if oldValue !== pairing { oldValue?.cancel() } }
    }
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
    /// 终端页上切过去的分屏：键是栈里的终端页，值是它现在显示的会话。切分屏不改导航栈：栈顶的值一变，
    /// SwiftUI 会把这一页工具栏上的按钮整组重建，右上角闪一下。
    private var panes: [Route: SessionId] = [:]
    /// 终端页上显示过的会话，最近的在最后；分屏结束时按它切回上次看的那个。
    @ObservationIgnored private var shownHistory: [SessionId] = []
    @ObservationIgnored private var active = true
    /// 系统交来、要等电脑列表读进来才能打开的会话链接（冷启动时）。
    @ObservationIgnored private var pendingLink: SessionLink?
    /// 正在查通知中心的电脑，查完之前列表又变了不再查一次：查完时按那时的列表筛。
    @ObservationIgnored private var clearingNotifications: Set<UUID> = []

    public init(dependencies: AppDependencies) {
        self.dependencies = dependencies
        machineList = MachineListModel(store: dependencies.store, keyStore: dependencies.keyStore)
        let settings = SettingsModel(store: dependencies.preferences, systemDeviceName: dependencies.deviceName)
        self.settings = settings
        push = PushRegistration(
            system: dependencies.notifications, identity: dependencies.pushIdentity,
            enabled: settings.preferences.alertsBlockedAgents)
        recent = dependencies.recents.load()
        savedTheme = dependencies.themes.load()
        machineList.willDelete = { [weak self] id in await self?.forget(machine: id) ?? true }
        machineList.didLoad = { [weak self] in self?.syncConnections() }
        settings.deviceNameDidChange = { [weak self] name in self?.deviceNameChanged(name) }
        settings.alertsDidChange = { [weak self] enabled in self?.push.enabled = enabled }
    }

    /// 打开一个终端页：在这台电脑的会话列表上时压在它上面，别处（首页、别的电脑）打开时连同它的
    /// 会话列表一起换上，返回时先回到列表。
    public func openTerminal(machine: UUID, session: SessionId) {
        let route = Route.terminal(machine: machine, session: session)
        // 栈顶已经是这个终端页、只是切到了别的分屏时，回到它本身。
        panes[route] = nil
        if path.last == .machine(machine) {
            path.append(route)
        } else {
            path = [.machine(machine), route]
        }
    }

    /// 终端页上切到电脑上同一个标签里的另一个分屏：栈顶的终端页原地改显示这个会话，导航栈不动，返回时
    /// 照旧回到列表。旧终端关掉；终端视图接着用，键盘开着就还开着。
    public func switchTerminal(machine: UUID, to session: SessionId) {
        guard let last = path.last, case .terminal(machine, let entry) = last else { return }
        let current = shown(last)
        guard current != .terminal(machine: machine, session: session) else { return }
        let keyboard = terminals[current]?.keyboardVisible ?? false
        panes[last] = session == entry ? nil : session
        pathChanged()
        // 先记上，底部的按键栏这一帧就不出来。
        terminal(machine: machine, session: session)?.setKeyboardVisible(keyboard)
    }

    /// 终端页上显示的分屏 `session` 结束了：切回 `tabMates`（它所在标签里的分屏，结束前最后一次知道的）
    /// 里上次看过、还没结束的那个，都没看过时切到第一个还没结束的；一个都没有时留在这页，盖着印章。
    public func leaveEndedPane(machine: UUID, session: SessionId, tabMates: [SessionId]) {
        guard let list = sessionList(for: machine) else { return }
        let alive = tabMates.filter { $0 != session && list.session($0).map { !$0.exited } == true }
        guard let target = shownHistory.last(where: alive.contains) ?? alive.first else { return }
        switchTerminal(machine: machine, to: target)
    }

    /// 栈里这个终端页现在显示的会话：切过分屏的是切到的那个。
    public func shownSession(machine: UUID, session: SessionId) -> SessionId {
        panes[.terminal(machine: machine, session: session)] ?? session
    }

    private func shown(_ route: Route) -> Route {
        guard case .terminal(let machine, let session) = route else { return route }
        return .terminal(machine: machine, session: shownSession(machine: machine, session: session))
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

    /// 系统交来的 `runode://` 链接：`runode://pair?…` 打开配对页，`runode://session?…` 打开那台电脑上那个
    /// 会话的终端页。别的不认。
    public func open(url: URL) {
        guard url.scheme?.lowercased() == "runode" else { return }
        switch url.host?.lowercased() {
        case "pair":
            startPairing(link: url.absoluteString)
        case SessionLink.host:
            guard let link = SessionLink(url: url) else { return }
            open(link)
        default:
            break
        }
    }

    /// 打开那台电脑上那个会话的终端页：点开了等回答的通知，或者系统交来了会话链接。
    public func open(_ link: SessionLink) {
        pendingLink = link
        openPendingLink()
    }

    /// 打开记下的会话链接。电脑列表还没读进来时先留着，读进来以后（`syncConnections`）再开；不认识的
    /// 电脑、写法不对的会话编号就算了。
    private func openPendingLink() {
        guard machineList.loaded, let link = pendingLink else { return }
        pendingLink = nil
        guard machineList.machine(link.machine) != nil, let session = SessionId(link.session) else { return }
        showingSettings = false
        pairsAfterSettings = false
        // 配对页盖在整个导航栈上，不关掉就看不见打开的终端。
        pairing = nil
        openTerminal(machine: link.machine, session: session)
    }

    /// 打开配对页；`link` 是从别处（比如系统打开的 `runode://pair` 链接）带来的配对链接，只填进输入框，
    /// 由用户看过要配的电脑后点「配对」：任何网页、短信都能发这种链接，自动配对会让手机连上别人的主机。
    public func startPairing(link: String? = nil) {
        let model = PairingModel(pairing: dependencies.pairing, deviceName: settings.deviceName) {
            [weak self] machine in
            try await self?.machineList.add(machine)
        }
        if let link {
            model.linkText = link
        }
        pairing = model
    }

    public func sessionList(for machineId: UUID) -> SessionListModel? {
        if let existing = sessionLists[machineId] { return existing }
        guard let machine = machineList.machine(machineId) ?? machineList.leaving.first(where: { $0.id == machineId })
        else { return nil }
        let model = SessionListModel(machine: machine, link: dependencies.makeLink(machine))
        model.onSpawned = { [weak self] session in self?.openTerminal(machine: machineId, session: session) }
        model.onThemeChanged = { [weak self] in self?.saveTheme() }
        model.onSessionsChanged = { [weak self, weak model] in
            if let model { self?.clearAnsweredNotifications(on: model) }
        }
        model.onLinkEvent = { [weak self, weak model] event in
            guard let self, let model else { return }
            self.linkEvent(event, on: model)
        }
        sessionLists[machineId] = model
        model.link.setFocused(path.contains { $0.machine == machineId })
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
            link: list.link, linkState: list.linkState, ownerHint: hint, sizePreference: settings.preferences.defaultSize,
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

    /// App 回到前台或进了后台。后台里 iOS 会挂起 App，连接迟早被断，主动断开干净；回来时重连。
    public func setActive(_ isActive: Bool) {
        guard isActive != active else { return }
        active = isActive
        for list in sessionLists.values {
            if isActive { list.start() } else { list.stop() }
        }
        // 在后台时打开的终端页（点通知、系统交来链接比回到前台先到）只建了模型、没订阅连接。
        if isActive {
            for model in terminals.values {
                model.open()
            }
        }
    }

    /// 一台电脑的连接事件：连上时登记推送，断开时不再等它的回话，消息里有登记的回话。
    private func linkEvent(_ event: HostEvent, on list: SessionListModel) {
        let id = list.machine.id
        switch event {
        case .ready:
            push.connected(list.machine, link: list.link)
            if list.machine.deletedAt != nil {
                Task { [weak self] in await self?.finishDeleting(machine: id) }
            }
        case .state(.failed(.rejected(.unknownDevice))) where list.machine.deletedAt != nil:
            // 电脑那边撤销了这部手机，撤销时连推送登记一起删了，用不着再注销；不再连了，留着只剩一把私钥。
            push.disconnected(id)
            Task { [weak self] in await self?.machineList.purge(id) }
        case .state(let state) where !state.isConnected:
            push.disconnected(id)
        case .message(let message):
            push.handle(message, from: id)
        default:
            break
        }
    }

    /// 前台、连着这台电脑、这次连上以后收到过列表时，从通知中心清掉这台电脑上已经不等回答（或者没了）的
    /// 会话的通知：电脑撤不回发出去的通知。
    private func clearAnsweredNotifications(on list: SessionListModel) {
        let machine = list.machine.id
        guard let system = dependencies.notifications, current(list), !clearingNotifications.contains(machine)
        else { return }
        clearingNotifications.insert(machine)
        Task { [weak self, weak list] in
            let delivered = await system.delivered()
            guard let self else { return }
            self.clearingNotifications.remove(machine)
            guard let list, self.current(list) else { return }
            let answered = delivered.filter { notification in
                notification.link.machine == machine
                    && SessionId(notification.link.session).flatMap(list.session).map(SessionGroup.of) != .waiting
            }
            if !answered.isEmpty { system.removeDelivered(answered.map(\.id)) }
        }
    }

    /// 能拿这台电脑的列表判断哪些会话还在等：App 在前台、连着、这次连上以后收到过列表。
    private func current(_ list: SessionListModel) -> Bool {
        active && list.linkState.isConnected && list.listCurrent
    }

    /// 导航栈变了或切了分屏：退出去的终端页、Git 页关掉，新打开的终端记作「上次打开」。会话列表的连接
    /// 不跟导航栈走，只是栈里有页面的电脑算在看（`HostLink.setFocused`），连不上时重试得勤一些。
    private func pathChanged() {
        panes = panes.filter { path.contains($0.key) }
        let live = Set(path.map(shown))
        for (route, model) in terminals where !live.contains(route) {
            model.close()
            terminals[route] = nil
        }
        for (route, model) in gits where !live.contains(route) {
            model.close()
            gits[route] = nil
        }
        if case .terminal(let machine, let session)? = path.last.map(shown) {
            remember(machine: machine, session: session)
            shownHistory.removeAll { $0 == session }
            shownHistory.append(session)
            if shownHistory.count > 32 { shownHistory.removeFirst() }
        }
        let viewed = Set(path.map(\.machine))
        for (id, list) in sessionLists {
            list.link.setFocused(viewed.contains(id))
        }
    }

    /// 配对过的电脑变了（配对、改名、删除之后重新读了列表）：每台都有一条连接，删掉的断开，改了
    /// 名字的换上新记录。
    private func syncConnections() {
        // 删掉了、还没注销推送的也连着，连上了好注销。
        let machines = machineList.machines + machineList.leaving
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

    /// 要删掉的电脑：先退出它的页面；连着的话注销推送，限时等电脑回话再断开连接。电脑那边注销好了
    /// 时为真。
    private func forget(machine id: UUID) async -> Bool {
        path.removeAll { $0.machine == id }
        let unregistered = await push.unregister(id, timeout: .seconds(2))
        push.forget(id)
        sessionLists[id]?.stop()
        sessionLists[id] = nil
        return unregistered
    }

    /// 删掉时没注销成推送的电脑（`MachineRecord.deletedAt`）又连上了：注销好了才真删掉。
    private func finishDeleting(machine id: UUID) async {
        guard await push.unregister(id, timeout: .seconds(5)) else { return }
        await machineList.purge(id)
    }

    /// App 在前台时这条通知弹不弹：删掉了的、不认识的电脑的不弹。电脑列表还没读进来时照弹。
    public func showsNotification(for link: SessionLink) -> Bool {
        !machineList.loaded || machineList.machine(link.machine) != nil
    }
}
