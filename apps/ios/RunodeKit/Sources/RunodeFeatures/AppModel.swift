import Foundation
import Observation
import RunodeConnection
import RunodeProtocol

/// 导航栈里的一页。
public enum Route: Hashable, Sendable {
    /// 一台 Mac 的会话列表。
    case machine(UUID)
    /// 一个会话的终端页。
    case terminal(machine: UUID, session: SessionId)
}

/// App 用到的外部依赖，由 App 入口组装，测试换成假的。
public struct AppDependencies {
    public var store: any MachineStore
    public var keyStore: any DeviceKeyStore
    public var pairing: any Pairing
    /// 给一台 Mac 建连接。
    public var makeLink: @MainActor (MachineRecord) -> any HostLink
    /// 报给 Mac 的设备名。
    public var deviceName: String

    public init(
        store: any MachineStore, keyStore: any DeviceKeyStore, pairing: any Pairing,
        makeLink: @escaping @MainActor (MachineRecord) -> any HostLink, deviceName: String
    ) {
        self.store = store
        self.keyStore = keyStore
        self.pairing = pairing
        self.makeLink = makeLink
        self.deviceName = deviceName
    }
}

/// 整个 App 的状态：导航栈、各页的视图模型、每台 Mac 一条连接。连接跟着导航栈走：栈里有这台
/// Mac 的页面时连着，退出来就断开；App 进后台时全部断开，回到前台再连上、各页重新 `Attach`。
@Observable
@MainActor
public final class AppModel {
    public var path: [Route] = [] {
        didSet { pathChanged() }
    }
    /// 配对页开着时的视图模型。
    public var pairing: PairingModel?
    public let machineList: MachineListModel

    @ObservationIgnored private let dependencies: AppDependencies
    @ObservationIgnored private var sessionLists: [UUID: SessionListModel] = [:]
    @ObservationIgnored private var terminals: [Route: TerminalModel] = [:]
    @ObservationIgnored private var active = true

    public init(dependencies: AppDependencies) {
        self.dependencies = dependencies
        machineList = MachineListModel(store: dependencies.store, keyStore: dependencies.keyStore)
        machineList.willDelete = { [weak self] id in self?.forget(machine: id) }
    }

    /// 打开配对页；`link` 是从别处（比如系统打开的 `runode://pair` 链接）带来的配对链接。
    public func startPairing(link: String? = nil) {
        let model = PairingModel(pairing: dependencies.pairing, deviceName: dependencies.deviceName) {
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
        sessionLists[machineId] = model
        if active { model.start() }
        return model
    }

    public func terminal(machine machineId: UUID, session: SessionId) -> TerminalModel? {
        let route = Route.terminal(machine: machineId, session: session)
        if let existing = terminals[route] { return existing }
        guard let list = sessionList(for: machineId) else { return nil }
        let title = list.session(session)?.meta.displayTitle ?? "终端"
        let model = TerminalModel(
            sessionId: session, title: title, link: list.link,
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

    /// 导航栈变了：退出去的页面关掉，没有页面的 Mac 断开。
    private func pathChanged() {
        let live = Set(path)
        for (route, model) in terminals where !live.contains(route) {
            model.close()
            terminals[route] = nil
        }
        let machines = Set(path.map { route -> UUID in
            switch route {
            case .machine(let id): id
            case .terminal(let id, _): id
            }
        })
        for (id, list) in sessionLists where !machines.contains(id) {
            list.stop()
            sessionLists[id] = nil
        }
    }

    /// 要删掉的 Mac：先退出它的页面、断开连接。
    private func forget(machine id: UUID) {
        path.removeAll { route in
            switch route {
            case .machine(let machine): machine == id
            case .terminal(let machine, _): machine == id
            }
        }
        sessionLists[id]?.stop()
        sessionLists[id] = nil
    }
}
