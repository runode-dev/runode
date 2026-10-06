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
    /// 上次打开的终端记在哪里；不给时只记在内存里（测试、演示模式）。
    public var recents: any RecentTerminalStore
    /// 设置存在哪里；不给时只记在内存里。
    public var preferences: any PreferencesStore

    @MainActor
    public init(
        store: any MachineStore, keyStore: any DeviceKeyStore, pairing: any Pairing,
        makeLink: @escaping @MainActor (MachineRecord) -> any HostLink, deviceName: String,
        recents: any RecentTerminalStore = MemoryRecentTerminalStore(),
        preferences: any PreferencesStore = MemoryPreferencesStore()
    ) {
        self.store = store
        self.keyStore = keyStore
        self.pairing = pairing
        self.makeLink = makeLink
        self.deviceName = deviceName
        self.recents = recents
        self.preferences = preferences
    }
}

/// 整个 App 的状态：导航栈、各页的视图模型、每台电脑一条连接。配对过的每台电脑在 App 处于前台
/// 时都连着，首页据此显示各台的状态、等回答的会话和统计；App 进后台时全部断开，回到前台再连上、
/// 各页重新 `Attach`。
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
    /// 上次打开的终端，首页的「继续」用它。
    public private(set) var recent: RecentTerminal?

    @ObservationIgnored private let dependencies: AppDependencies
    @ObservationIgnored private var sessionLists: [UUID: SessionListModel] = [:]
    @ObservationIgnored private var terminals: [Route: TerminalModel] = [:]
    @ObservationIgnored private var gits: [Route: GitModel] = [:]
    @ObservationIgnored private var active = true

    public init(dependencies: AppDependencies) {
        self.dependencies = dependencies
        machineList = MachineListModel(store: dependencies.store, keyStore: dependencies.keyStore)
        settings = SettingsModel(store: dependencies.preferences, systemDeviceName: dependencies.deviceName)
        recent = dependencies.recents.load()
        machineList.willDelete = { [weak self] id in self?.forget(machine: id) }
        machineList.didLoad = { [weak self] in self?.syncConnections() }
        settings.deviceNameDidChange = { [weak self] name in self?.deviceNameChanged(name) }
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
            sessionId: session, title: info?.meta.displayTitle ?? "终端", agent: info?.meta.agent,
            link: list.link, ownerHint: hint, sizePreference: settings.preferences.defaultSize,
            onOpen: { [weak list] id in list?.screenOpened(id) },
            onClose: { [weak list] id in list?.screenClosed(id) })
        terminals[route] = model
        if active { model.open() }
        return model
    }

    /// App 回到前台或进了后台。后台里 iOS 会挂起 App，连接迟早被断，主动断开干净；回来时重连。
    public func setActive(_ isActive: Bool) {
        guard isActive != active else { return }
        active = isActive
        for list in sessionLists.values {
            if isActive {
                list.start()
            } else {
                list.stop()
            }
        }
    }

    /// 导航栈变了：退出去的终端页、Git 页关掉，新打开的终端记作「继续」。会话列表的连接不跟导航栈走。
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
