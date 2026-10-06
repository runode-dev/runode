import Foundation
import RunodeConnection
import RunodeProtocol
import Testing

@testable import RunodeFeatures

private let waiting = SessionId("11111111111111111111111111111111")!
private let working = SessionId("22222222222222222222222222222222")!
private let plain = SessionId("33333333333333333333333333333333")!

private func info(_ id: SessionId, _ state: AgentState?, exited: Bool = false) -> SessionInfo {
    SessionInfo(
        id: id, size: smallGrid,
        meta: SessionMeta(
            title: "\(id)", agent: state.map { Agent(kind: AgentKind("claude"), state: $0) }, cwd: "/Users/ethan/dev"),
        exited: exited)
}

/// 两台 Mac，各自一条假连接。
@MainActor
private struct TwoMacs {
    let first = machineRecord(name: "一", fingerprintByte: 1)
    let second: MachineRecord = {
        var record = machineRecord(name: "二", fingerprintByte: 2)
        record.pairedAt += 1
        return record
    }()
    let firstLink = FakeLink()
    let secondLink = FakeLink()
    let recents = MemoryRecentTerminalStore()
    let app: AppModel

    /// `savedTitle` 不为空时，先记下第二台上的普通 shell 是上次打开的终端。
    init(savedTitle: String? = nil) async {
        let store = InMemoryMachineStore()
        await store.upsert(first)
        await store.upsert(second)
        if let savedTitle {
            recents.save(RecentTerminal(machine: second.id, session: plain, title: savedTitle, directory: "/tmp"))
        }
        let links = [first.id: firstLink, second.id: secondLink]
        let paired = second
        app = AppModel(
            dependencies: AppDependencies(
                store: store, keyStore: InMemoryKeyStore(), pairing: FakePairing { _ in paired },
                makeLink: { links[$0.id]! }, deviceName: "测试 iPhone", recents: recents))
        await app.machineList.load()
    }

    /// 第一台连上，列出这些会话；第二台没连上。
    func connectFirst(_ sessions: [SessionInfo]) throws {
        let list = try #require(app.sessionList(for: first.id))
        list.handle(.state(.connected(hostName: "一", address: nil)))
        list.handle(.ready(generation: 1))
        list.handle(.message(.sessionList(sessions)))
    }
}

@MainActor
@Suite struct HomeTests {
    @Test func everyPairedMacIsConnected() async {
        let macs = await TwoMacs()
        #expect(await eventually { macs.firstLink.starts == 1 && macs.secondLink.starts == 1 })
        #expect(macs.app.machineLists.map(\.machine.name) == ["一", "二"])
    }

    @Test func theSummaryCountsConnectedMacsOnly() async throws {
        let macs = await TwoMacs()
        try macs.connectFirst([info(waiting, .blocked), info(working, .working), info(plain, nil), info(sessionA, nil, exited: true)])
        // 第二台没连着，留下的旧列表不算。
        let second = try #require(macs.app.sessionList(for: macs.second.id))
        second.handle(.ready(generation: 1))
        second.handle(.message(.sessionList([info(sessionB, .blocked)])))
        second.handle(.state(.waiting(reason: "断了", retryAt: .now + 5)))
        #expect(macs.app.summary == HomeSummary(waiting: 1, working: 1, sessions: 3))
        #expect(macs.app.waitingSessions.map(\.session.id) == [waiting])
        #expect(macs.app.spawnableLists.map(\.machine.id) == [macs.first.id])
    }

    @Test func openingATerminalIsRememberedForResume() async throws {
        let macs = await TwoMacs()
        try macs.connectFirst([info(waiting, .blocked), info(plain, nil)])
        macs.app.openTerminal(machine: macs.first.id, session: plain)
        #expect(macs.app.path == [.machine(macs.first.id), .terminal(machine: macs.first.id, session: plain)])
        let saved = try #require(macs.recents.load())
        #expect(saved == RecentTerminal(machine: macs.first.id, session: plain, title: "\(plain)", directory: "/Users/ethan/dev"))
        macs.app.path = []
        #expect(macs.app.resume?.session == plain)
        macs.app.resumeRecent()
        #expect(macs.app.path == [.machine(macs.first.id), .terminal(machine: macs.first.id, session: plain)])
        // 会话结束了，「继续」就不出现。
        macs.app.path = []
        let list = try #require(macs.app.sessionList(for: macs.first.id))
        list.handle(.message(.sessionList([info(waiting, .blocked)])))
        #expect(macs.app.resume == nil)
    }

    /// 还没连上时「继续」用记下的标题；连上后列表里没有这个会话（结束了）就不显示。
    @Test func resumeShowsTheSavedTitleBeforeConnecting() async throws {
        let macs = await TwoMacs(savedTitle: "上次的")
        #expect(macs.app.resume?.title == "上次的")
        #expect(macs.app.resume?.machine.id == macs.second.id)
        let list = try #require(macs.app.sessionList(for: macs.second.id))
        list.handle(.ready(generation: 1))
        list.handle(.message(.sessionList([info(sessionA, nil)])))
        #expect(macs.app.resume == nil)
        // 那台 Mac 删掉了也不显示。
        let other = await TwoMacs(savedTitle: "上次的")
        await other.app.machineList.delete(other.second.id)
        #expect(other.app.resume == nil)
    }

    /// 新开的会话开好以后打开它：在会话列表上开的压在列表上面，首页开的连同列表一起换上。
    @Test func spawnedSessionsOpen() async throws {
        let macs = await TwoMacs()
        try macs.connectFirst([])
        let list = try #require(macs.app.sessionList(for: macs.first.id))
        await list.spawn()
        guard case .spawn(let req, _, _, _, _)? = macs.firstLink.sent.last else {
            Issue.record("expected a spawn, got \(macs.firstLink.sent)")
            return
        }
        list.handle(.message(.spawned(req: req, id: sessionA)))
        #expect(macs.app.path == [.machine(macs.first.id), .terminal(machine: macs.first.id, session: sessionA)])

        macs.app.path = [.machine(macs.first.id)]
        await list.spawn()
        guard case .spawn(let next, _, _, _, _)? = macs.firstLink.sent.last else {
            Issue.record("expected a spawn, got \(macs.firstLink.sent)")
            return
        }
        list.handle(.message(.spawned(req: next, id: sessionB)))
        #expect(macs.app.path == [.machine(macs.first.id), .terminal(machine: macs.first.id, session: sessionB)])
    }

    @Test func renamingUpdatesTheConnectedList() async throws {
        let macs = await TwoMacs()
        let list = try #require(macs.app.sessionList(for: macs.first.id))
        await macs.app.machineList.rename(macs.first.id, to: "新名字")
        #expect(list.machine.name == "新名字")
        #expect(macs.app.sessionList(for: macs.first.id) === list)
    }

    @Test func machineCardsSummariseTheirSessions() {
        let connected = LinkState.connected(hostName: "Mac", address: nil)
        #expect(
            Presentation.machineSummary(
                connected, sessions: [info(waiting, .blocked), info(working, .working), info(plain, nil)], loaded: true)
                == "3 个会话 · 1 个等你回答 · 1 个在干活")
        #expect(Presentation.machineSummary(connected, sessions: [info(plain, nil, exited: true)], loaded: true) == "没有终端")
        #expect(Presentation.machineSummary(connected, sessions: [], loaded: false) == "已连接")
        #expect(Presentation.machineSummary(.idle, sessions: [info(plain, nil)], loaded: true) == "未连接")
    }
}
