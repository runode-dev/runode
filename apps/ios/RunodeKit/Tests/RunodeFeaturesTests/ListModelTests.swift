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
        #expect(link.sent.prefix(2) == [.listSessions, .layout(req: 0)])
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

    /// 连上就问宿主装着哪些 agent，只认自己那次请求的回话；断开后留着上次问到的。
    @Test func findsTheInstalledAgentsOnConnect() throws {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        guard case .findCommands(let req, nil, let programs)? = link.sent.last else {
            Issue.record("expected find_commands, got \(link.sent)")
            return
        }
        #expect(programs == AgentLauncher.programs)
        var found: [String?] = programs.map { _ in nil }
        found[try #require(programs.firstIndex(of: "claude"))] = "claude"
        model.handle(.message(.commands(req: req &+ 1, found: found)))
        #expect(model.agents.isEmpty)
        model.handle(.message(.commands(req: req, found: found)))
        #expect(model.agents.map(\.command) == ["claude"])
        model.handle(.state(.idle))
        #expect(model.agents.map(\.command) == ["claude"])
    }

    /// 会话里 shell 报告了 PATH 就按它重新找：宿主新起的 shell 拿到的 PATH 常常没有装 agent 的目录。PATH 没变时
    /// 不重找，报告 PATH 的会话没了时换下一个会话的。
    @Test func findsAgentsOnThePathTheShellsReport() {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        model.handle(.message(.sessionList([info(sessionA, title: "a"), info(sessionB, title: "b")])))
        link.clearSent()
        func searched() -> [String?] {
            link.sent.compactMap {
                if case .findCommands(_, let path, _) = $0 { path } else { nil }
            }
        }
        let attached = HostMsg.Attached(
            id: sessionB, channel: 1, size: smallGrid, mode: .metaOnly, meta: SessionMeta(title: "b"), settings: nil,
            shell: ShellReport(path: "/b/bin"))
        model.handle(.message(.attached(attached)))
        #expect(searched() == ["/b/bin"])
        model.handle(.message(.meta(id: sessionB, meta: SessionMeta(title: "b"), shell: ShellReport(path: "/b/bin"))))
        model.handle(.message(.meta(id: sessionB, meta: SessionMeta(title: "b2"))))
        #expect(searched() == ["/b/bin"])
        model.handle(.message(.meta(id: sessionA, meta: SessionMeta(title: "a"), shell: ShellReport(path: "/a/bin"))))
        #expect(searched() == ["/b/bin", "/a/bin"])
        model.handle(.message(.sessionList([info(sessionB, title: "b")])))
        #expect(searched() == ["/b/bin", "/a/bin", "/b/bin"])
    }

    /// 新开 agent：和新开终端一样开标签，开好后在新终端里打它的命令、回车。
    @Test func spawningAnAgentRunsItInTheNewTerminal() async {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        link.clearSent()
        let agent = InstalledAgent(label: "Pi", kind: AgentKind("pi"), command: "pi")
        await model.spawn(near: sessionB, agent: agent)
        guard case .open(let req, .tab, sessionB?, nil, false)? = link.sent.first else {
            Issue.record("expected an open beside sessionB, got \(link.sent)")
            return
        }
        link.clearSent()
        model.handle(.message(.opened(req: req, id: sessionA)))
        let typed = link.sent.filter {
            switch $0 {
            case .paste, .sendKeys: true
            default: false
            }
        }
        guard case .paste(_, sessionA, "pi")? = typed.first, case .sendKeys(_, sessionA, ["enter"])? = typed.last,
            typed.count == 2
        else {
            Issue.record("expected the agent to be typed, got \(link.sent)")
            return
        }
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
        model.handle(.message(.error(req: openReq, id: nil, message: "there is no Runode window to do this in")))
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
                                // 恢复窗口时还没启动的标签页：有标签，但里面没有会话。
                                WorkspaceLayout(
                                    index: 3, name: "未启动", dir: "/Users/ethan/later",
                                    tabs: [TabLayout(index: 1, active: true, panes: [])]),
                            ])
                    ])))
        let sections = model.sections
        #expect(
            sections.map(\.id) == [
                .workspace(window: 1, index: 1), .workspace(window: 1, index: 2), .workspace(window: 1, index: 3),
            ])
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

    /// 筛选只留那一类的会话，筛空了的节整节去掉；全部照旧，连空工作区一起。
    @Test func filtersKeepOnlyMatchingSessions() {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        func agent(_ id: SessionId, _ state: AgentState) -> SessionInfo {
            var session = info(id, title: "t")
            session.meta.agent = Agent(kind: AgentKind("claude"), state: state)
            return session
        }
        model.handle(
            .message(.sessionList([agent(sessionA, .blocked), agent(sessionB, .working), info(sessionC, title: "c")])))
        let tab = TabLayout(
            index: 1, active: true,
            panes: [PaneLayout(index: 1, id: sessionA, focused: true), PaneLayout(index: 2, id: sessionB)])
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
        #expect(model.sections(matching: .all) == model.sections)
        #expect(
            model.sections(matching: .all).map(\.id) == [
                .workspace(window: 1, index: 1), .workspace(window: 1, index: 2), .background,
            ])
        let waiting = model.sections(matching: .waiting)
        #expect(waiting.map(\.id) == [.workspace(window: 1, index: 1)])
        #expect(waiting.flatMap(\.sessions).map(\.id) == [sessionA])
        let working = model.sections(matching: .working)
        #expect(working.map(\.id) == [.workspace(window: 1, index: 1)])
        #expect(working.flatMap(\.sessions).map(\.id) == [sessionB])
        #expect(model.count(.all) == 3)
        #expect(model.count(.waiting) == 1)
        #expect(model.count(.working) == 1)
        // 没有在等的会话时整节去掉，一节都不剩。
        model.handle(.message(.meta(id: sessionA, meta: SessionMeta(title: "t"), shell: nil)))
        #expect(model.sections(matching: .waiting).isEmpty)
        #expect(model.count(.waiting) == 0)
    }

    /// 工作区有几个标签页时卡片标出第几个；只有一个标签页、不在窗口里的不标。
    @Test func tabNumberCountsWithinTheWorkspace() {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        model.handle(.message(.sessionList([info(sessionA, title: "a"), info(sessionB, title: "b")])))
        #expect(model.tabNumber(of: sessionA) == nil)
        let first = TabLayout(index: 1, active: true, panes: [PaneLayout(index: 1, id: sessionA, focused: true)])
        let second = TabLayout(index: 2, active: false, panes: [PaneLayout(index: 1, id: sessionB)])
        model.handle(
            .message(
                .layout(req: 0, windows: [WindowLayout(index: 1, workspaces: [WorkspaceLayout(index: 1, tabs: [first, second])])])))
        #expect(model.tabNumber(of: sessionA) == 1)
        #expect(model.tabNumber(of: sessionB) == 2)
        // 另一个标签页的会话列表里没有（电脑上还没启动它）：只剩一张卡片，不标。
        model.handle(.message(.sessionList([info(sessionA, title: "a")])))
        #expect(model.tabNumber(of: sessionA) == nil)
        model.handle(.message(.sessionList([info(sessionA, title: "a"), info(sessionB, title: "b")])))
        model.handle(
            .message(
                .layout(
                    req: 0,
                    windows: [
                        WindowLayout(
                            index: 1,
                            workspaces: [WorkspaceLayout(index: 1, tabs: [first]), WorkspaceLayout(index: 2, tabs: [second])])
                    ])))
        #expect(model.tabNumber(of: sessionA) == nil)
        #expect(model.tabNumber(of: sessionB) == nil)
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

    /// 改名只改名字：连接在列表不知道时记下的 `node`、`lastAddress` 不被列表里的旧副本盖掉。
    @Test func renamingKeepsWhatTheConnectionLearned() async throws {
        let store = MemoryMachineStore()
        let model = MachineListModel(store: store, keyStore: MemoryDeviceKeyStore())
        let machine = machineRecord()
        try await model.add(machine)
        await store.updateNode(id: machine.id, "learned-node")
        await store.updateLastAddress(id: machine.id, "10.0.0.9")
        await model.rename(machine.id, to: "工作机")
        let stored = try #require(await store.all().first)
        #expect(stored.name == "工作机")
        #expect(stored.node == "learned-node")
        #expect(stored.lastAddress == "10.0.0.9")
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
    func updateNode(id: UUID, _ node: String) throws { throw WriteFailed() }
    func updateName(id: UUID, _ name: String) throws { throw WriteFailed() }
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

    /// 配好后回到扫码，镜头里还是刚才那张码：不重配同一台；换一张码照常配。
    @Test func goingBackToScanDoesNotPairTheSameCodeAgain() async {
        var paired: [MachineRecord] = []
        let model = PairingModel(
            pairing: FakePairing { _ in machineRecord() }, deviceName: "x",
            now: { Date(timeIntervalSince1970: 1_800_000_000) }, onPaired: { paired.append($0) })
        await model.scanned(link())
        #expect(paired.count == 1)
        model.reset()
        await model.scanned(link())
        #expect(model.phase == .idle)
        #expect(paired.count == 1)
        await model.scanned(link(exp: 1_900_000_001))
        #expect(paired.count == 2)
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
    /// 旁边写着的电脑名字和连上的那台报的一样时，状态里不再重复名字。
    @Test func linkStateSkipsTheNameAlreadyShown() {
        let state = LinkState.connected(hostName: "Mac mini", address: nil)
        #expect(Presentation.linkState(state, shownName: "Mac mini") == "已连接")
        #expect(Presentation.linkState(state, shownName: "办公室") == "已连接 Mac mini")
        #expect(Presentation.linkState(state) == "已连接 Mac mini")
    }

    @Test func directoriesUnderHomeAreShortened() {
        #expect(Presentation.directory("/Users/ethan/dev/runode") == "~/dev/runode")
        #expect(Presentation.directory("/Users/ethan") == "~")
        #expect(Presentation.directory("/tmp") == "/tmp")
        #expect(Presentation.directory(nil) == nil)
    }

    @Test func sectionDetailSkipsADirectoryThatRepeatsTheTitle() {
        let home = SessionSection(id: .workspace(window: 1, index: 0), name: "~", dir: "/Users/ethan", sessions: [])
        #expect(Presentation.sectionDetail(home) == nil)
        var second = home
        second.window = 2
        #expect(Presentation.sectionDetail(second) == "窗口 2")
        let repo = SessionSection(id: .workspace(window: 1, index: 1), name: "runode", dir: "/Users/ethan/dev/runode", sessions: [])
        #expect(Presentation.sectionDetail(repo) == "~/dev/runode")
        var repoInSecond = repo
        repoInSecond.window = 2
        #expect(Presentation.sectionDetail(repoInSecond) == "窗口 2，~/dev/runode")
        #expect(Presentation.sectionDirectory(home) == nil)
        #expect(Presentation.sectionWindow(home) == nil)
        #expect(Presentation.sectionDirectory(repo) == "~/dev/runode")
        #expect(Presentation.sectionWindow(second) == "窗口 2")
        let background = SessionSection(id: .background, sessions: [])
        #expect(Presentation.sectionDirectory(background) == nil)
        #expect(Presentation.sectionWindow(background) == nil)
    }

    @Test func programLabelsDropTheExeSuffix() {
        #expect(Presentation.programLabel("powershell.exe") == "powershell")
        #expect(Presentation.programLabel("Code.EXE") == "Code")
        #expect(Presentation.programLabel("zsh") == "zsh")
        #expect(Presentation.programLabel(".exe") == ".exe")
    }

    @Test func pairingListsTwoAddressesIPv4First() {
        #expect(Presentation.pairingAddresses(["10.0.0.2"], port: 7866) == "10.0.0.2:7866")
        #expect(
            Presentation.pairingAddresses(["fd00::1", "10.0.0.2", "192.168.1.5"], port: 7866)
                == "10.0.0.2:7866, 192.168.1.5:7866 等 3 个地址")
        #expect(Presentation.pairingAddresses(["fd00::1", "10.0.0.2"], port: 7866) == "10.0.0.2:7866, [fd00::1]:7866")
    }

    @Test func agentLogosMatchTheBundledImages() throws {
        let catalog = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .appending(path: "Sources/RunodeFeatures/Resources/AgentLogos.xcassets")
        let images = Set(
            try FileManager.default.contentsOfDirectory(atPath: catalog.path)
                .filter { $0.hasSuffix(".imageset") }.map { String($0.dropLast(".imageset".count)) })
        // 宿主认得的各种 agent，取自 `runode_protocol` 的测试 `message_fixture` 生成的样例。
        struct Agents: Decodable {
            struct Agent: Decodable { let kind: String }
            let agents: [Agent]
        }
        let fixture = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .appending(path: "crates/protocol/tests/fixtures/messages.json")
        let labels = try JSONDecoder().decode(Agents.self, from: Data(contentsOf: fixture)).agents.map(\.kind)
        try #require(labels.contains("claude"))
        let mapped = Set(labels.compactMap { Presentation.agentLogoAsset(AgentKind($0)) })
        #expect(mapped == images)
        // 每个 imageset 里都有图：符号链接指着的 SVG 在，或者栅格化好的 PNG 在。
        for image in images {
            let set = catalog.appending(path: "\(image).imageset")
            let svg = FileManager.default.fileExists(atPath: set.appending(path: "\(image).svg").path)
            let png = FileManager.default.fileExists(atPath: set.appending(path: "\(image)@3x.png").path)
            #expect(svg || png, "\(image).imageset 里没有图")
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

    /// 宿主没报时刻时只说状态。
    @Test func agentStatesReadNaturally() {
        func status(_ kind: String, _ state: AgentState) -> String? {
            Presentation.agentStatus(Agent(kind: AgentKind(kind), state: state), since: nil, now: .now)?.text
        }
        #expect(status("claude", .working) == "Claude Code，干活中")
        #expect(status("codex", .idle) == "Codex，空闲")
        #expect(status("new", .blocked) == "new，等你回答")
        #expect(status("x", .unknown("z")) == nil)
        #expect(Presentation.agentStatus(nil, since: nil, now: .now) == nil)
    }

    /// 有时刻、满一分钟时说等了、干了多久：只写最大的一个单位，往下取整；不满一分钟（包括时刻比现在晚）
    /// 照旧只说状态；空闲不写时长。
    @Test func agentStatesSayHowLong() {
        let now = Date(timeIntervalSince1970: 1_790_000_000)
        let zh = Locale(identifier: "zh-Hans")
        func status(_ state: AgentState, ago: TimeInterval, locale: Locale = zh) -> String? {
            Presentation.agentStatus(
                Agent(kind: AgentKind("claude"), state: state), since: now.addingTimeInterval(-ago), now: now,
                locale: locale)?.text
        }
        #expect(status(.blocked, ago: 12 * 60 + 40) == "Claude Code 等了 12分钟")
        #expect(status(.working, ago: 2 * 3600 + 59 * 60) == "Claude Code 干了 2小时")
        #expect(status(.blocked, ago: 59) == "Claude Code，等你回答")
        #expect(status(.working, ago: 30) == "Claude Code，干活中")
        #expect(status(.blocked, ago: 60) == "Claude Code 等了 1分钟")
        #expect(status(.idle, ago: 600) == "Claude Code，空闲")
        #expect(status(.blocked, ago: -90) == "Claude Code，等你回答")
        #expect(status(.working, ago: 5 * 60, locale: Locale(identifier: "en")) == "Claude Code 干了 5 minutes")
    }

    /// 时长的重画时刻：从 `since` 起每满一分钟；时刻比现在晚时等到 `since` 起的第一分钟。
    @Test func agentClockTicks() {
        let since = Date(timeIntervalSince1970: 1_790_000_000)
        func offsets(from start: TimeInterval, count: Int) -> [TimeInterval] {
            Array(AgentClock.ticks(since: since, from: since.addingTimeInterval(start)).prefix(count))
                .map { ($0.timeIntervalSince(since) * 1000).rounded() / 1000 }
        }
        #expect(offsets(from: 3, count: 4) == [3, 60, 120, 180])
        #expect(offsets(from: 60, count: 3) == [60, 120, 180])
        #expect(offsets(from: 150, count: 3) == [150, 180, 240])
        #expect(offsets(from: -5, count: 3) == [-5, 60, 120])
    }

    /// 时长在三种界面语言下写出来的样子。
    @Test func agentElapsedFollowsTheLocale() {
        let now = Date(timeIntervalSince1970: 1_790_000_000)
        func elapsed(_ seconds: TimeInterval, _ locale: String) -> String? {
            Presentation.agentElapsed(since: now.addingTimeInterval(-seconds), now: now, locale: Locale(identifier: locale))
        }
        #expect(elapsed(59, "en") == nil)
        #expect(elapsed(12 * 60, "zh-Hans") == "12分钟")
        #expect(elapsed(12 * 60, "en") == "12 minutes")
        #expect(elapsed(12 * 60, "zh-Hant") == "12分鐘")
        #expect(elapsed(60, "en") == "1 minute")
        #expect(elapsed(2 * 3600, "zh-Hans") == "2小时")
        #expect(elapsed(2 * 3600, "en") == "2 hours")
        #expect(elapsed(2 * 3600, "zh-Hant") == "2小時")
        #expect(Presentation.agentElapsed(since: nil, now: now) == nil)
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

    /// Windows 宿主回 `C:/Users/me` 这种写法：照样按 `/` 拼子目录、往上走，到盘符根为止。
    @Test func windowsPathsStopAtTheDriveRoot() async {
        let picker = await listed("C:/Users/me", ["dev"])
        #expect(picker.parent == "C:/Users")
        await picker.enter("dev")
        #expect(link.sent.last.map { if case .listDirs(_, "C:/Users/me/dev") = $0 { true } else { false } } == true)
        let root = await listed("C:/", ["Users"])
        #expect(root.parent == nil)
    }

    /// Windows 宿主回的 UNC 路径 `//server/share/…`：拼子目录、往上走都留着开头的 `//`，到共享根为止。
    @Test func uncPathsKeepTheirLeadingSlashes() async {
        let picker = await listed("//server/share/team", ["dev"])
        #expect(picker.parent == "//server/share")
        await picker.enter("dev")
        #expect(
            link.sent.last.map { if case .listDirs(_, "//server/share/team/dev") = $0 { true } else { false } } == true)
        let share = await listed("//server/share", ["team"])
        #expect(share.parent == nil)
    }
}

@Suite struct SessionStatusTests {
    func session(
        agent: AgentState? = nil, foregroundIsShell: Bool = true, foreground: String? = "zsh", exited: Bool = false
    ) -> SessionInfo {
        SessionInfo(
            id: sessionA, size: smallGrid,
            meta: SessionMeta(
                title: "t", agent: agent.map { Agent(kind: AgentKind("claude"), state: $0) },
                foregroundIsShell: foregroundIsShell, foreground: foreground),
            exited: exited)
    }

    @Test func agentStates() {
        #expect(SessionStatus.of(session(agent: .blocked)) == .waiting)
        #expect(SessionStatus.of(session(agent: .working)) == .working)
        #expect(SessionStatus.of(session(agent: .idle)) == .agentIdle)
        #expect(SessionStatus.of(session(agent: .unknown("thinking"))) == .agentIdle)
    }

    /// 前台不是 shell 时带程序名，Windows 的 `.exe` 去掉；只有 shell 时是 `shell`。
    @Test func programsAndShells() {
        #expect(SessionStatus.of(session(foregroundIsShell: false, foreground: "vim")) == .running("vim"))
        #expect(SessionStatus.of(session(foregroundIsShell: false, foreground: "nvim.exe")) == .running("nvim"))
        #expect(SessionStatus.of(session()) == .shell)
        #expect(SessionStatus.of(session(foregroundIsShell: false, foreground: nil)) == .shell)
    }

    /// 退出了的会话不看 agent 和前台程序。
    @Test func exitedWins() {
        #expect(SessionStatus.of(session(agent: .blocked, exited: true)) == .exited)
        #expect(SessionStatus.of(session(foregroundIsShell: false, foreground: "vim", exited: true)) == .exited)
    }
}
