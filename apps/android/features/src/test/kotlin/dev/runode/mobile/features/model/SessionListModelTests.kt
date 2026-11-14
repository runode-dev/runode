package dev.runode.mobile.features.model

import dev.runode.mobile.connection.HostEvent
import dev.runode.mobile.connection.LinkFailure
import dev.runode.mobile.connection.LinkState
import dev.runode.mobile.protocol.Agent
import dev.runode.mobile.protocol.AgentKind
import dev.runode.mobile.protocol.AgentState
import dev.runode.mobile.protocol.AttachMode
import dev.runode.mobile.protocol.ClientMsg
import dev.runode.mobile.protocol.HostMsg
import dev.runode.mobile.protocol.IntegrationMode
import dev.runode.mobile.protocol.PaneLayout
import dev.runode.mobile.protocol.PaneRect
import dev.runode.mobile.protocol.Placement
import dev.runode.mobile.protocol.SessionId
import dev.runode.mobile.protocol.SessionInfo
import dev.runode.mobile.protocol.SessionMeta
import dev.runode.mobile.protocol.ShellReport
import dev.runode.mobile.protocol.TabLayout
import dev.runode.mobile.protocol.WindowLayout
import dev.runode.mobile.protocol.WorkspaceLayout
import java.time.Instant
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNotEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import org.junit.Rule

class SessionListModelTests {
    @get:Rule val main = MainDispatcherRule()

    private val link = FakeLink()

    private fun info(id: SessionId, title: String) =
        SessionInfo(id, smallGrid, SessionMeta(title = title), sizeOwner = "Ethan 的 MacBook")

    private fun model() = SessionListModel(machineRecord(), link)

    private fun connected() = HostEvent.State(LinkState.Connected("homelab", null))

    private fun lost() = HostEvent.State(LinkState.Waiting(LinkFailure.Timeout, Instant.now()))

    private fun message(message: HostMsg) = HostEvent.Message(message)

    private fun layout(vararg workspaces: WorkspaceLayout) =
        message(HostMsg.Layout(0u, listOf(WindowLayout(1u, workspaces = workspaces.toList()))))

    private fun SessionListModel.track(): MutableList<SessionId> =
        mutableListOf<SessionId>().also { spawned -> onSpawned = { spawned += it } }

    @Test fun listsAndWatchesEverySession() {
        val model = model()
        model.handle(connected())
        model.handle(HostEvent.Ready(1u))
        assertEquals(listOf(ClientMsg.ListSessions, ClientMsg.Layout(0u)), link.sent.take(2))
        model.screenOpened(sessionB)
        model.handle(message(HostMsg.SessionList(listOf(info(sessionA, "a"), info(sessionB, "b")))))
        assertEquals(2, model.sessions.size)
        assertTrue(model.loaded)
        // 终端页开着的会话不发只看状态的 Attach，免得换掉终端页的订阅。
        assertEquals(listOf(ClientMsg.Attach(sessionA, null, AttachMode.MetaOnly)), attaches(link.sent))
        // 再列一次不重复发。
        link.clearSent()
        model.handle(message(HostMsg.SessionList(listOf(info(sessionA, "a"), info(sessionB, "b")))))
        assertTrue(attaches(link.sent).isEmpty())
    }

    /** 停了订阅收不到断开的状态：stop 自己当作断开，回到前台、还没连上时不往没有的连接上发请求。 */
    @Test fun stoppingCountsAsDisconnected() {
        val model = model()
        model.handle(connected())
        model.handle(HostEvent.Ready(1u))
        model.handle(message(HostMsg.SessionList(listOf(info(sessionA, "a")))))
        model.stop()
        link.clearSent()
        assertFalse(model.linkState.isConnected)
        model.refresh()
        model.screenClosed(sessionA)
        assertTrue(link.sent.isEmpty())
    }

    /** Resync 后重新只看状态被宿主以积压拒掉：下一次列会话时再订；终端页开着的会话不因此多发。 */
    @Test fun aBackedUpRewatchIsRetriedOnTheNextList() {
        val model = model()
        model.handle(connected())
        model.handle(HostEvent.Ready(1u))
        model.screenOpened(sessionB)
        model.handle(message(HostMsg.SessionList(listOf(info(sessionA, "a"), info(sessionB, "b")))))
        model.handle(message(HostMsg.Resync(sessionA, "slow")))
        val backedUp = HostMsg.backedUpPrefix + "; read first"
        model.handle(message(HostMsg.Error(null, sessionA, backedUp)))
        model.handle(message(HostMsg.Error(null, sessionB, backedUp)))
        link.clearSent()
        model.handle(message(HostMsg.SessionList(listOf(info(sessionA, "a"), info(sessionB, "b")))))
        assertEquals(listOf(ClientMsg.Attach(sessionA, null, AttachMode.MetaOnly)), attaches(link.sent))
    }

    @Test fun metaUpdatesArriveLive() {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        model.handle(message(HostMsg.SessionList(listOf(info(sessionA, "a")))))
        val agent = Agent(AgentKind("claude"), AgentState.Blocked)
        model.handle(message(HostMsg.Meta(sessionA, SessionMeta(title = "修 bug", agent = agent))))
        assertEquals("修 bug", model.session(sessionA)?.meta?.title)
        assertEquals(agent, model.session(sessionA)?.meta?.agent)
        model.handle(message(HostMsg.Exited(sessionA, 0)))
        assertEquals(true, model.session(sessionA)?.exited)
    }

    @Test fun closingATerminalGoesBackToMetaOnly() {
        val model = model()
        model.handle(connected())
        model.handle(HostEvent.Ready(1u))
        model.screenOpened(sessionA)
        model.handle(message(HostMsg.SessionList(listOf(info(sessionA, "a")))))
        link.clearSent()
        model.screenClosed(sessionA)
        assertEquals(listOf<ClientMsg>(ClientMsg.Attach(sessionA, null, AttachMode.MetaOnly)), link.sent)
    }

    /** 新开会话先请电脑上的 app 在窗口里开一个不抢焦点的新标签，电脑上也看得到。 */
    @Test fun spawnOpensATabOnTheComputer() {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        link.clearSent()
        model.spawn()
        assertTrue(model.isSpawning)
        val open = assertIs<ClientMsg.Open>(link.sent.first())
        assertEquals(ClientMsg.Open(open.req, Placement.Tab, null, null, false), open)
        val spawned = model.track()
        model.handle(message(HostMsg.Opened(open.req + 7u, sessionB)))
        assertTrue(spawned.isEmpty())
        model.handle(message(HostMsg.Opened(open.req, sessionA)))
        assertEquals(listOf(sessionA), spawned)
        assertFalse(model.isSpawning)
    }

    /** 等 Opened 时断了（断线等重连、进后台回来先是 Idle）：回话不会来了，重连后还能再开。 */
    @Test fun disconnectingWhileSpawningLetsYouSpawnAgain() {
        for (lost in listOf(LinkState.Waiting(LinkFailure.Timeout, Instant.now()), LinkState.Idle)) {
            val link = FakeLink()
            val model = SessionListModel(machineRecord(), link)
            model.handle(HostEvent.Ready(1u))
            model.spawn()
            val oldReq = assertIs<ClientMsg.Open>(link.sent.last()).req
            model.handle(HostEvent.State(lost))
            assertFalse(model.isSpawning)
            val spawned = model.track()
            model.handle(message(HostMsg.Opened(oldReq, sessionB)))
            assertTrue(spawned.isEmpty())

            model.handle(HostEvent.Ready(2u))
            link.clearSent()
            model.spawn()
            assertTrue(model.isSpawning)
            val req = assertIs<ClientMsg.Open>(link.sent.last()).req
            model.handle(message(HostMsg.Opened(req, sessionA)))
            assertEquals(listOf(sessionA), spawned)
            assertFalse(model.isSpawning)
        }
    }

    /** 连上就问宿主装着哪些 agent，只认自己那次请求的回话；断开后留着上次问到的。 */
    @Test fun findsTheInstalledAgentsOnConnect() {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        val find = assertIs<ClientMsg.FindCommands>(link.sent.last())
        assertNull(find.path)
        assertEquals(AgentLauncher.programs, find.programs)
        val found = MutableList<String?>(find.programs.size) { null }
        found[find.programs.indexOf("claude")] = "claude"
        model.handle(message(HostMsg.Commands(find.req + 1u, found)))
        assertTrue(model.agents.isEmpty())
        model.handle(message(HostMsg.Commands(find.req, found)))
        assertEquals(listOf("claude"), model.agents.map { it.command })
        model.handle(HostEvent.State(LinkState.Idle))
        assertEquals(listOf("claude"), model.agents.map { it.command })
    }

    /**
     * 会话里 shell 报告了 PATH 就按它重新找：宿主新起的 shell 拿到的 PATH 常常没有装 agent 的目录。PATH 没变时不重找，
     * 报告 PATH 的会话没了时换下一个会话的。
     */
    @Test fun findsAgentsOnThePathTheShellsReport() {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        model.handle(message(HostMsg.SessionList(listOf(info(sessionA, "a"), info(sessionB, "b")))))
        link.clearSent()
        fun searched() = link.sent.filterIsInstance<ClientMsg.FindCommands>().map { it.path }
        val attached = HostMsg.Attached(
            sessionB, 1u, smallGrid, AttachMode.MetaOnly, SessionMeta(title = "b"), null, ShellReport("/b/bin"),
        )
        model.handle(message(attached))
        assertEquals(listOf<String?>("/b/bin"), searched())
        model.handle(message(HostMsg.Meta(sessionB, SessionMeta(title = "b"), ShellReport("/b/bin"))))
        model.handle(message(HostMsg.Meta(sessionB, SessionMeta(title = "b2"))))
        assertEquals(listOf<String?>("/b/bin"), searched())
        model.handle(message(HostMsg.Meta(sessionA, SessionMeta(title = "a"), ShellReport("/a/bin"))))
        assertEquals(listOf<String?>("/b/bin", "/a/bin"), searched())
        model.handle(message(HostMsg.SessionList(listOf(info(sessionB, "b")))))
        assertEquals(listOf<String?>("/b/bin", "/a/bin", "/b/bin"), searched())
    }

    /** 新开 agent：和新开终端一样开标签，开好后在新终端里打它的命令、回车。 */
    @Test fun spawningAnAgentRunsItInTheNewTerminal() {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        link.clearSent()
        val agent = InstalledAgent("Pi", AgentKind("pi"), "pi")
        model.spawn(near = sessionB, agent = agent)
        val open = assertIs<ClientMsg.Open>(link.sent.first())
        assertEquals(ClientMsg.Open(open.req, Placement.Tab, sessionB, null, false), open)
        link.clearSent()
        model.handle(message(HostMsg.Opened(open.req, sessionA)))
        val typed = link.sent.filter { it is ClientMsg.Paste || it is ClientMsg.SendKeys }
        assertEquals(2, typed.size)
        val paste = assertIs<ClientMsg.Paste>(typed.first())
        assertEquals(sessionA to "pi", paste.id to paste.text)
        val keys = assertIs<ClientMsg.SendKeys>(typed.last())
        assertEquals(sessionA to listOf("enter"), keys.id to keys.keys)
    }

    /** 电脑上的 app 没开着窗口时退回自己开一个后台会话。 */
    @Test fun spawnFallsBackWithoutADesktopWindow() {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        link.clearSent()
        model.spawn()
        val openReq = assertIs<ClientMsg.Open>(link.sent.first()).req
        link.clearSent()
        model.handle(message(HostMsg.Error(openReq, null, "there is no Runode window to do this in")))
        assertNull(model.errorMessage)
        assertTrue(model.isSpawning)
        val spawn = assertIs<ClientMsg.Spawn>(link.sent.first())
        assertEquals(ClientMsg.Spawn(spawn.req, model.spawnSize, null, IntegrationMode.Detect, true), spawn)
        assertNotEquals(openReq, spawn.req)
        val spawned = model.track()
        model.handle(message(HostMsg.Spawned(openReq, sessionB)))
        assertTrue(spawned.isEmpty())
        model.handle(message(HostMsg.Spawned(spawn.req, sessionA)))
        assertEquals(listOf(sessionA), spawned)
        assertFalse(model.isSpawning)
    }

    @Test fun spawnErrorsAreShown() {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        link.clearSent()
        model.spawn()
        val openReq = assertIs<ClientMsg.Open>(link.sent.first()).req
        model.handle(message(HostMsg.Error(openReq, null, "no window")))
        val req = assertIs<ClientMsg.Spawn>(link.sent.last()).req
        model.handle(message(HostMsg.Error(req, null, "no shell")))
        assertEquals("Couldn't open a new terminal: no shell", model.errorMessage.english)
        assertFalse(model.isSpawning)
    }

    /** 在某个工作区里新开终端：挨着那个工作区的 anchor 开新标签，开好后连列表带布局再要一次。 */
    @Test fun spawnInAWorkspaceOpensBesideItsAnchor() {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        link.clearSent()
        model.spawn(near = sessionB)
        val open = assertIs<ClientMsg.Open>(link.sent.first())
        assertEquals(ClientMsg.Open(open.req, Placement.Tab, sessionB, null, false), open)
        link.clearSent()
        model.handle(message(HostMsg.Opened(open.req, sessionA)))
        assertEquals(listOf(ClientMsg.ListSessions, ClientMsg.Layout(0u)), link.sent)
    }

    /** 新建工作区：选目录的页面收起，OpenWorkspace 开好后和新开终端一样打开它的终端。 */
    @Test fun createWorkspaceOpensItsFirstTerminal() {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        model.handle(layout())
        model.beginNewWorkspace()
        assertNotNull(model.directoryPicker)
        link.clearSent()
        model.createWorkspace("/Users/ethan/dev")
        assertNull(model.directoryPicker)
        assertTrue(model.isSpawning)
        val open = assertIs<ClientMsg.OpenWorkspace>(link.sent.first())
        assertEquals(ClientMsg.OpenWorkspace(open.req, "/Users/ethan/dev", false, null), open)
        val spawned = model.track()
        model.handle(message(HostMsg.Opened(open.req, sessionA)))
        assertEquals(listOf(sessionA), spawned)
        assertFalse(model.isSpawning)
    }

    /** 新建工作区没成时不退回 Spawn，直接提示。 */
    @Test fun createWorkspaceErrorsAreShown() {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        model.handle(layout())
        link.clearSent()
        model.createWorkspace("/nope")
        val req = assertIs<ClientMsg.OpenWorkspace>(link.sent.first()).req
        link.clearSent()
        model.handle(message(HostMsg.Error(req, null, "not a directory: /nope")))
        assertTrue(link.sent.isEmpty())
        assertEquals("Couldn't create the workspace: not a directory: /nope", model.errorMessage.english)
        assertFalse(model.isSpawning)
    }

    /** 等 OpenWorkspace 时来了不带编号的 Error：不是给新建工作区的，照样等着，随后的 Opened 仍然打开新终端。 */
    @Test fun unnumberedErrorsAreNotTakenForTheWorkspaceReply() {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        model.handle(layout())
        link.clearSent()
        model.createWorkspace("/Users/ethan")
        val req = assertIs<ClientMsg.OpenWorkspace>(link.sent.first()).req
        val spawned = model.track()
        model.handle(message(HostMsg.Error(null, null, "unknown message")))
        assertTrue(model.isSpawning)
        assertNull(model.errorMessage)
        model.handle(message(HostMsg.Opened(req, sessionB)))
        assertEquals(listOf(sessionB), spawned)
        assertFalse(model.isSpawning)
    }

    /** 新建工作区时填的名字去掉首尾空白后带上；只有空白时当没填。 */
    @Test fun createWorkspaceCarriesTheName() {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        model.handle(layout())
        link.clearSent()
        model.createWorkspace("/Users/ethan/dev", "  后端 ")
        val named = assertIs<ClientMsg.OpenWorkspace>(link.sent.first())
        assertEquals(ClientMsg.OpenWorkspace(named.req, "/Users/ethan/dev", false, "后端"), named)
        model.handle(message(HostMsg.Opened(named.req, sessionA)))
        link.clearSent()
        model.createWorkspace("/Users/ethan/dev", "  ")
        assertNull(assertIs<ClientMsg.OpenWorkspace>(link.sent.first()).name)
    }

    /**
     * 电脑上关掉了最后一个标签的工作区还在：列成一个空节。它没有 anchor，在里面新开终端是对它的目录发
     * OpenWorkspace。
     */
    @Test fun emptyWorkspacesGetASection() {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        model.handle(message(HostMsg.SessionList(listOf(info(sessionA, "a")))))
        val tab = TabLayout(1u, true, listOf(PaneLayout(1u, sessionA, focused = true)))
        model.handle(
            layout(
                WorkspaceLayout(1u, "a", "/Users/ethan/a", tabs = listOf(tab)),
                WorkspaceLayout(2u, "空", "/Users/ethan/empty", tabs = emptyList()),
                // 恢复窗口时还没启动的标签页：有标签，但里面没有会话。
                WorkspaceLayout(3u, "未启动", "/Users/ethan/later", tabs = listOf(TabLayout(1u, true, emptyList()))),
            ),
        )
        val sections = model.sections
        assertEquals(
            listOf<SessionSection.Id>(
                SessionSection.Id.Workspace(1u, 1u),
                SessionSection.Id.Workspace(1u, 2u),
                SessionSection.Id.Workspace(1u, 3u),
            ),
            sections.map { it.id },
        )
        val empty = sections[1]
        assertTrue(empty.sessions.isEmpty())
        assertNull(empty.anchor)
        assertTrue(model.canSpawn(empty))
        assertEquals(sections, model.spawnChoices)
        link.clearSent()
        model.spawn(empty)
        val open = assertIs<ClientMsg.OpenWorkspace>(link.sent.first())
        assertEquals(ClientMsg.OpenWorkspace(open.req, "/Users/ethan/empty", false, null), open)
        val spawned = model.track()
        model.handle(message(HostMsg.Opened(open.req, sessionB)))
        assertEquals(listOf(sessionB), spawned)
        // 有 anchor 的照旧挨着它开新标签。
        link.clearSent()
        model.spawn(sections[0])
        val beside = assertIs<ClientMsg.Open>(link.sent.first())
        assertEquals(ClientMsg.Open(beside.req, Placement.Tab, sessionA, null, false), beside)
    }

    /** 只有一个工作区时新开终端不用挑；后台那一节不算工作区。 */
    @Test fun oneWorkspaceOffersNoChoice() {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        model.handle(message(HostMsg.SessionList(listOf(info(sessionA, "a"), info(sessionB, "b")))))
        val tab = TabLayout(1u, true, listOf(PaneLayout(1u, sessionA, focused = true)))
        model.handle(layout(WorkspaceLayout(1u, "a", "/Users/ethan/a", tabs = listOf(tab))))
        assertEquals(
            listOf(SessionSection.Id.Workspace(1u, 1u), SessionSection.Id.Background),
            model.sections.map { it.id },
        )
        assertTrue(model.spawnChoices.isEmpty())
    }

    /** 筛选只留那一类的会话，筛空了的节整节去掉；全部照旧，连空工作区一起。 */
    @Test fun filtersKeepOnlyMatchingSessions() {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        fun agent(id: SessionId, state: AgentState) =
            info(id, "t").let { it.copy(meta = it.meta.copy(agent = Agent(AgentKind("claude"), state))) }
        model.handle(
            message(
                HostMsg.SessionList(
                    listOf(agent(sessionA, AgentState.Blocked), agent(sessionB, AgentState.Working), info(sessionC, "c")),
                ),
            ),
        )
        val tab = TabLayout(1u, true, listOf(PaneLayout(1u, sessionA, focused = true), PaneLayout(2u, sessionB)))
        model.handle(
            layout(
                WorkspaceLayout(1u, "a", "/Users/ethan/a", tabs = listOf(tab)),
                WorkspaceLayout(2u, "空", "/Users/ethan/empty", tabs = emptyList()),
            ),
        )
        assertEquals(model.sections, model.sections(SessionFilter.All))
        assertEquals(
            listOf(SessionSection.Id.Workspace(1u, 1u), SessionSection.Id.Workspace(1u, 2u), SessionSection.Id.Background),
            model.sections(SessionFilter.All).map { it.id },
        )
        val waiting = model.sections(SessionFilter.Waiting)
        assertEquals(listOf<SessionSection.Id>(SessionSection.Id.Workspace(1u, 1u)), waiting.map { it.id })
        assertEquals(listOf(sessionA), waiting.flatMap { it.sessions }.map { it.id })
        val working = model.sections(SessionFilter.Working)
        assertEquals(listOf<SessionSection.Id>(SessionSection.Id.Workspace(1u, 1u)), working.map { it.id })
        assertEquals(listOf(sessionB), working.flatMap { it.sessions }.map { it.id })
        assertEquals(3, model.count(SessionFilter.All))
        assertEquals(1, model.count(SessionFilter.Waiting))
        assertEquals(1, model.count(SessionFilter.Working))
        // 没有在等的会话时整节去掉，一节都不剩。
        model.handle(message(HostMsg.Meta(sessionA, SessionMeta(title = "t"))))
        assertTrue(model.sections(SessionFilter.Waiting).isEmpty())
        assertEquals(0, model.count(SessionFilter.Waiting))
    }

    /** 工作区有几个标签页时卡片标出第几个；只有一个标签页、不在窗口里的不标。 */
    @Test fun tabNumberCountsWithinTheWorkspace() {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        model.handle(message(HostMsg.SessionList(listOf(info(sessionA, "a"), info(sessionB, "b")))))
        assertNull(model.tabNumber(sessionA))
        val first = TabLayout(1u, true, listOf(PaneLayout(1u, sessionA, focused = true)))
        val second = TabLayout(2u, false, listOf(PaneLayout(1u, sessionB)))
        model.handle(layout(WorkspaceLayout(1u, tabs = listOf(first, second))))
        assertEquals(1, model.tabNumber(sessionA))
        assertEquals(2, model.tabNumber(sessionB))
        // 另一个标签页的会话列表里没有（电脑上还没启动它）：只剩一张卡片，不标。
        model.handle(message(HostMsg.SessionList(listOf(info(sessionA, "a")))))
        assertNull(model.tabNumber(sessionA))
        model.handle(message(HostMsg.SessionList(listOf(info(sessionA, "a"), info(sessionB, "b")))))
        model.handle(layout(WorkspaceLayout(1u, tabs = listOf(first)), WorkspaceLayout(2u, tabs = listOf(second))))
        assertNull(model.tabNumber(sessionA))
        assertNull(model.tabNumber(sessionB))
    }

    /** 同一个标签里分了屏的会话互为兄弟；独自一个标签、不在窗口里的没有。 */
    @Test fun panesSharingATabListEachOther() {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        model.handle(message(HostMsg.SessionList(listOf(info(sessionA, "a"), info(sessionB, "b")))))
        assertTrue(model.panes(sessionA).isEmpty())
        val split = TabLayout(
            1u,
            true,
            listOf(
                PaneLayout(1u, sessionA, PaneRect(0u, 0u, 500u, 1000u), focused = true),
                PaneLayout(2u, sessionB, PaneRect(500u, 0u, 500u, 1000u)),
            ),
        )
        model.handle(layout(WorkspaceLayout(1u, tabs = listOf(split))))
        assertEquals(listOf(sessionA, sessionB), model.panes(sessionB).map { it.id })
        assertEquals(500u.toUShort(), model.panes(sessionB).last().rect?.x)
        // 电脑上刚分出来、列表里还没有的会话：先不列，补要一次列表，列表到了再列。
        model.handle(message(HostMsg.SessionList(listOf(info(sessionA, "a")))))
        link.clearSent()
        model.handle(layout(WorkspaceLayout(1u, tabs = listOf(split))))
        assertTrue(model.panes(sessionA).isEmpty())
        assertTrue(ClientMsg.ListSessions in link.sent)
        model.handle(message(HostMsg.SessionList(listOf(info(sessionA, "a"), info(sessionB, "b")))))
        assertEquals(listOf(sessionA, sessionB), model.panes(sessionA).map { it.id })
        // 电脑推来布局变了：马上重新要布局。
        link.clearSent()
        model.handle(message(HostMsg.LayoutChanged))
        assertEquals(listOf<ClientMsg>(ClientMsg.Layout(0u)), link.sent)
        val alone = TabLayout(1u, true, listOf(PaneLayout(1u, sessionA, focused = true)))
        model.handle(layout(WorkspaceLayout(1u, tabs = listOf(alone))))
        assertTrue(model.panes(sessionA).isEmpty())
    }

    /** 给工作区改名：按布局里的序号发 RenameWorkspace，办好后重新要布局；名字只有空白时不发。 */
    @Test fun renamingAWorkspaceRefreshesTheLayout() {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        val section = SessionSection(SessionSection.Id.Workspace(2u, 3u), sessions = emptyList())
        link.clearSent()
        model.rename(section, "   ")
        model.rename(SessionSection(SessionSection.Id.Background, sessions = emptyList()), "后台")
        assertTrue(link.sent.isEmpty())
        model.rename(section, " 前端 ")
        val rename = assertIs<ClientMsg.RenameWorkspace>(link.sent.first())
        assertEquals(ClientMsg.RenameWorkspace(rename.req, 2u, 3u, "前端"), rename)
        link.clearSent()
        model.handle(message(HostMsg.Done(rename.req + 7u)))
        assertTrue(link.sent.isEmpty())
        model.handle(message(HostMsg.Done(rename.req)))
        assertEquals(listOf(ClientMsg.ListSessions, ClientMsg.Layout(0u)), link.sent)
        assertNull(model.errorMessage)
    }

    @Test fun renameErrorsAreShown() {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        val section = SessionSection(SessionSection.Id.Workspace(1u, 9u), sessions = emptyList())
        link.clearSent()
        model.rename(section, "x")
        val req = assertIs<ClientMsg.RenameWorkspace>(link.sent.first()).req
        model.handle(message(HostMsg.Error(req, null, "no workspace 9")))
        assertEquals("Couldn't rename: no workspace 9", model.errorMessage.english)
    }

    /** 电脑上的 app 没开着窗口时建不了工作区：在选好的目录里开一个后台会话，开好后照样打开它的终端。 */
    @Test fun createWorkspaceWithoutADesktopWindowSpawnsInTheDirectory() {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        assertFalse(model.hasDesktopWindow)
        model.beginNewWorkspace()
        link.clearSent()
        model.createWorkspace("/Users/ethan/dev")
        assertNull(model.directoryPicker)
        assertTrue(model.isSpawning)
        val spawn = assertIs<ClientMsg.Spawn>(link.sent.first())
        assertEquals(ClientMsg.Spawn(spawn.req, model.spawnSize, "/Users/ethan/dev", IntegrationMode.Detect, true), spawn)
        val spawned = model.track()
        model.handle(message(HostMsg.Spawned(spawn.req, sessionA)))
        assertEquals(listOf(sessionA), spawned)
        assertFalse(model.isSpawning)
    }

    @Test fun killRemovesTheSession() {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        model.handle(message(HostMsg.SessionList(listOf(info(sessionA, "a")))))
        model.killTarget = sessionA
        assertNotNull(model.killTarget)
        link.clearSent()
        model.kill(sessionA)
        assertEquals(listOf<ClientMsg>(ClientMsg.Kill(sessionA)), link.sent)
        assertTrue(model.sessions.isEmpty())
    }

    @Test fun killWhileDisconnectedKeepsTheSession() {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        model.handle(message(HostMsg.SessionList(listOf(info(sessionA, "a")))))
        model.handle(lost())
        link.clearSent()
        model.kill(sessionA)
        assertTrue(link.sent.isEmpty())
        assertEquals(listOf(sessionA), model.sessions.map { it.id })
        assertEquals("Not connected; couldn't end the session", model.errorMessage.english)
    }

    @Test fun disconnectingForgetsWhatWasWatched() {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        model.handle(message(HostMsg.SessionList(listOf(info(sessionA, "a")))))
        model.handle(lost())
        assertIs<LinkState.Waiting>(model.linkState)
        link.clearSent()
        model.handle(HostEvent.Ready(2u))
        model.handle(message(HostMsg.SessionList(listOf(info(sessionA, "a")))))
        assertEquals(ClientMsg.ListSessions, link.sent.first())
        assertEquals(listOf(ClientMsg.Attach(sessionA, null, AttachMode.MetaOnly)), attaches(link.sent))
    }
}

class SessionStatusTests {
    private fun session(
        agent: AgentState? = null,
        foregroundIsShell: Boolean = true,
        foreground: String? = "zsh",
        exited: Boolean = false,
    ) = SessionInfo(
        sessionA,
        smallGrid,
        SessionMeta(
            title = "t",
            agent = agent?.let { Agent(AgentKind("claude"), it) },
            foregroundIsShell = foregroundIsShell,
            foreground = foreground,
        ),
        exited = exited,
    )

    @Test fun agentStates() {
        assertEquals(SessionStatus.Waiting, SessionStatus.of(session(AgentState.Blocked)))
        assertEquals(SessionStatus.Working, SessionStatus.of(session(AgentState.Working)))
        assertEquals(SessionStatus.AgentIdle, SessionStatus.of(session(AgentState.Idle)))
        assertEquals(SessionStatus.AgentIdle, SessionStatus.of(session(AgentState.Unknown("thinking"))))
    }

    /** 前台不是 shell 时带程序名，Windows 的 .exe 去掉；只有 shell 时是 Shell。 */
    @Test fun programsAndShells() {
        assertEquals(SessionStatus.Running("vim"), SessionStatus.of(session(foregroundIsShell = false, foreground = "vim")))
        assertEquals(
            SessionStatus.Running("nvim"),
            SessionStatus.of(session(foregroundIsShell = false, foreground = "nvim.exe")),
        )
        assertEquals(SessionStatus.Shell, SessionStatus.of(session()))
        assertEquals(SessionStatus.Shell, SessionStatus.of(session(foregroundIsShell = false, foreground = null)))
    }

    /** 退出了的会话不看 agent 和前台程序。 */
    @Test fun exitedWins() {
        assertEquals(SessionStatus.Exited, SessionStatus.of(session(AgentState.Blocked, exited = true)))
        assertEquals(
            SessionStatus.Exited,
            SessionStatus.of(session(foregroundIsShell = false, foreground = "vim", exited = true)),
        )
    }
}
