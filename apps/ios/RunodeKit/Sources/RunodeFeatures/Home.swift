import Foundation
import RunodeConnection
import RunodeProtocol

/// 上次打开的终端：首页「上次打开」那一块，点一下回到它。标题和目录是打开时的样子，连上以后以会话列表
/// 里实时的为准。
public struct RecentTerminal: Hashable, Sendable, Codable {
    public var machine: UUID
    public var session: SessionId
    public var title: String
    public var directory: String?

    public init(machine: UUID, session: SessionId, title: String, directory: String?) {
        self.machine = machine
        self.session = session
        self.title = title
        self.directory = directory
    }
}

/// 首页顶上的几个数：连着的电脑上加起来。
public struct HomeSummary: Hashable, Sendable {
    public var waiting = 0
    public var working = 0
    /// 没退出的会话。
    public var sessions = 0
}

/// 首页「上次打开」那一块要显示的东西。
public struct ResumeItem: Hashable, Sendable {
    public var machine: MachineRecord
    public var session: SessionId
    public var title: String
    public var directory: String?
    /// 连上以后会话里实时的 agent；还没连上时为空。
    public var agent: Agent?
}

/// 首页「等你回答」里的一个会话，带着它所在的电脑和会话列表（快速回复经那条连接发）。
public struct WaitingSession: Identifiable {
    public let list: SessionListModel
    public let session: SessionInfo
    /// 哪台电脑上的哪个会话。
    public let id: String

    @MainActor
    init(list: SessionListModel, session: SessionInfo) {
        self.list = list
        self.session = session
        id = "\(list.machine.id)/\(session.id)"
    }
}

extension AppModel {
    /// 配对过的每台电脑的会话列表，按配对的先后。
    public var machineLists: [SessionListModel] {
        machineList.machines.compactMap { sessionList(for: $0.id) }
    }

    /// 连着的电脑上的统计；断开的电脑上留着的旧列表不算。
    public var summary: HomeSummary {
        var summary = HomeSummary()
        for list in machineLists where list.linkState.isConnected {
            for session in list.sessions where !session.exited {
                summary.sessions += 1
                switch SessionGroup.of(session) {
                case .waiting: summary.waiting += 1
                case .working: summary.working += 1
                case .other: break
                }
            }
        }
        return summary
    }

    /// 连着的电脑上等用户回答的会话，按电脑的先后、再按宿主给的先后。
    public var waitingSessions: [WaitingSession] {
        machineLists.filter(\.linkState.isConnected).flatMap { list in
            list.sessions.filter { SessionGroup.of($0) == .waiting }.map { WaitingSession(list: list, session: $0) }
        }
    }

    /// 上次打开的终端；那台电脑删掉了，或者连上后列表里已经没有这个会话（结束了）时为空。
    public var resume: ResumeItem? {
        guard let recent, let machine = machineList.machine(recent.machine) else { return nil }
        let list = sessionList(for: recent.machine)
        let live = list?.session(recent.session)
        if let list, list.loaded, live == nil || live?.exited == true { return nil }
        return ResumeItem(
            machine: machine, session: recent.session,
            title: live.map(Presentation.sessionTitle) ?? recent.title,
            directory: live?.meta.cwd ?? recent.directory, agent: live?.meta.agent)
    }

    /// 能新开会话的电脑：连着的。
    public var spawnableLists: [SessionListModel] {
        machineLists.filter(\.linkState.isConnected)
    }

    /// 打开上次的终端。
    public func resumeRecent() {
        guard let resume else { return }
        openTerminal(machine: resume.machine.id, session: resume.session)
    }

    /// 首页在看着的时候：每台电脑都定时刷新列表和在干活、等回答的会话的预览。
    public func keepHomeRefreshing() async {
        await withTaskGroup(of: Void.self) { group in
            for list in machineLists {
                group.addTask { await list.keepRefreshing() }
            }
        }
    }

    /// 下拉刷新：每台都重新要列表、刷新全部预览，断开的立刻重连。
    public func refreshHome() {
        for list in machineLists {
            switch list.linkState {
            case .connected:
                list.refresh()
                list.refreshPreviews()
            case .waiting, .failed:
                list.reconnect()
            case .idle, .connecting:
                break
            }
        }
    }
}
