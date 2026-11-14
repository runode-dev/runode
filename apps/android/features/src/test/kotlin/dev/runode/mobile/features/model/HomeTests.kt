package dev.runode.mobile.features.model

import dev.runode.mobile.connection.HostEvent
import dev.runode.mobile.connection.LinkFailure
import dev.runode.mobile.connection.LinkState
import dev.runode.mobile.connection.MachineRecord
import dev.runode.mobile.connection.MemoryDeviceKeyStore
import dev.runode.mobile.connection.MemoryMachineStore
import dev.runode.mobile.protocol.Agent
import dev.runode.mobile.protocol.AgentKind
import dev.runode.mobile.protocol.AgentState
import dev.runode.mobile.protocol.AttachMode
import dev.runode.mobile.protocol.ClientMsg
import dev.runode.mobile.protocol.HostMsg
import dev.runode.mobile.protocol.SessionId
import dev.runode.mobile.protocol.SessionInfo
import dev.runode.mobile.protocol.SessionLink
import dev.runode.mobile.protocol.SessionMeta
import java.time.Instant
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertSame
import kotlin.test.assertTrue
import kotlinx.coroutines.test.runTest
import org.junit.Rule

private val waiting = SessionId("11111111111111111111111111111111")!!
private val working = SessionId("22222222222222222222222222222222")!!
private val plain = SessionId("33333333333333333333333333333333")!!

private fun info(id: SessionId, state: AgentState?, exited: Boolean = false) = SessionInfo(
    id,
    smallGrid,
    SessionMeta(title = id.rawValue, agent = state?.let { Agent(AgentKind("claude"), it) }, cwd = "/Users/ethan/dev"),
    exited = exited,
)

private fun lost() = HostEvent.State(LinkState.Waiting(LinkFailure.Timeout, Instant.now().plusSeconds(5)))

/** 两台电脑，各自一条假连接。 */
private class TwoMachines(
    val first: MachineRecord,
    val second: MachineRecord,
    val firstLink: FakeLink,
    val secondLink: FakeLink,
    val recents: DefaultsStore<RecentTerminal>,
    val app: AppModel,
) {
    /** 第一台连上，列出这些会话；第二台没连上。 */
    fun connectFirst(sessions: List<SessionInfo>) {
        val list = assertNotNull(app.sessionList(first.id))
        list.handle(HostEvent.State(LinkState.Connected("一", null)))
        list.handle(HostEvent.Ready(1u))
        list.handle(HostEvent.Message(HostMsg.SessionList(sessions)))
    }
}

/** [savedTitle] 不为空时，先记下第二台上的普通 shell 是上次打开的终端。 */
private suspend fun twoMachines(savedTitle: String? = null): TwoMachines {
    val first = machineRecord(name = "一", fingerprintByte = 1)
    val second = machineRecord(name = "二", fingerprintByte = 2).let { it.copy(pairedAt = it.pairedAt.plusSeconds(1)) }
    val firstLink = FakeLink()
    val secondLink = FakeLink()
    val recents = DefaultsStore<RecentTerminal>("recentTerminal")
    val store = MemoryMachineStore()
    store.upsert(first)
    store.upsert(second)
    if (savedTitle != null) recents.save(RecentTerminal(second.id, plain, savedTitle, "/tmp"))
    val links = mapOf(first.id to firstLink, second.id to secondLink)
    val app = AppModel(
        AppDependencies(
            store = store,
            keyStore = MemoryDeviceKeyStore(),
            pairing = FakePairing { second },
            makeLink = { links.getValue(it.id) },
            deviceName = "测试手机",
            recents = recents,
        ),
    )
    app.machineList.load()
    return TwoMachines(first, second, firstLink, secondLink, recents, app)
}

class HomeTests {
    @get:Rule val main = MainDispatcherRule()

    @Test fun everyPairedMachineIsConnected() = runTest {
        val machines = twoMachines()
        assertTrue(eventually { machines.firstLink.starts == 1 && machines.secondLink.starts == 1 })
        assertEquals(listOf("一", "二"), machines.app.machineLists.map { it.machine.name })
    }

    @Test fun theSummaryCountsConnectedMachinesOnly() = runTest {
        val machines = twoMachines()
        machines.connectFirst(
            listOf(
                info(waiting, AgentState.Blocked),
                info(working, AgentState.Working),
                info(plain, null),
                info(sessionA, null, exited = true),
            ),
        )
        // 第二台没连着，留下的旧列表不算。
        val second = assertNotNull(machines.app.sessionList(machines.second.id))
        second.handle(HostEvent.Ready(1u))
        second.handle(HostEvent.Message(HostMsg.SessionList(listOf(info(sessionB, AgentState.Blocked)))))
        second.handle(lost())
        assertEquals(HomeSummary(waiting = 1, working = 1, sessions = 3, connected = 1), machines.app.summary)
        assertEquals(listOf(waiting), machines.app.waitingSessions.map { it.session.id })
        assertEquals(listOf(machines.first.id), machines.app.spawnableLists.map { it.machine.id })
    }

    @Test fun waitingCardsWaitForTheFreshList() = runTest {
        val machines = twoMachines()
        machines.connectFirst(listOf(info(waiting, AgentState.Blocked)))
        assertEquals(listOf(waiting), machines.app.waitingSessions.map { it.session.id })
        // 断开再连上，新列表还没到：断开前在等的会话不当成还在等。
        val list = assertNotNull(machines.app.sessionList(machines.first.id))
        list.handle(lost())
        list.handle(HostEvent.State(LinkState.Connected("一", null)))
        list.handle(HostEvent.Ready(2u))
        assertTrue(machines.app.waitingSessions.isEmpty())
        list.handle(HostEvent.Message(HostMsg.SessionList(listOf(info(waiting, AgentState.Blocked)))))
        assertEquals(listOf(waiting), machines.app.waitingSessions.map { it.session.id })
    }

    /** 点开通知时配对页开着：关掉配对页（撤掉它的配对），终端页才看得见。 */
    @Test fun aSessionLinkClosesPairing() = runTest {
        val machines = twoMachines()
        machines.app.startPairing()
        assertNotNull(machines.app.pairing)
        machines.app.open(SessionLink(machines.first.id, plain.rawValue))
        assertNull(machines.app.pairing)
        assertEquals(
            listOf(Route.Machine(machines.first.id), Route.Terminal(machines.first.id, plain)),
            machines.app.path,
        )
        machines.app.path = emptyList()
    }

    /** 在后台时打开的终端页（通知比回到前台先到）回到前台时订阅连接，连上就 Attach。 */
    @Test fun aTerminalOpenedInTheBackgroundAttachesOnReturn() = runTest {
        val machines = twoMachines()
        val link = machines.firstLink
        assertTrue(eventually { link.subscriberCount == 1 })
        machines.app.setActive(false)
        val terminal = assertNotNull(machines.app.terminal(machines.first.id, plain))
        machines.app.setActive(true)
        // 列表重新订阅一次，终端页订阅一次。
        assertTrue(eventually { link.subscriberCount == 3 })
        link.emit(HostEvent.Ready(2u))
        assertTrue(eventually { link.sent.any { it is ClientMsg.Attach && it.id == plain && it.mode == AttachMode.VtReplay } })
        terminal.close()
    }

    @Test fun openingATerminalIsRememberedForResume() = runTest {
        val machines = twoMachines()
        machines.connectFirst(listOf(info(waiting, AgentState.Blocked), info(plain, null)))
        machines.app.openTerminal(machines.first.id, plain)
        val page = listOf(Route.Machine(machines.first.id), Route.Terminal(machines.first.id, plain))
        assertEquals(page, machines.app.path)
        assertEquals(RecentTerminal(machines.first.id, plain, plain.rawValue, "/Users/ethan/dev"), machines.recents.load())
        machines.app.path = emptyList()
        assertEquals(plain, machines.app.resume?.session)
        machines.app.resumeRecent()
        assertEquals(page, machines.app.path)
        // 会话结束了，「上次打开」就不出现。
        machines.app.path = emptyList()
        val list = assertNotNull(machines.app.sessionList(machines.first.id))
        list.handle(HostEvent.Message(HostMsg.SessionList(listOf(info(waiting, AgentState.Blocked)))))
        assertNull(machines.app.resume)
    }

    /** 还没连上时「上次打开」用记下的标题；连上后列表里没有这个会话（结束了）就不显示。 */
    @Test fun resumeShowsTheSavedTitleBeforeConnecting() = runTest {
        val machines = twoMachines(savedTitle = "上次的")
        assertEquals("上次的", machines.app.resume?.title.english)
        assertEquals(machines.second.id, machines.app.resume?.machine?.id)
        val list = assertNotNull(machines.app.sessionList(machines.second.id))
        list.handle(HostEvent.Ready(1u))
        list.handle(HostEvent.Message(HostMsg.SessionList(listOf(info(sessionA, null)))))
        assertNull(machines.app.resume)
        // 那台电脑删掉了也不显示。
        val other = twoMachines(savedTitle = "上次的")
        other.app.machineList.delete(other.second.id)
        assertNull(other.app.resume)
    }

    /** 新开的会话开好以后打开它：在会话列表上开的压在列表上面，首页开的连同列表一起换上。 */
    @Test fun spawnedSessionsOpen() = runTest {
        val machines = twoMachines()
        machines.connectFirst(emptyList())
        val list = assertNotNull(machines.app.sessionList(machines.first.id))
        list.spawn()
        val req = assertIs<ClientMsg.Open>(machines.firstLink.sent.last()).req
        list.handle(HostEvent.Message(HostMsg.Opened(req, sessionA)))
        assertEquals(
            listOf(Route.Machine(machines.first.id), Route.Terminal(machines.first.id, sessionA)),
            machines.app.path,
        )

        machines.app.path = listOf(Route.Machine(machines.first.id))
        list.spawn()
        val next = assertIs<ClientMsg.Open>(machines.firstLink.sent.last()).req
        list.handle(HostEvent.Message(HostMsg.Opened(next, sessionB)))
        assertEquals(
            listOf(Route.Machine(machines.first.id), Route.Terminal(machines.first.id, sessionB)),
            machines.app.path,
        )
        machines.app.path = emptyList()
    }

    @Test fun renamingUpdatesTheConnectedList() = runTest {
        val machines = twoMachines()
        val list = assertNotNull(machines.app.sessionList(machines.first.id))
        machines.app.machineList.rename(machines.first.id, "新名字")
        assertEquals("新名字", list.machine.name)
        assertSame(list, machines.app.sessionList(machines.first.id))
    }

    @Test fun machineCardsSummariseTheirSessions() {
        val connected = LinkState.Connected("homelab", null)
        fun line(state: LinkState, sessions: List<SessionInfo>, loaded: Boolean = true) =
            Presentation.machineLine(state, sessions, loaded)
        fun MachineLine.english() = (emphasis.english ?: "") + text.english

        val waitingLine = line(connected, listOf(info(waiting, AgentState.Blocked), info(working, AgentState.Working), info(plain, null)))
        assertEquals(MachineLine.Tone.Waiting, waitingLine.tone)
        assertEquals("1 waiting, 3 sessions in all", waitingLine.english())
        val workingLine = line(connected, listOf(info(working, AgentState.Working), info(plain, null)))
        assertEquals(MachineLine.Tone.Working, workingLine.tone)
        assertEquals("1 working", workingLine.emphasis.english)
        assertEquals(", 2 sessions in all", workingLine.text.english)
        line(connected, listOf(info(plain, null))).let {
            assertEquals(MachineLine.Tone.Neutral, it.tone)
            assertNull(it.emphasis)
            assertEquals("1 session", it.text.english)
        }
        assertEquals("No terminals", line(connected, listOf(info(plain, null, exited = true))).english())
        assertEquals("Connected", line(connected, emptyList(), loaded = false).english())
        assertEquals("Not connected", line(LinkState.Idle, listOf(info(plain, null))).english())
        line(LinkState.Failed(LinkFailure.GateVersion(9u)), emptyList()).let {
            assertEquals(MachineLine.Tone.Failure, it.tone)
            assertEquals("Can’t connect", it.emphasis.english)
            assertEquals("", it.text.english)
        }
    }

    @Test fun homeTitleLeadsWithWhatNeedsYou() {
        fun title(summary: HomeSummary) = Presentation.homeTitle(summary).english to Presentation.homeDetail(summary).english
        assertEquals(
            "2 sessions are waiting for you" to "1 more working",
            title(HomeSummary(waiting = 2, working = 1, sessions = 4, connected = 1)),
        )
        assertEquals("1 session is waiting for you" to null, title(HomeSummary(waiting = 1, sessions = 4, connected = 1)))
        assertEquals("3 sessions are working" to null, title(HomeSummary(working = 3, sessions = 4, connected = 2)))
        assertEquals(
            "Nothing is waiting for you" to "4 sessions in all",
            title(HomeSummary(sessions = 4, connected = 1)),
        )
        assertEquals("Nothing is waiting for you" to "No terminals", title(HomeSummary(connected = 1)))
        assertEquals("No computer connected" to null, title(HomeSummary()))
    }

    @Test fun waitingCardsPageInALoop() {
        val ids = listOf("a", "b", "c")
        assertEquals("a", HomePager.current(null, ids))
        assertEquals("b", HomePager.current("b", ids))
        assertEquals("b", HomePager.next("a", ids))
        assertEquals("a", HomePager.next("c", ids))
        assertEquals("a", HomePager.previous("b", ids))
        assertEquals("c", HomePager.previous("a", ids))
        assertEquals("a", HomePager.next("a", listOf("a")))
        assertNull(HomePager.current(null, emptyList()))
        assertNull(HomePager.next(null, emptyList()))
    }

    /** 翻到的那张答完、不在列表里了，退回第一张，再往后翻从第一张接着翻。 */
    @Test fun waitingCardsFallBackToTheFirstWhenTheFocusedOneIsGone() {
        val ids = listOf("a", "c")
        assertEquals("a", HomePager.current("b", ids))
        assertEquals("c", HomePager.next(HomePager.current("b", ids), ids))
        assertEquals("a", HomePager.next("b", ids))
        assertEquals("a", HomePager.previous("b", ids))
    }
}
