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
        model.handle(.state(.connected(hostName: "homelab", address: nil)))
        model.handle(.ready(generation: 1))
        #expect(link.sent == [.listSessions, .layout(req: 0)])
        model.screenOpened(sessionB)
        model.handle(.message(.sessionList([info(sessionA, title: "a"), info(sessionB, title: "b")])))
        #expect(model.sessions.count == 2)
        #expect(model.loaded)
        // 终端页开着的会话不发只看状态的 `Attach`，免得换掉终端页的订阅。
        #expect(attaches(link.sent) == [.attach(id: sessionA, size: nil, mode: .metaOnly)])
        // 再列一次不重复发。
        link.clearSent()
        model.handle(.message(.sessionList([info(sessionA, title: "a"), info(sessionB, title: "b")])))
        #expect(attaches(link.sent).isEmpty)
    }

    /// 停了订阅收不到断开的状态：`stop` 自己当作断开，回到前台、还没连上时不往没有的连接上发请求。
    @Test func stoppingCountsAsDisconnected() {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.state(.connected(hostName: "homelab", address: nil)))
        model.handle(.ready(generation: 1))
        model.handle(.message(.sessionList([info(sessionA, title: "a")])))
        model.stop()
        link.clearSent()
        #expect(!model.linkState.isConnected)
        model.refresh()
        model.screenClosed(sessionA)
        #expect(link.sent.isEmpty)
    }

    /// `Resync` 后重新只看状态被宿主以积压拒掉：下一次列会话时再订；终端页开着的会话不因此多发。
    @Test func aBackedUpRewatchIsRetriedOnTheNextList() {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.state(.connected(hostName: "homelab", address: nil)))
        model.handle(.ready(generation: 1))
        model.screenOpened(sessionB)
        model.handle(.message(.sessionList([info(sessionA, title: "a"), info(sessionB, title: "b")])))
        model.handle(.message(.resync(id: sessionA, reason: "slow")))
        let backedUp = HostMsg.backedUpPrefix + "; read first"
        model.handle(.message(.error(req: nil, id: sessionA, message: backedUp)))
        model.handle(.message(.error(req: nil, id: sessionB, message: backedUp)))
        link.clearSent()
        model.handle(.message(.sessionList([info(sessionA, title: "a"), info(sessionB, title: "b")])))
        #expect(attaches(link.sent) == [.attach(id: sessionA, size: nil, mode: .metaOnly)])
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
        model.handle(.state(.connected(hostName: "homelab", address: nil)))
        model.handle(.ready(generation: 1))
        model.screenOpened(sessionA)
        model.handle(.message(.sessionList([info(sessionA, title: "a")])))
        link.clearSent()
        model.screenClosed(sessionA)
        #expect(link.sent == [.attach(id: sessionA, size: nil, mode: .metaOnly)])
    }

    /// 新开会话先请电脑上的 app 在窗口里开一个不抢焦点的新标签，电脑上也看得到。
    @Test func spawnOpensATabOnTheComputer() async {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        link.clearSent()
        await model.spawn()
        #expect(model.isSpawning)
        guard case .open(let req, .tab, nil, nil, false)? = link.sent.first else {
            Issue.record("expected an open, got \(link.sent)")
            return
        }
        var spawned: [SessionId] = []
        model.onSpawned = { spawned.append($0) }
        model.handle(.message(.opened(req: req &+ 7, id: sessionB)))
        #expect(spawned.isEmpty)
        model.handle(.message(.opened(req: req, id: sessionA)))
        #expect(spawned == [sessionA])
        #expect(!model.isSpawning)
    }

    /// 等 `Opened` 时断了（断线等重连、进后台回来先是 `idle`）：回话不会来了，重连后还能再开。
    @Test(arguments: [LinkState.waiting(reason: "断了", retryAt: .now), .idle])
    func disconnectingWhileSpawningLetsYouSpawnAgain(_ lost: LinkState) async throws {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        await model.spawn()
        guard case .open(let oldReq, _, _, _, _)? = link.sent.last else {
            Issue.record("expected an open, got \(link.sent)")
            return
        }
        model.handle(.state(lost))
        #expect(!model.isSpawning)
        var spawned: [SessionId] = []
        model.onSpawned = { spawned.append($0) }
        model.handle(.message(.opened(req: oldReq, id: sessionB)))
        #expect(spawned.isEmpty)

        model.handle(.ready(generation: 2))
        link.clearSent()
        await model.spawn()
        #expect(model.isSpawning)
        guard case .open(let req, _, _, _, _)? = link.sent.last else {
            Issue.record("expected an open, got \(link.sent)")
            return
        }
        model.handle(.message(.opened(req: req, id: sessionA)))
        #expect(spawned == [sessionA])
        #expect(!model.isSpawning)
    }

    /// 电脑上的 app 没开着窗口时退回自己开一个后台会话。
    @Test func spawnFallsBackWithoutADesktopWindow() async {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        link.clearSent()
        await model.spawn()
        guard case .open(let openReq, _, _, _, _)? = link.sent.first else {
            Issue.record("expected an open, got \(link.sent)")
            return
        }
        link.clearSent()
        model.handle(.message(.error(req: openReq, id: nil, message: "there is no runode window to do this in")))
        #expect(model.errorMessage == nil)
        #expect(model.isSpawning)
        guard case .spawn(let req, let size, nil, .detect, true)? = link.sent.first else {
            Issue.record("expected a spawn, got \(link.sent)")
            return
        }
        #expect(req != openReq)
        #expect(size == model.spawnSize)
        var spawned: [SessionId] = []
        model.onSpawned = { spawned.append($0) }
        model.handle(.message(.spawned(req: openReq, id: sessionB)))
        #expect(spawned.isEmpty)
        model.handle(.message(.spawned(req: req, id: sessionA)))
        #expect(spawned == [sessionA])
        #expect(!model.isSpawning)
    }

    @Test func spawnErrorsAreShown() async {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        link.clearSent()
        await model.spawn()
        guard case .open(let openReq, _, _, _, _)? = link.sent.first else {
            Issue.record("expected an open, got \(link.sent)")
            return
        }
        model.handle(.message(.error(req: openReq, id: nil, message: "no window")))
        guard case .spawn(let req, _, _, _, _)? = link.sent.last else {
            Issue.record("expected a spawn, got \(link.sent)")
            return
        }
        model.handle(.message(.error(req: req, id: nil, message: "no shell")))
        #expect(model.errorMessage?.contains("no shell") == true)
        #expect(!model.isSpawning)
    }

    /// 在某个工作区里新开终端：挨着那个工作区的 `anchor` 开新标签，开好后连列表带布局再要一次。
    @Test func spawnInAWorkspaceOpensBesideItsAnchor() async {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        link.clearSent()
        await model.spawn(near: sessionB)
        guard case .open(let req, .tab, sessionB?, nil, false)? = link.sent.first else {
            Issue.record("expected an open beside sessionB, got \(link.sent)")
            return
        }
        link.clearSent()
        model.handle(.message(.opened(req: req, id: sessionA)))
        #expect(link.sent == [.listSessions, .layout(req: 0)])
    }

    /// 新建工作区：选目录的页面收起，`OpenWorkspace` 开好后和新开终端一样打开它的终端。
    @Test func createWorkspaceOpensItsFirstTerminal() async {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        model.handle(.message(.layout(req: 0, windows: [WindowLayout(index: 1, workspaces: [])])))
        model.beginNewWorkspace()
        #expect(model.directoryPicker != nil)
        link.clearSent()
        await model.createWorkspace(at: "/Users/ethan/dev")
        #expect(model.directoryPicker == nil)
        #expect(model.isSpawning)
        guard case .openWorkspace(let req, "/Users/ethan/dev", false, nil)? = link.sent.first else {
            Issue.record("expected an open_workspace, got \(link.sent)")
            return
        }
        var spawned: [SessionId] = []
        model.onSpawned = { spawned.append($0) }
        model.handle(.message(.opened(req: req, id: sessionA)))
        #expect(spawned == [sessionA])
        #expect(!model.isSpawning)
    }

    /// 新建工作区没成时不退回 `Spawn`，直接提示。
    @Test func createWorkspaceErrorsAreShown() async {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        model.handle(.message(.layout(req: 0, windows: [WindowLayout(index: 1, workspaces: [])])))
        link.clearSent()
        await model.createWorkspace(at: "/nope")
        guard case .openWorkspace(let req, _, _, _)? = link.sent.first else {
            Issue.record("expected an open_workspace, got \(link.sent)")
            return
        }
        link.clearSent()
        model.handle(.message(.error(req: req, id: nil, message: "not a directory: /nope")))
        #expect(link.sent.isEmpty)
        #expect(model.errorMessage == "建不了工作区：not a directory: /nope")
        #expect(!model.isSpawning)
    }

    /// 等 `OpenWorkspace` 时来了不带编号的 `Error`：不是给新建工作区的，照样等着，随后的 `Opened` 仍然
    /// 打开新终端。
    @Test func unnumberedErrorsAreNotTakenForTheWorkspaceReply() async {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        model.handle(.message(.layout(req: 0, windows: [WindowLayout(index: 1, workspaces: [])])))
        link.clearSent()
        await model.createWorkspace(at: "/Users/ethan")
        guard case .openWorkspace(let req, _, _, _)? = link.sent.first else {
            Issue.record("expected an open_workspace, got \(link.sent)")
            return
        }
        var spawned: [SessionId] = []
        model.onSpawned = { spawned.append($0) }
        model.handle(.message(.error(req: nil, id: nil, message: "unknown message")))
        #expect(model.isSpawning)
        #expect(model.errorMessage == nil)
        model.handle(.message(.opened(req: req, id: sessionB)))
        #expect(spawned == [sessionB])
        #expect(!model.isSpawning)
    }

    /// 新建工作区时填的名字去掉首尾空白后带上；只有空白时当没填。
    @Test func createWorkspaceCarriesTheName() async {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        model.handle(.message(.layout(req: 0, windows: [WindowLayout(index: 1, workspaces: [])])))
        link.clearSent()
        await model.createWorkspace(at: "/Users/ethan/dev", name: "  后端 ")
        guard case .openWorkspace(let req, "/Users/ethan/dev", false, "后端")? = link.sent.first else {
            Issue.record("expected a named open_workspace, got \(link.sent)")
            return
        }
        model.handle(.message(.opened(req: req, id: sessionA)))
        link.clearSent()
        await model.createWorkspace(at: "/Users/ethan/dev", name: "  ")
        guard case .openWorkspace(_, _, _, nil)? = link.sent.first else {
            Issue.record("expected an unnamed open_workspace, got \(link.sent)")
            return
        }
    }

    /// 电脑上关掉了最后一个标签的工作区还在：列成一个空节。它没有 `anchor`，在里面新开终端是对它的目录
    /// 发 `OpenWorkspace`。
    @Test func emptyWorkspacesGetASection() async {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        model.handle(.message(.sessionList([info(sessionA, title: "a")])))
        let tab = TabLayout(index: 1, active: true, panes: [PaneLayout(index: 1, id: sessionA, focused: true)])
        model.handle(
            .message(
                .layout(
                    req: 0,
                    windows: [
                        WindowLayout(
                            index: 1,
                            workspaces: [
                                WorkspaceLayout(index: 1, name: "a", dir: "/Users/ethan/a", tabs: [tab]),
                                WorkspaceLayout(index: 2, name: "空", dir: "/Users/ethan/empty", tabs: []),
                            ])
                    ])))
        let sections = model.sections
        #expect(sections.map(\.id) == [.workspace(window: 1, index: 1), .workspace(window: 1, index: 2)])
        let empty = sections[1]
        #expect(empty.sessions.isEmpty)
        #expect(empty.anchor == nil)
        #expect(model.canSpawn(in: empty))
        #expect(model.spawnChoices == sections)
        link.clearSent()
        await model.spawn(in: empty)
        guard case .openWorkspace(let req, "/Users/ethan/empty", false, nil)? = link.sent.first else {
            Issue.record("expected an open_workspace in the empty workspace, got \(link.sent)")
            return
        }
        var spawned: [SessionId] = []
        model.onSpawned = { spawned.append($0) }
        model.handle(.message(.opened(req: req, id: sessionB)))
        #expect(spawned == [sessionB])
        // 有 `anchor` 的照旧挨着它开新标签。
        link.clearSent()
        await model.spawn(in: sections[0])
        guard case .open(_, .tab, sessionA?, nil, false)? = link.sent.first else {
            Issue.record("expected an open beside sessionA, got \(link.sent)")
            return
        }
    }

    /// 只有一个工作区时新开终端不用挑；后台那一节不算工作区。
    @Test func oneWorkspaceOffersNoChoice() {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        model.handle(.message(.sessionList([info(sessionA, title: "a"), info(sessionB, title: "b")])))
        let tab = TabLayout(index: 1, active: true, panes: [PaneLayout(index: 1, id: sessionA, focused: true)])
        model.handle(
            .message(
                .layout(
                    req: 0,
                    windows: [
                        WindowLayout(
                            index: 1, workspaces: [WorkspaceLayout(index: 1, name: "a", dir: "/Users/ethan/a", tabs: [tab])])
                    ])))
        #expect(model.sections.map(\.id) == [.workspace(window: 1, index: 1), .background])
        #expect(model.spawnChoices.isEmpty)
    }

    /// 同一个标签里分了屏的会话互为兄弟；独自一个标签、不在窗口里的没有。
    @Test func panesSharingATabListEachOther() {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        model.handle(.message(.sessionList([info(sessionA, title: "a"), info(sessionB, title: "b")])))
        #expect(model.panes(sharingTabWith: sessionA).isEmpty)
        let split = TabLayout(
            index: 1, active: true,
            panes: [
                PaneLayout(index: 1, id: sessionA, rect: PaneRect(x: 0, y: 0, width: 500, height: 1000), focused: true),
                PaneLayout(index: 2, id: sessionB, rect: PaneRect(x: 500, y: 0, width: 500, height: 1000)),
            ])
        model.handle(
            .message(.layout(req: 0, windows: [WindowLayout(index: 1, workspaces: [WorkspaceLayout(index: 1, tabs: [split])])])))
        #expect(model.panes(sharingTabWith: sessionB).map(\.id) == [sessionA, sessionB])
        #expect(model.panes(sharingTabWith: sessionB).last?.rect?.x == 500)
        // 电脑上刚分出来、列表里还没有的会话：先不列，补要一次列表，列表到了再列。
        model.handle(.message(.sessionList([info(sessionA, title: "a")])))
        link.clearSent()
        model.handle(
            .message(.layout(req: 0, windows: [WindowLayout(index: 1, workspaces: [WorkspaceLayout(index: 1, tabs: [split])])])))
        #expect(model.panes(sharingTabWith: sessionA).isEmpty)
        #expect(link.sent.contains(.listSessions))
        model.handle(.message(.sessionList([info(sessionA, title: "a"), info(sessionB, title: "b")])))
        #expect(model.panes(sharingTabWith: sessionA).map(\.id) == [sessionA, sessionB])
        // 电脑推来布局变了：马上重新要布局。
        link.clearSent()
        model.handle(.message(.layoutChanged))
        #expect(link.sent == [.layout(req: 0)])
        let alone = TabLayout(index: 1, active: true, panes: [PaneLayout(index: 1, id: sessionA, focused: true)])
        model.handle(
            .message(.layout(req: 0, windows: [WindowLayout(index: 1, workspaces: [WorkspaceLayout(index: 1, tabs: [alone])])])))
        #expect(model.panes(sharingTabWith: sessionA).isEmpty)
    }

    /// 给工作区改名：按布局里的序号发 `RenameWorkspace`，办好后重新要布局；名字只有空白时不发。
    @Test func renamingAWorkspaceRefreshesTheLayout() async {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        let section = SessionSection(id: .workspace(window: 2, index: 3), sessions: [])
        link.clearSent()
        await model.rename(section, to: "   ")
        await model.rename(SessionSection(id: .background, sessions: []), to: "后台")
        #expect(link.sent.isEmpty)
        await model.rename(section, to: " 前端 ")
        guard case .renameWorkspace(let req, 2, 3, "前端")? = link.sent.first else {
            Issue.record("expected a rename_workspace, got \(link.sent)")
            return
        }
        link.clearSent()
        model.handle(.message(.done(req: req &+ 7)))
        #expect(link.sent.isEmpty)
        model.handle(.message(.done(req: req)))
        #expect(link.sent == [.listSessions, .layout(req: 0)])
        #expect(model.errorMessage == nil)
    }

    @Test func renameErrorsAreShown() async {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        let section = SessionSection(id: .workspace(window: 1, index: 9), sessions: [])
        link.clearSent()
        await model.rename(section, to: "x")
        guard case .renameWorkspace(let req, _, _, _)? = link.sent.first else {
            Issue.record("expected a rename_workspace, got \(link.sent)")
            return
        }
        model.handle(.message(.error(req: req, id: nil, message: "no workspace 9")))
        #expect(model.errorMessage == "改不了名：no workspace 9")
    }

    /// 电脑上的 app 没开着窗口时建不了工作区：在选好的目录里开一个后台会话，开好后照样打开它的终端。
    @Test func createWorkspaceWithoutADesktopWindowSpawnsInTheDirectory() async {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        #expect(!model.hasDesktopWindow)
        model.beginNewWorkspace()
        link.clearSent()
        await model.createWorkspace(at: "/Users/ethan/dev")
        #expect(model.directoryPicker == nil)
        #expect(model.isSpawning)
        guard case .spawn(let req, let size, "/Users/ethan/dev", .detect, true)? = link.sent.first else {
            Issue.record("expected a spawn in the directory, got \(link.sent)")
            return
        }
        #expect(size == model.spawnSize)
        var spawned: [SessionId] = []
        model.onSpawned = { spawned.append($0) }
        model.handle(.message(.spawned(req: req, id: sessionA)))
        #expect(spawned == [sessionA])
        #expect(!model.isSpawning)
    }

    @Test func killRemovesTheSession() {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        model.handle(.message(.sessionList([info(sessionA, title: "a")])))
        model.killTarget = sessionA
        #expect(model.killTarget != nil)
        link.clearSent()
        model.kill(sessionA)
        #expect(link.sent == [.kill(id: sessionA)])
        #expect(model.sessions.isEmpty)
    }

    @Test func killWhileDisconnectedKeepsTheSession() {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        model.handle(.message(.sessionList([info(sessionA, title: "a")])))
        model.handle(.state(.waiting(reason: "断了", retryAt: .now)))
        link.clearSent()
        model.kill(sessionA)
        #expect(link.sent.isEmpty)
        #expect(model.sessions.map(\.id) == [sessionA])
        #expect(model.errorMessage == "未连接，结束不了会话")
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
        #expect(link.sent.first == .listSessions)
        #expect(attaches(link.sent) == [.attach(id: sessionA, size: nil, mode: .metaOnly)])
    }
}

/// 发出去的消息里的 `Attach`。
func attaches(_ sent: [ClientMsg]) -> [ClientMsg] {
    sent.filter {
        if case .attach = $0 { return true }
        return false
    }
}

extension LinkState {
    var retryDate: Date? {
        if case .waiting(_, let date, _) = self { return date }
        return nil
    }
}

@MainActor
@Suite struct MachineListModelTests {
    @Test func addRenameDelete() async throws {
        let store = MemoryMachineStore()
        let keys = MemoryDeviceKeyStore()
        let model = MachineListModel(store: store, keyStore: keys)
        var deleted: [UUID] = []
        model.willDelete = {
            deleted.append($0)
            return true
        }
        let machine = machineRecord()
        try keys.save(StoredDeviceKey(kind: .software, data: Data([1])), for: machine.id)
        try await model.add(machine)
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

    @Test func repairingTheSameMachineReplacesTheOldRecord() async throws {
        let store = MemoryMachineStore()
        let keys = MemoryDeviceKeyStore()
        let model = MachineListModel(store: store, keyStore: keys)
        let old = machineRecord(name: "旧")
        try keys.save(StoredDeviceKey(kind: .software, data: Data([1])), for: old.id)
        try await model.add(old)
        let new = machineRecord(name: "新")
        try await model.add(new)
        #expect(model.machines.map(\.name) == ["新"])
        #expect(try keys.key(for: old.id) == nil)
    }

    /// 重新配对的记录存不下：旧的配对连同私钥留着，新存的私钥丢掉，错误交给配对页。
    @Test func aFailedRepairKeepsTheOldPairing() async throws {
        let store = UnwritableMachineStore()
        let keys = MemoryDeviceKeyStore()
        let model = MachineListModel(store: store, keyStore: keys)
        var deleted: [UUID] = []
        model.willDelete = {
            deleted.append($0)
            return true
        }
        let old = machineRecord(name: "旧")
        await store.memory.upsert(old)
        try keys.save(StoredDeviceKey(kind: .software, data: Data([1])), for: old.id)
        await model.load()
        let new = machineRecord(name: "新")
        try keys.save(StoredDeviceKey(kind: .software, data: Data([2])), for: new.id)
        await #expect(throws: (any Error).self) { try await model.add(new) }
        #expect(model.machines.map(\.name) == ["旧"])
        #expect(deleted.isEmpty)
        #expect(try keys.key(for: old.id) != nil)
        #expect(try keys.key(for: new.id) == nil)
    }

    /// 电脑那边没注销好推送：记录和私钥留着、记下删掉的时刻，列表里不再显示；注销好了（`purge`）才真删。
    @Test func aMachineNotUnregisteredIsHiddenUntilPurged() async throws {
        let store = MemoryMachineStore()
        let keys = MemoryDeviceKeyStore()
        let model = MachineListModel(store: store, keyStore: keys)
        model.willDelete = { _ in false }
        let machine = machineRecord()
        try keys.save(StoredDeviceKey(kind: .software, data: Data([1])), for: machine.id)
        try await model.add(machine)
        await model.delete(machine.id)
        #expect(model.machines.isEmpty)
        #expect(model.leaving.map(\.id) == [machine.id])
        #expect(await store.all().first?.deletedAt != nil)
        #expect(try keys.key(for: machine.id) != nil)
        // 重新读一遍（App 重启）也还藏着。
        await model.load()
        #expect(model.machines.isEmpty)
        await model.purge(machine.id)
        #expect(model.leaving.isEmpty)
        #expect(await store.all().isEmpty)
        #expect(try keys.key(for: machine.id) == nil)
    }

    /// 删掉一个月还没注销成的，读列表时真删掉。
    @Test func aMachineLeftForAMonthIsPurged() async throws {
        let store = MemoryMachineStore()
        let keys = MemoryDeviceKeyStore()
        let model = MachineListModel(store: store, keyStore: keys)
        var old = machineRecord()
        old.deletedAt = .now - MachineListModel.leavingLimit - 60
        var recent = machineRecord()
        recent.deletedAt = .now - 60
        await store.upsert(old)
        await store.upsert(recent)
        try keys.save(StoredDeviceKey(kind: .software, data: Data([1])), for: old.id)
        await model.load()
        #expect(model.leaving.map(\.id) == [recent.id])
        #expect(await store.all().map(\.id) == [recent.id])
        #expect(try keys.key(for: old.id) == nil)
    }

    /// 删不掉记录：电脑和私钥都留着，照样能连，错误交给设置页。
    @Test func aFailedDeleteKeepsTheMachine() async throws {
        let store = UnwritableMachineStore()
        let keys = MemoryDeviceKeyStore()
        let model = MachineListModel(store: store, keyStore: keys)
        let machine = machineRecord()
        await store.memory.upsert(machine)
        try keys.save(StoredDeviceKey(kind: .software, data: Data([1])), for: machine.id)
        await model.load()
        await model.delete(machine.id)
        #expect(model.machines.map(\.id) == [machine.id])
        #expect(try keys.key(for: machine.id) != nil)
        #expect(model.errorMessage?.hasPrefix("删不掉这台电脑") == true)
    }
}

/// 读得出、写不进的电脑列表。
actor UnwritableMachineStore: MachineStore {
    struct WriteFailed: Error {}
    let memory = MemoryMachineStore()

    func all() async -> [MachineRecord] { await memory.all() }
    func upsert(_ machine: MachineRecord) throws { throw WriteFailed() }
    func updateLastAddress(id: UUID, _ address: String) throws { throw WriteFailed() }
    func remove(id: UUID) throws { throw WriteFailed() }
}

@MainActor
@Suite struct PairingModelTests {
    let fingerprint = Base64URL.encode(Data(repeating: 1, count: 32))
    let secret = Base64URL.encode(Data(repeating: 2, count: 32))

    func link(exp: Int = 1_900_000_000) -> String {
        "runode://pair?v=1&name=homelab&fp=\(fingerprint)&secret=\(secret)&port=7866&addr=192.168.1.20&exp=\(exp)"
    }

    @Test func pairsAndHandsTheMachineOver() async {
        var paired: [MachineRecord] = []
        let machine = machineRecord()
        let model = PairingModel(
            pairing: FakePairing { invitation in
                #expect(invitation.hostName == "homelab")
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

    /// 配对前给用户看的电脑名和地址从输入框里的链接解析出来；不是配对链接时没有。
    @Test func theInvitationIsShownBeforePairing() {
        let model = PairingModel(pairing: FakePairing { _ in machineRecord() }, deviceName: "x", onPaired: { _ in })
        #expect(model.invitation == nil)
        model.linkText = link()
        #expect(model.invitation?.hostName == "homelab")
        #expect(model.invitation?.addresses == ["192.168.1.20"])
        #expect(model.invitation?.port == 7866)
        #expect(model.phase == .idle)
        model.linkText = "https://example.com"
        #expect(model.invitation == nil)
    }

    @Test func badAndExpiredLinksFailWithoutPairing() async {
        let model = PairingModel(
            pairing: FakePairing { _ in
                Issue.record("should not pair")
                return machineRecord()
            }, deviceName: "x", now: { Date(timeIntervalSince1970: 1_950_000_000) }, onPaired: { _ in })
        model.linkText = "https://example.com"
        await model.submitLink()
        #expect(model.phase == .failed("这不是 Runode 的配对链接"))
        model.linkText = link()
        await model.submitLink()
        #expect(model.phase == .failed("二维码已经过期，请在电脑上重新生成"))
    }

    /// 配好了却存不下：不算配好，不给「完成」。
    @Test func aMachineThatCannotBeSavedIsNotPaired() async {
        let model = PairingModel(
            pairing: FakePairing { _ in machineRecord() }, deviceName: "x",
            now: { Date(timeIntervalSince1970: 1_800_000_000) },
            onPaired: { _ in throw UnwritableMachineStore.WriteFailed() })
        await model.scanned(link())
        guard case .failed(let message) = model.phase else {
            Issue.record("expected a failure, got \(model.phase)")
            return
        }
        #expect(message.hasPrefix("存不下这台电脑"))
    }

    /// 配对页关掉以后配好的不存，为它存的私钥也删掉。
    @Test func closingThePageDropsThePairing() async throws {
        var paired: [MachineRecord] = []
        let gate = AsyncStream<Void>.makeStream()
        let gated = GatedPairing(gate: gate.stream)
        let model = PairingModel(
            pairing: gated, deviceName: "x",
            now: { Date(timeIntervalSince1970: 1_800_000_000) }, onPaired: { paired.append($0) })
        let pairing = Task { await model.scanned(link()) }
        #expect(await eventually { model.isBusy })
        model.cancel()
        gate.continuation.yield()
        await pairing.value
        #expect(paired.isEmpty)
        #expect(try gated.keys.key(for: gated.machine.id) == nil, "the key saved for the dropped pairing is gone")
        if case .paired = model.phase { Issue.record("a closed page should not reach paired") }
        await model.scanned(link())
        #expect(paired.isEmpty, "a closed page does not pair again")
    }

    /// 没关页面时配好的照常存，私钥留着。
    @Test func aKeptPairingKeepsItsKey() async throws {
        var paired: [MachineRecord] = []
        let gate = AsyncStream<Void>.makeStream()
        let gated = GatedPairing(gate: gate.stream)
        let model = PairingModel(
            pairing: gated, deviceName: "x",
            now: { Date(timeIntervalSince1970: 1_800_000_000) }, onPaired: { paired.append($0) })
        gate.continuation.yield()
        await model.scanned(link())
        #expect(paired.map(\.id) == [gated.machine.id])
        #expect(try gated.keys.key(for: gated.machine.id) != nil)
    }

    @Test func rejectionsAreExplained() async {
        let model = PairingModel(
            pairing: FakePairing { _ in throw LinkFailure.rejected(.pairingInvalid) }, deviceName: "x",
            now: { Date(timeIntervalSince1970: 1_800_000_000) }, onPaired: { _ in })
        await model.scanned(link())
        #expect(model.phase == .failed("配对口令不对、已过期或已经用过，请在电脑上重新生成二维码"))
        model.reset()
        #expect(model.phase == .idle)
    }
}

@MainActor
@Suite struct AppModelTests {
    /// 配对过的电脑一读进来就连上，进出它的页面不断开；终端页退出去就关掉。
    @Test func pairedMachinesStayConnected() async throws {
        let store = MemoryMachineStore()
        let machine = machineRecord()
        await store.upsert(machine)
        let link = FakeLink()
        let app = AppModel(
            dependencies: AppDependencies(
                store: store, keyStore: MemoryDeviceKeyStore(),
                pairing: FakePairing { _ in machine }, makeLink: { _ in link }, deviceName: "测试 iPhone"))
        await app.machineList.load()
        #expect(await eventually { link.starts == 1 })
        let list = try #require(app.sessionList(for: machine.id))
        app.path = [.machine(machine.id), .terminal(machine: machine.id, session: sessionA)]
        let terminal = try #require(app.terminal(machine: machine.id, session: sessionA))
        #expect(app.terminal(machine: machine.id, session: sessionA) === terminal)
        #expect(list.link === link)
        app.path = []
        #expect(app.terminal(machine: machine.id, session: sessionA) !== terminal)
        #expect(app.sessionList(for: machine.id) === list)
        try await Task.sleep(for: .milliseconds(20))
        #expect(link.stops == 0)
        // 删掉这台电脑才断开。
        await app.machineList.delete(machine.id)
        #expect(await eventually { link.stops == 1 })
    }

    /// 显示的分屏结束了：切回同一个标签里上次看过、还没结束的分屏；看过的都结束了时切到第一个还没结束的。
    @Test func anEndedPaneReturnsToTheLastSeenPaneOfItsTab() async throws {
        let store = MemoryMachineStore()
        let machine = machineRecord()
        await store.upsert(machine)
        let link = FakeLink()
        let app = AppModel(
            dependencies: AppDependencies(
                store: store, keyStore: MemoryDeviceKeyStore(),
                pairing: FakePairing { _ in machine }, makeLink: { _ in link }, deviceName: "测试 iPhone"))
        await app.machineList.load()
        let list = try #require(app.sessionList(for: machine.id))
        func info(_ id: SessionId, exited: Bool = false) -> SessionInfo {
            SessionInfo(id: id, size: smallGrid, meta: SessionMeta(title: "zsh"), exited: exited)
        }
        list.handle(.ready(generation: 1))
        list.handle(.message(.sessionList([info(sessionA), info(sessionB), info(sessionC)])))
        app.path = [.machine(machine.id), .terminal(machine: machine.id, session: sessionA)]
        // 在 A 上跑命令，开出来的 C 换掉了这一页。
        app.openTerminal(machine: machine.id, session: sessionC)
        app.leaveEndedPane(machine: machine.id, session: sessionC, tabMates: [sessionB, sessionA, sessionC])
        #expect(app.shownSession(machine: machine.id, session: sessionC) == sessionA)

        list.handle(.message(.sessionList([info(sessionA, exited: true), info(sessionB), info(sessionC)])))
        app.leaveEndedPane(machine: machine.id, session: sessionA, tabMates: [sessionB, sessionA, sessionC])
        #expect(app.shownSession(machine: machine.id, session: sessionC) == sessionC)
        // 标签里只剩它自己时不动。
        app.leaveEndedPane(machine: machine.id, session: sessionC, tabMates: [sessionC])
        #expect(app.shownSession(machine: machine.id, session: sessionC) == sessionC)
    }

    /// 切到同一个标签里的另一个分屏：导航栈不动，栈顶的终端页改显示新会话，旧终端关掉。
    @Test func switchingPanesReplacesTheTerminalPage() async throws {
        let store = MemoryMachineStore()
        let machine = machineRecord()
        await store.upsert(machine)
        let link = FakeLink()
        let app = AppModel(
            dependencies: AppDependencies(
                store: store, keyStore: MemoryDeviceKeyStore(),
                pairing: FakePairing { _ in machine }, makeLink: { _ in link }, deviceName: "测试 iPhone"))
        await app.machineList.load()
        app.path = [.machine(machine.id), .terminal(machine: machine.id, session: sessionA)]
        let first = try #require(app.terminal(machine: machine.id, session: sessionA))
        first.setKeyboardVisible(true)
        app.switchTerminal(machine: machine.id, to: sessionB)
        #expect(app.path == [.machine(machine.id), .terminal(machine: machine.id, session: sessionA)])
        #expect(app.shownSession(machine: machine.id, session: sessionA) == sessionB)
        #expect(app.terminal(machine: machine.id, session: sessionA) !== first)
        #expect(app.terminal(machine: machine.id, session: sessionB)?.keyboardVisible == true)
        app.switchTerminal(machine: machine.id, to: sessionA)
        #expect(app.shownSession(machine: machine.id, session: sessionA) == sessionA)
        // 不在终端页上时不动。
        app.path = [.machine(machine.id)]
        app.switchTerminal(machine: machine.id, to: sessionA)
        #expect(app.path == [.machine(machine.id)])
    }

    @Test func backgroundDisconnectsAndForegroundReconnects() async throws {
        let store = MemoryMachineStore()
        let machine = machineRecord()
        await store.upsert(machine)
        let link = FakeLink()
        let app = AppModel(
            dependencies: AppDependencies(
                store: store, keyStore: MemoryDeviceKeyStore(),
                pairing: FakePairing { _ in machine }, makeLink: { _ in link }, deviceName: "测试 iPhone"))
        await app.machineList.load()
        #expect(await eventually { link.starts == 1 })
        app.setActive(false)
        #expect(await eventually { link.stops == 1 })
        app.setActive(true)
        #expect(await eventually { link.starts == 2 })
    }

    /// 导航栈里有页面的电脑算在看，连不上时重试得勤；回到首页时都不算。
    @Test func machinesOnThePathAreFocused() async throws {
        let store = MemoryMachineStore()
        let machine = machineRecord()
        let other = machineRecord(name: "Mac mini", fingerprintByte: 2)
        await store.upsert(machine)
        await store.upsert(other)
        let link = FakeLink()
        let otherLink = FakeLink()
        let app = AppModel(
            dependencies: AppDependencies(
                store: store, keyStore: MemoryDeviceKeyStore(),
                pairing: FakePairing { _ in machine }, makeLink: { $0.id == machine.id ? link : otherLink },
                deviceName: "测试 iPhone"))
        await app.machineList.load()
        #expect(link.focused.last == false)
        #expect(otherLink.focused.last == false)
        app.openTerminal(machine: machine.id, session: sessionA)
        #expect(link.focused.last == true)
        #expect(otherLink.focused.last == false)
        app.path = []
        #expect(link.focused.last == false)
    }

    @Test func deletingAMachineLeavesItsPages() async throws {
        let store = MemoryMachineStore()
        let machine = machineRecord()
        await store.upsert(machine)
        let link = FakeLink()
        let app = AppModel(
            dependencies: AppDependencies(
                store: store, keyStore: MemoryDeviceKeyStore(),
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

    @Test func agentLogosMatchTheBundledImages() throws {
        let catalog = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .appending(path: "Sources/RunodeFeatures/Resources/AgentLogos.xcassets")
        let images = Set(
            try FileManager.default.contentsOfDirectory(atPath: catalog.path)
                .filter { $0.hasSuffix(".imageset") }.map { String($0.dropLast(".imageset".count)) })
        let labels = [
            "pi", "claude", "codex", "gemini", "cursor", "devin", "antigravity", "cline", "mastracode", "open_code",
            "github_copilot", "kimi", "kiro", "amp", "grok", "hermes", "kilo", "qodercli", "qwen", "goose", "junie",
            "open_hands", "trae", "code_buddy", "mistral_vibe", "jules", "omp",
        ]
        let mapped = Set(labels.compactMap { Presentation.agentLogoAsset(AgentKind($0)) })
        // `prompt` 是 shell 那块的提示符，不是 agent 的 logo。
        #expect(mapped == images.subtracting(["prompt"]))
        // 符号链接指着的 SVG 都在。
        for image in images {
            #expect(FileManager.default.fileExists(atPath: catalog.appending(path: "\(image).imageset/\(image).svg").path))
        }
        #expect(Presentation.agentLogoAsset(AgentKind("aider")) == nil)
    }

    @Test func sessionDirectoriesSkipTheWorkspaceDirectory() {
        let ws = "/Users/ethan/dev/runode"
        #expect(Presentation.sessionDirectory(ws, in: ws) == nil)
        #expect(Presentation.sessionDirectory(ws, in: ws + "/") == nil)
        #expect(Presentation.sessionDirectory(ws + "/apps/ios", in: ws) == "./apps/ios")
        #expect(Presentation.sessionDirectory("/Users/ethan/dev/runode-old", in: ws) == "~/dev/runode-old")
        #expect(Presentation.sessionDirectory("/tmp", in: nil) == "/tmp")
        #expect(Presentation.sessionDirectory(nil, in: ws) == nil)
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

@MainActor
@Suite struct DirectoryPickerTests {
    let link = FakeLink()

    func listed(_ path: String, _ dirs: [String]) async -> DirectoryPickerModel {
        let picker = DirectoryPickerModel(link: link)
        await picker.load(nil)
        guard case .listDirs(let req, nil)? = link.sent.last else {
            Issue.record("expected list_dirs for home, got \(link.sent)")
            return picker
        }
        #expect(picker.isLoading)
        #expect(picker.handle(.dirs(req: req, path: path, dirs: dirs, truncated: false)))
        return picker
    }

    @Test func startsAtHomeAndHidesDotDirectories() async {
        let picker = await listed("/Users/ethan", [".config", "dev", "中文"])
        #expect(!picker.isLoading)
        #expect(picker.path == "/Users/ethan")
        #expect(picker.visibleDirs == ["dev", "中文"])
        picker.showsHidden = true
        #expect(picker.visibleDirs == [".config", "dev", "中文"])
        #expect(picker.parent == "/Users")
    }

    @Test func entersAndGoesUp() async {
        let picker = await listed("/Users/ethan", ["dev"])
        await picker.enter("dev")
        #expect(link.sent.last.map { if case .listDirs(_, "/Users/ethan/dev") = $0 { true } else { false } } == true)
        await picker.goUp()
        #expect(link.sent.last.map { if case .listDirs(_, "/Users") = $0 { true } else { false } } == true)
    }

    /// 只认最后一次请求的回话：点得快时前面的回话丢掉。
    @Test func onlyTheLatestReplyCounts() async {
        let picker = await listed("/Users/ethan", ["a", "b"])
        await picker.enter("a")
        guard case .listDirs(let first, _)? = link.sent.last else { return }
        await picker.enter("b")
        guard case .listDirs(let second, _)? = link.sent.last else { return }
        #expect(!picker.handle(.dirs(req: first, path: "/Users/ethan/a", dirs: [], truncated: false)))
        #expect(picker.handle(.dirs(req: second, path: "/Users/ethan/b", dirs: ["x"], truncated: true)))
        #expect(picker.path == "/Users/ethan/b")
        #expect(picker.truncated)
    }

    @Test func errorsKeepTheLastListingAndCanRetry() async {
        let picker = await listed("/Users/ethan", ["secret"])
        await picker.enter("secret")
        guard case .listDirs(let req, let path)? = link.sent.last else { return }
        #expect(picker.handle(.error(req: req, id: nil, message: "permission denied")))
        #expect(picker.errorMessage == "permission denied")
        #expect(picker.path == "/Users/ethan")
        // 错误层盖着列表，`path` 还是上次的目录：这时不能在这里新建，收起提示回到那个目录后才能。
        #expect(!picker.canChoose)
        picker.dismissError()
        #expect(picker.canChoose)
        await picker.enter("secret")
        guard case .listDirs(let again, _)? = link.sent.last else {
            Issue.record("expected a listDirs, got \(link.sent)")
            return
        }
        #expect(picker.handle(.error(req: again, id: nil, message: "permission denied")))
        await picker.retry()
        #expect(link.sent.last.map { if case .listDirs(_, path) = $0 { true } else { false } } == true)
        #expect(picker.errorMessage == nil)
    }

    /// 连接断了：在等的回话不会来了，不再转圈、报出错；断着时不发请求，重新连上后能重试。
    @Test func losingTheConnectionStopsTheSpinner() async {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        model.beginNewWorkspace()
        let picker = try! #require(model.directoryPicker)
        await picker.load(nil)
        #expect(picker.isLoading)
        model.handle(.state(.waiting(reason: "断了", retryAt: .now)))
        #expect(!picker.isLoading)
        #expect(picker.errorMessage == "未连接")
        // 先前那次请求晚到的回话不认。
        guard case .listDirs(let req, nil)? = link.sent.last else {
            Issue.record("expected list_dirs for home, got \(link.sent)")
            return
        }
        #expect(!picker.handle(.dirs(req: req, path: "/Users/ethan", dirs: [], truncated: false)))
        link.clearSent()
        await picker.retry()
        #expect(link.sent.isEmpty)
        #expect(!picker.isLoading)
        #expect(picker.errorMessage == "未连接")
        model.handle(.ready(generation: 2))
        await picker.retry()
        #expect(link.sent.last.map { if case .listDirs(_, nil) = $0 { true } else { false } } == true)
        #expect(picker.isLoading)
        #expect(picker.errorMessage == nil)
    }

    /// 取请求编号时连接断了：不再发、不转圈，报未连接。
    @Test func aDisconnectWhileNumberingDoesNotSpin() async {
        let held = HeldRequestLink()
        let picker = DirectoryPickerModel(link: held)
        let loading = Task { await picker.load(nil) }
        #expect(await eventually { held.waiting == 1 })
        picker.connectionLost()
        held.release()
        await loading.value
        #expect(held.inner.sent.isEmpty)
        #expect(!picker.isLoading)
        #expect(picker.errorMessage == "未连接")
    }

    /// 不带编号的 `Error` 是同一条连接上别的请求的，不当成列目录的回话，接着等。
    @Test func unnumberedErrorsAreNotClaimed() async {
        let picker = DirectoryPickerModel(link: link)
        await picker.load(nil)
        guard case .listDirs(let req, nil)? = link.sent.last else {
            Issue.record("expected list_dirs for home, got \(link.sent)")
            return
        }
        #expect(!picker.handle(.error(req: nil, id: nil, message: "unknown message")))
        #expect(picker.isLoading)
        #expect(picker.errorMessage == nil)
        #expect(picker.handle(.dirs(req: req, path: "/Users/ethan", dirs: ["dev"], truncated: false)))
        #expect(picker.path == "/Users/ethan")
    }

    @Test func theRootHasNoParent() async {
        let picker = await listed("/", ["Users"])
        #expect(picker.parent == nil)
        await picker.enter("Users")
        #expect(link.sent.last.map { if case .listDirs(_, "/Users") = $0 { true } else { false } } == true)
    }
}
