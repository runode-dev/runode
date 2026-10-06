import Foundation
import RunodeConnection
import RunodeProtocol
import Testing

@testable import RunodeFeatures

@MainActor
@Suite struct SessionListModelTests {
    let link = FakeLink()

    func info(_ id: SessionId, title: String) -> SessionInfo {
        SessionInfo(id: id, size: smallGrid, meta: SessionMeta(title: title), sizeOwner: "Ethan 的 MacBook")
    }

    @Test func listsAndWatchesEverySession() {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.state(.connected(hostName: "Mac", address: nil)))
        model.handle(.ready(generation: 1))
        #expect(link.sent == [.listSessions])
        model.screenOpened(sessionB)
        model.handle(.message(.sessionList([info(sessionA, title: "a"), info(sessionB, title: "b")])))
        #expect(model.sessions.count == 2)
        #expect(model.loaded)
        // 终端页开着的会话不发只看状态的 `Attach`，免得换掉终端页的订阅。
        #expect(link.sent.dropFirst() == [.attach(id: sessionA, size: nil, mode: .metaOnly)])
        // 再列一次不重复发。
        link.clearSent()
        model.handle(.message(.sessionList([info(sessionA, title: "a"), info(sessionB, title: "b")])))
        #expect(link.sent.isEmpty)
    }

    @Test func metaUpdatesArriveLive() {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        model.handle(.message(.sessionList([info(sessionA, title: "a")])))
        let agent = Agent(kind: AgentKind("claude"), state: .blocked)
        model.handle(.message(.meta(id: sessionA, meta: SessionMeta(title: "修 bug", agent: agent))))
        #expect(model.session(sessionA)?.meta.title == "修 bug")
        #expect(model.session(sessionA)?.meta.agent == agent)
        model.handle(.message(.exited(id: sessionA, status: 0)))
        #expect(model.session(sessionA)?.exited == true)
    }

    @Test func closingATerminalGoesBackToMetaOnly() {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.state(.connected(hostName: "Mac", address: nil)))
        model.handle(.ready(generation: 1))
        model.screenOpened(sessionA)
        model.handle(.message(.sessionList([info(sessionA, title: "a")])))
        link.clearSent()
        model.screenClosed(sessionA)
        #expect(link.sent == [.attach(id: sessionA, size: nil, mode: .metaOnly)])
    }

    @Test func spawnWaitsForItsReply() async {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        link.clearSent()
        await model.spawn()
        #expect(model.isSpawning)
        guard case .spawn(let req, let size, nil, .detect, true)? = link.sent.first else {
            Issue.record("expected a spawn, got \(link.sent)")
            return
        }
        #expect(size == model.spawnSize)
        model.handle(.message(.spawned(req: req &+ 1, id: sessionB)))
        #expect(model.spawnedSession == nil)
        model.handle(.message(.spawned(req: req, id: sessionA)))
        #expect(model.spawnedSession == sessionA)
        #expect(!model.isSpawning)
    }

    @Test func spawnErrorsAreShown() async {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        await model.spawn()
        model.handle(.message(.error(req: 1, id: nil, message: "no shell")))
        #expect(model.errorMessage?.contains("no shell") == true)
        #expect(!model.isSpawning)
    }

    @Test func killRemovesTheSession() {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        model.handle(.message(.sessionList([info(sessionA, title: "a")])))
        model.killTarget = sessionA
        #expect(model.isConfirmingKill)
        link.clearSent()
        model.kill(sessionA)
        #expect(link.sent == [.kill(id: sessionA)])
        #expect(model.sessions.isEmpty)
    }

    @Test func disconnectingForgetsWhatWasWatched() {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        model.handle(.message(.sessionList([info(sessionA, title: "a")])))
        model.handle(.state(.waiting(reason: "断了", retryAt: .now)))
        #expect(model.linkState == .waiting(reason: "断了", retryAt: model.linkState.retryDate!))
        link.clearSent()
        model.handle(.ready(generation: 2))
        model.handle(.message(.sessionList([info(sessionA, title: "a")])))
        #expect(link.sent == [.listSessions, .attach(id: sessionA, size: nil, mode: .metaOnly)])
    }
}

extension LinkState {
    var retryDate: Date? {
        if case .waiting(_, let date) = self { return date }
        return nil
    }
}

@MainActor
@Suite struct MachineListModelTests {
    @Test func addRenameDelete() async throws {
        let store = InMemoryMachineStore()
        let keys = InMemoryKeyStore()
        let model = MachineListModel(store: store, keyStore: keys)
        var deleted: [UUID] = []
        model.willDelete = { deleted.append($0) }
        let machine = machineRecord()
        try keys.save(StoredDeviceKey(kind: .software, data: Data([1])), for: machine.id)
        await model.add(machine)
        #expect(model.machines.map(\.id) == [machine.id])

        model.beginRename(machine.id)
        #expect(model.isRenaming)
        #expect(model.renameText == "MacBook")
        await model.rename(machine.id, to: "  工作机  ")
        #expect(model.machines.first?.name == "工作机")
        await model.rename(machine.id, to: "   ")
        #expect(model.machines.first?.name == "工作机")

        await model.delete(machine.id)
        #expect(model.machines.isEmpty)
        #expect(deleted == [machine.id])
        #expect(try keys.key(for: machine.id) == nil)
    }

    @Test func repairingTheSameMacReplacesTheOldRecord() async throws {
        let store = InMemoryMachineStore()
        let keys = InMemoryKeyStore()
        let model = MachineListModel(store: store, keyStore: keys)
        let old = machineRecord(name: "旧")
        try keys.save(StoredDeviceKey(kind: .software, data: Data([1])), for: old.id)
        await model.add(old)
        let new = machineRecord(name: "新")
        await model.add(new)
        #expect(model.machines.map(\.name) == ["新"])
        #expect(try keys.key(for: old.id) == nil)
    }
}

@MainActor
@Suite struct PairingModelTests {
    let fingerprint = Base64URL.encode(Data(repeating: 1, count: 32))
    let secret = Base64URL.encode(Data(repeating: 2, count: 32))

    func link(exp: Int = 1_900_000_000) -> String {
        "runode://pair?v=1&name=Mac&fp=\(fingerprint)&secret=\(secret)&port=7866&addr=192.168.1.20&exp=\(exp)"
    }

    @Test func pairsAndHandsTheMachineOver() async {
        var paired: [MachineRecord] = []
        let machine = machineRecord()
        let model = PairingModel(
            pairing: FakePairing { invitation in
                #expect(invitation.hostName == "Mac")
                return machine
            }, deviceName: "测试 iPhone", now: { Date(timeIntervalSince1970: 1_800_000_000) },
            onPaired: { paired.append($0) })
        model.linkText = link()
        await model.submitLink()
        #expect(model.phase == .paired(machine))
        #expect(paired == [machine])
        // 配好以后再扫到码也不再配。
        await model.scanned(link())
        #expect(paired.count == 1)
    }

    @Test func badAndExpiredLinksFailWithoutPairing() async {
        let model = PairingModel(
            pairing: FakePairing { _ in
                Issue.record("should not pair")
                return machineRecord()
            }, deviceName: "x", now: { Date(timeIntervalSince1970: 1_950_000_000) }, onPaired: { _ in })
        model.linkText = "https://example.com"
        await model.submitLink()
        #expect(model.phase == .failed("这不是 runode 的配对链接"))
        model.linkText = link()
        await model.submitLink()
        #expect(model.phase == .failed("二维码已经过期，请在 Mac 上重新生成"))
    }

    @Test func rejectionsAreExplained() async {
        let model = PairingModel(
            pairing: FakePairing { _ in throw LinkFailure.rejected(.pairingInvalid) }, deviceName: "x",
            now: { Date(timeIntervalSince1970: 1_800_000_000) }, onPaired: { _ in })
        await model.scanned(link())
        #expect(model.phase == .failed("配对口令不对、已过期或已经用过，请在 Mac 上重新生成二维码"))
        model.reset()
        #expect(model.phase == .idle)
    }
}

@MainActor
@Suite struct AppModelTests {
    @Test func connectionsFollowTheNavigationStack() async throws {
        let store = InMemoryMachineStore()
        let machine = machineRecord()
        await store.upsert(machine)
        let link = FakeLink()
        let app = AppModel(
            dependencies: AppDependencies(
                store: store, keyStore: InMemoryKeyStore(),
                pairing: FakePairing { _ in machine }, makeLink: { _ in link }, deviceName: "测试 iPhone"))
        await app.machineList.load()
        app.path = [.machine(machine.id)]
        let list = try #require(app.sessionList(for: machine.id))
        #expect(await eventually { link.starts == 1 })
        app.path.append(.terminal(machine: machine.id, session: sessionA))
        let terminal = try #require(app.terminal(machine: machine.id, session: sessionA))
        #expect(app.terminal(machine: machine.id, session: sessionA) === terminal)
        #expect(list.link === link)
        // 退出到根：终端页关掉、连接断开。
        app.path = []
        #expect(await eventually { link.stops == 1 })
        #expect(app.sessionList(for: machine.id) !== list)
    }

    @Test func backgroundDisconnectsAndForegroundReconnects() async throws {
        let store = InMemoryMachineStore()
        let machine = machineRecord()
        await store.upsert(machine)
        let link = FakeLink()
        let app = AppModel(
            dependencies: AppDependencies(
                store: store, keyStore: InMemoryKeyStore(),
                pairing: FakePairing { _ in machine }, makeLink: { _ in link }, deviceName: "测试 iPhone"))
        await app.machineList.load()
        app.path = [.machine(machine.id)]
        _ = app.sessionList(for: machine.id)
        #expect(await eventually { link.starts == 1 })
        app.setActive(false)
        #expect(await eventually { link.stops == 1 })
        app.setActive(true)
        #expect(await eventually { link.starts == 2 })
    }

    @Test func deletingAMachineLeavesItsPages() async throws {
        let store = InMemoryMachineStore()
        let machine = machineRecord()
        await store.upsert(machine)
        let link = FakeLink()
        let app = AppModel(
            dependencies: AppDependencies(
                store: store, keyStore: InMemoryKeyStore(),
                pairing: FakePairing { _ in machine }, makeLink: { _ in link }, deviceName: "测试 iPhone"))
        await app.machineList.load()
        app.path = [.machine(machine.id)]
        _ = app.sessionList(for: machine.id)
        await app.machineList.delete(machine.id)
        #expect(app.path.isEmpty)
        #expect(app.machineList.machines.isEmpty)
    }
}

@Suite struct PresentationTests {
    @Test func directoriesUnderHomeAreShortened() {
        #expect(Presentation.directory("/Users/ethan/dev/runode") == "~/dev/runode")
        #expect(Presentation.directory("/Users/ethan") == "~")
        #expect(Presentation.directory("/tmp") == "/tmp")
        #expect(Presentation.directory(nil) == nil)
    }

    @Test func agentStatesReadNaturally() {
        #expect(Presentation.agentStatus(Agent(kind: AgentKind("claude"), state: .working))?.text == "Claude Code · 干活中")
        #expect(Presentation.agentStatus(Agent(kind: AgentKind("codex"), state: .idle))?.text == "Codex · 空闲")
        #expect(Presentation.agentStatus(Agent(kind: AgentKind("new"), state: .blocked))?.text == "new · 等你回答")
        #expect(Presentation.agentStatus(Agent(kind: AgentKind("x"), state: .unknown("z"))) == nil)
    }

    @Test func sizeOwners() {
        #expect(Presentation.sizeOwner("Ethan 的 MacBook") == "尺寸跟随 Ethan 的 MacBook")
        #expect(Presentation.sizeOwner(nil) == "尺寸无人控制")
        #expect(Presentation.sizeOwnership(.mine) == "尺寸跟随本机")
    }
}
