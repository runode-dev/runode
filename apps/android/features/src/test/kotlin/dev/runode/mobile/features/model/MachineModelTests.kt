package dev.runode.mobile.features.model

import dev.runode.mobile.connection.HostEvent
import dev.runode.mobile.connection.LinkFailure
import dev.runode.mobile.connection.LinkState
import dev.runode.mobile.connection.MachineRecord
import dev.runode.mobile.connection.MachineStore
import dev.runode.mobile.connection.MemoryDeviceKeyStore
import dev.runode.mobile.connection.MemoryMachineStore
import dev.runode.mobile.features.R
import dev.runode.mobile.protocol.Agent
import dev.runode.mobile.protocol.AgentKind
import dev.runode.mobile.protocol.AgentState
import dev.runode.mobile.protocol.Base64URL
import dev.runode.mobile.protocol.ClientMsg
import dev.runode.mobile.protocol.GateRejection
import dev.runode.mobile.protocol.HostMsg
import dev.runode.mobile.protocol.SessionId
import dev.runode.mobile.protocol.SessionInfo
import dev.runode.mobile.protocol.SessionMeta
import java.io.File
import java.io.IOException
import java.time.Instant
import java.util.UUID
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertNotSame
import kotlin.test.assertNull
import kotlin.test.assertSame
import kotlin.test.assertTrue
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Rule

/** 读得出、写不进的电脑列表。 */
class UnwritableMachineStore : MachineStore {
    val memory = MemoryMachineStore()

    override suspend fun all(): List<MachineRecord> = memory.all()

    override suspend fun upsert(machine: MachineRecord) = throw IOException("write failed")

    override suspend fun updateLastAddress(id: UUID, address: String) = throw IOException("write failed")

    override suspend fun updateNode(id: UUID, node: String) = throw IOException("write failed")

    override suspend fun updateName(id: UUID, name: String) = throw IOException("write failed")

    override suspend fun remove(id: UUID) = throw IOException("write failed")
}

class MachineListModelTests {
    @get:Rule val main = MainDispatcherRule()

    @Test fun addRenameDelete() = runTest {
        val store = MemoryMachineStore()
        val keys = MemoryDeviceKeyStore()
        val model = MachineListModel(store, keys)
        val deleted = mutableListOf<UUID>()
        model.willDelete = {
            deleted += it
            true
        }
        val machine = machineRecord()
        keys.generate(machine.id)
        model.add(machine)
        assertEquals(listOf(machine.id), model.machines.map { it.id })

        model.beginRename(machine.id)
        assertTrue(model.isRenaming)
        assertEquals("MacBook", model.renameText)
        model.rename(machine.id, "  工作机  ")
        assertEquals("工作机", model.machines.first().name)
        model.rename(machine.id, "   ")
        assertEquals("工作机", model.machines.first().name)

        model.delete(machine.id)
        assertTrue(model.machines.isEmpty())
        assertEquals(listOf(machine.id), deleted)
        assertNull(keys.key(machine.id))
    }

    /** 改名只改名字：连接在列表不知道时记下的 node、lastAddress 不被列表里的旧副本盖掉。 */
    @Test fun renamingKeepsWhatTheConnectionLearned() = runTest {
        val store = MemoryMachineStore()
        val model = MachineListModel(store, MemoryDeviceKeyStore())
        val machine = machineRecord()
        model.add(machine)
        store.updateNode(machine.id, "learned-node")
        store.updateLastAddress(machine.id, "10.0.0.9")
        model.rename(machine.id, "工作机")
        val stored = store.all().first()
        assertEquals("工作机", stored.name)
        assertEquals("learned-node", stored.node)
        assertEquals("10.0.0.9", stored.lastAddress)
    }

    @Test fun repairingTheSameMachineReplacesTheOldRecord() = runTest {
        val store = MemoryMachineStore()
        val keys = MemoryDeviceKeyStore()
        val model = MachineListModel(store, keys)
        val old = machineRecord(name = "旧")
        keys.generate(old.id)
        model.add(old)
        model.add(machineRecord(name = "新"))
        assertEquals(listOf("新"), model.machines.map { it.name })
        assertNull(keys.key(old.id))
    }

    /** 重新配对的记录存不下：旧的配对连同私钥留着，新存的私钥丢掉，错误交给配对页。 */
    @Test fun aFailedRepairKeepsTheOldPairing() = runTest {
        val store = UnwritableMachineStore()
        val keys = MemoryDeviceKeyStore()
        val model = MachineListModel(store, keys)
        val deleted = mutableListOf<UUID>()
        model.willDelete = {
            deleted += it
            true
        }
        val old = machineRecord(name = "旧")
        store.memory.upsert(old)
        keys.generate(old.id)
        model.load()
        val new = machineRecord(name = "新")
        keys.generate(new.id)
        assertFailsWith<IOException> { model.add(new) }
        assertEquals(listOf("旧"), model.machines.map { it.name })
        assertTrue(deleted.isEmpty())
        assertNotNull(keys.key(old.id))
        assertNull(keys.key(new.id))
    }

    /** 电脑那边没注销好推送：记录和私钥留着、记下删掉的时刻，列表里不再显示；注销好了（purge）才真删。 */
    @Test fun aMachineNotUnregisteredIsHiddenUntilPurged() = runTest {
        val store = MemoryMachineStore()
        val keys = MemoryDeviceKeyStore()
        val model = MachineListModel(store, keys)
        model.willDelete = { false }
        val machine = machineRecord()
        keys.generate(machine.id)
        model.add(machine)
        model.delete(machine.id)
        assertTrue(model.machines.isEmpty())
        assertEquals(listOf(machine.id), model.leaving.map { it.id })
        assertNotNull(store.all().first().deletedAt)
        assertNotNull(keys.key(machine.id))
        // 重新读一遍（App 重启）也还藏着。
        model.load()
        assertTrue(model.machines.isEmpty())
        model.purge(machine.id)
        assertTrue(model.leaving.isEmpty())
        assertTrue(store.all().isEmpty())
        assertNull(keys.key(machine.id))
    }

    /** 删掉一个月还没注销成的，读列表时真删掉。 */
    @Test fun aMachineLeftForAMonthIsPurged() = runTest {
        val store = MemoryMachineStore()
        val keys = MemoryDeviceKeyStore()
        val model = MachineListModel(store, keys)
        val old = machineRecord().copy(deletedAt = Instant.now().minus(MachineListModel.leavingLimit).minusSeconds(60))
        val recent = machineRecord().copy(deletedAt = Instant.now().minusSeconds(60))
        store.upsert(old)
        store.upsert(recent)
        keys.generate(old.id)
        model.load()
        assertEquals(listOf(recent.id), model.leaving.map { it.id })
        assertEquals(listOf(recent.id), store.all().map { it.id })
        assertNull(keys.key(old.id))
    }

    /** 删不掉记录：电脑和私钥都留着，照样能连，错误交给设置页。 */
    @Test fun aFailedDeleteKeepsTheMachine() = runTest {
        val store = UnwritableMachineStore()
        val keys = MemoryDeviceKeyStore()
        val model = MachineListModel(store, keys)
        val machine = machineRecord()
        store.memory.upsert(machine)
        keys.generate(machine.id)
        model.load()
        model.delete(machine.id)
        assertEquals(listOf(machine.id), model.machines.map { it.id })
        assertNotNull(keys.key(machine.id))
        assertTrue(model.errorMessage.english!!.startsWith("Couldn't delete this computer"))
    }
}

class PairingModelTests {
    @get:Rule val main = MainDispatcherRule()

    private val fingerprint = Base64URL.encode(ByteArray(32) { 1 })
    private val secret = Base64URL.encode(ByteArray(32) { 2 })
    private val pairingTime = { Instant.ofEpochSecond(1_800_000_000) }

    private fun link(exp: Long = 1_900_000_000) =
        "runode://pair?v=1&name=homelab&fp=$fingerprint&secret=$secret&port=7866&addr=192.168.1.20&exp=$exp"

    @Test fun pairsAndHandsTheMachineOver() = runTest {
        val paired = mutableListOf<MachineRecord>()
        val machine = machineRecord()
        val model = PairingModel(
            FakePairing { invitation ->
                assertEquals("homelab", invitation.hostName)
                machine
            },
            "测试手机",
            pairingTime,
        ) { paired += it }
        model.linkText = link()
        model.submitLink()
        assertEquals(PairingModel.Phase.Paired(machine), model.phase)
        assertEquals(listOf(machine), paired)
        // 配好以后再扫到码也不再配。
        model.scanned(link())
        assertEquals(1, paired.size)
    }

    /** 配好后回到扫码，镜头里还是刚才那张码：不重配同一台；换一张码照常配。 */
    @Test fun goingBackToScanDoesNotPairTheSameCodeAgain() = runTest {
        val paired = mutableListOf<MachineRecord>()
        val model = PairingModel(FakePairing { machineRecord() }, "x", pairingTime) { paired += it }
        model.scanned(link())
        assertEquals(1, paired.size)
        model.reset()
        model.scanned(link())
        assertEquals(PairingModel.Phase.Idle, model.phase)
        assertEquals(1, paired.size)
        model.scanned(link(exp = 1_900_000_001))
        assertEquals(2, paired.size)
    }

    /** 配对前给用户看的电脑名和地址从输入框里的链接解析出来；不是配对链接时没有。 */
    @Test fun theInvitationIsShownBeforePairing() {
        val model = PairingModel(FakePairing { machineRecord() }, "x") {}
        assertNull(model.invitation)
        model.linkText = link()
        assertEquals("homelab", model.invitation?.hostName)
        assertEquals(listOf("192.168.1.20"), model.invitation?.addresses)
        assertEquals(7866u.toUShort(), model.invitation?.port)
        assertEquals(PairingModel.Phase.Idle, model.phase)
        model.linkText = "https://example.com"
        assertNull(model.invitation)
    }

    @Test fun badAndExpiredLinksFailWithoutPairing() = runTest {
        val model = PairingModel(
            FakePairing { throw AssertionError("should not pair") },
            "x",
            { Instant.ofEpochSecond(1_950_000_000) },
        ) {}
        model.linkText = "https://example.com"
        model.submitLink()
        assertEquals("This isn't a Runode pairing link", failure(model))
        model.linkText = link()
        model.submitLink()
        assertEquals("The QR code has expired. Generate a new one on your computer.", failure(model))
    }

    /** 配好了却存不下：不算配好，不给「完成」。 */
    @Test fun aMachineThatCannotBeSavedIsNotPaired() = runTest {
        val model = PairingModel(FakePairing { machineRecord() }, "x", pairingTime) { throw IOException("write failed") }
        model.scanned(link())
        assertTrue(failure(model).startsWith("Couldn't save this computer"))
    }

    /** 配对页关掉以后配好的不存，为它存的私钥也删掉。 */
    @Test fun closingThePageDropsThePairing() = runTest {
        val paired = mutableListOf<MachineRecord>()
        val gate = Channel<Unit>(Channel.UNLIMITED)
        val gated = GatedPairing(gate)
        val model = PairingModel(gated, "x", pairingTime) { paired += it }
        val pairing = launch { model.scanned(link()) }
        assertTrue(eventually { model.isBusy })
        model.cancel()
        gate.trySend(Unit)
        pairing.join()
        assertTrue(eventually { gated.keys.key(gated.machine.id) == null }, "the key saved for the dropped pairing is gone")
        assertTrue(paired.isEmpty())
        assertFalse(model.phase is PairingModel.Phase.Paired, "a closed page should not reach paired")
        model.scanned(link())
        assertTrue(paired.isEmpty(), "a closed page does not pair again")
    }

    /** 配对途中发起它的界面被撤掉（Activity 重建），页面没关：配好的照样存下，页面落到配好了。 */
    @Test fun aPairingOutlivesTheCallerThatStartedIt() = runTest {
        val paired = mutableListOf<MachineRecord>()
        val gate = Channel<Unit>(Channel.UNLIMITED)
        val gated = GatedPairing(gate)
        val model = PairingModel(gated, "x", pairingTime) { paired += it }
        val caller = launch { model.scanned(link()) }
        assertTrue(eventually { model.isBusy })
        caller.cancel()
        caller.join()
        gate.trySend(Unit)
        assertTrue(eventually { model.phase is PairingModel.Phase.Paired }, "the pairing should still finish")
        assertEquals(listOf(gated.machine.id), paired.map { it.id })
        assertNotNull(gated.keys.key(gated.machine.id))
    }

    /** 没关页面时配好的照常存，私钥留着。 */
    @Test fun aKeptPairingKeepsItsKey() = runTest {
        val paired = mutableListOf<MachineRecord>()
        val gate = Channel<Unit>(Channel.UNLIMITED)
        val gated = GatedPairing(gate)
        val model = PairingModel(gated, "x", pairingTime) { paired += it }
        gate.trySend(Unit)
        model.scanned(link())
        assertEquals(listOf(gated.machine.id), paired.map { it.id })
        assertNotNull(gated.keys.key(gated.machine.id))
    }

    @Test fun rejectionsAreExplained() = runTest {
        val model = PairingModel(
            FakePairing { throw LinkFailure.Rejected(GateRejection.PairingInvalid) },
            "x",
            pairingTime,
        ) {}
        model.scanned(link())
        assertEquals(
            "The pairing code is wrong, expired, or already used. Generate a new QR code on your computer.",
            failure(model),
        )
        model.reset()
        assertEquals(PairingModel.Phase.Idle, model.phase)
    }

    private fun failure(model: PairingModel): String = assertIs<PairingModel.Phase.Failed>(model.phase).message.english!!
}

class AppModelTests {
    @get:Rule val main = MainDispatcherRule()

    private fun app(store: MemoryMachineStore, machine: MachineRecord, makeLink: (MachineRecord) -> FakeLink) = AppModel(
        AppDependencies(
            store = store,
            keyStore = MemoryDeviceKeyStore(),
            pairing = FakePairing { machine },
            makeLink = makeLink,
            deviceName = "测试手机",
        ),
    )

    /** 配对过的电脑一读进来就连上，进出它的页面不断开；终端页退出去就关掉。 */
    @Test fun pairedMachinesStayConnected() = runTest {
        val store = MemoryMachineStore()
        val machine = machineRecord()
        store.upsert(machine)
        val link = FakeLink()
        val app = app(store, machine) { link }
        app.machineList.load()
        assertTrue(eventually { link.starts == 1 })
        val list = assertNotNull(app.sessionList(machine.id))
        app.path = listOf(Route.Machine(machine.id), Route.Terminal(machine.id, sessionA))
        val terminal = assertNotNull(app.terminal(machine.id, sessionA))
        assertSame(terminal, app.terminal(machine.id, sessionA))
        assertSame(link, list.link)
        app.path = emptyList()
        assertNotSame(terminal, app.terminal(machine.id, sessionA))
        assertSame(list, app.sessionList(machine.id))
        delay(20)
        assertEquals(0, link.stops)
        // 删掉这台电脑才断开。
        app.machineList.delete(machine.id)
        assertTrue(eventually { link.stops == 1 })
        app.path = emptyList()
    }

    /** 显示的分屏结束了：切回同一个标签里上次看过、还没结束的分屏；看过的都结束了时切到第一个还没结束的。 */
    @Test fun anEndedPaneReturnsToTheLastSeenPaneOfItsTab() = runTest {
        val store = MemoryMachineStore()
        val machine = machineRecord()
        store.upsert(machine)
        val app = app(store, machine) { FakeLink() }
        app.machineList.load()
        val list = assertNotNull(app.sessionList(machine.id))
        fun info(id: SessionId, exited: Boolean = false) = SessionInfo(id, smallGrid, SessionMeta(title = "zsh"), exited = exited)
        list.handle(HostEvent.Ready(1u))
        list.handle(HostEvent.Message(HostMsg.SessionList(listOf(info(sessionA), info(sessionB), info(sessionC)))))
        app.path = listOf(Route.Machine(machine.id), Route.Terminal(machine.id, sessionA))
        // 在 A 上跑命令，开出来的 C 换掉了这一页。
        app.openTerminal(machine.id, sessionC)
        app.leaveEndedPane(machine.id, sessionC, listOf(sessionB, sessionA, sessionC))
        assertEquals(sessionA, app.shownSession(machine.id, sessionC))

        list.handle(
            HostEvent.Message(HostMsg.SessionList(listOf(info(sessionA, exited = true), info(sessionB), info(sessionC)))),
        )
        app.leaveEndedPane(machine.id, sessionA, listOf(sessionB, sessionA, sessionC))
        assertEquals(sessionC, app.shownSession(machine.id, sessionC))
        // 标签里只剩它自己时不动。
        app.leaveEndedPane(machine.id, sessionC, listOf(sessionC))
        assertEquals(sessionC, app.shownSession(machine.id, sessionC))
        app.path = emptyList()
    }

    /** 切到同一个标签里的另一个分屏：导航栈不动，栈顶的终端页改显示新会话，旧终端关掉。 */
    @Test fun switchingPanesReplacesTheTerminalPage() = runTest {
        val store = MemoryMachineStore()
        val machine = machineRecord()
        store.upsert(machine)
        val app = app(store, machine) { FakeLink() }
        app.machineList.load()
        val page = Route.Terminal(machine.id, sessionA)
        app.path = listOf(Route.Machine(machine.id), page)
        val first = assertNotNull(app.terminal(machine.id, sessionA))
        first.keyboardVisible = true
        app.switchTerminal(machine.id, sessionB)
        assertEquals(listOf(Route.Machine(machine.id), page), app.path)
        assertEquals(sessionB, app.shownSession(machine.id, sessionA))
        assertNotSame(first, app.terminal(machine.id, sessionA))
        assertEquals(true, app.terminal(machine.id, sessionB)?.keyboardVisible)
        app.switchTerminal(machine.id, sessionA)
        assertEquals(sessionA, app.shownSession(machine.id, sessionA))
        // 不在终端页上时不动。
        app.path = listOf(Route.Machine(machine.id))
        app.switchTerminal(machine.id, sessionA)
        assertEquals(listOf<Route>(Route.Machine(machine.id)), app.path)
    }

    @Test fun backgroundDisconnectsAndForegroundReconnects() = runTest {
        val store = MemoryMachineStore()
        val machine = machineRecord()
        store.upsert(machine)
        val link = FakeLink()
        val app = app(store, machine) { link }
        app.machineList.load()
        assertTrue(eventually { link.starts == 1 })
        app.setActive(false)
        assertTrue(eventually { link.stops == 1 })
        app.setActive(true)
        assertTrue(eventually { link.starts == 2 })
        app.setActive(false)
    }

    /** 导航栈里有页面的电脑算在看，连不上时重试得勤；回到首页时都不算。 */
    @Test fun machinesOnThePathAreFocused() = runTest {
        val store = MemoryMachineStore()
        val machine = machineRecord()
        val other = machineRecord(name = "Mac mini", fingerprintByte = 2)
        store.upsert(machine)
        store.upsert(other)
        val link = FakeLink()
        val otherLink = FakeLink()
        val app = app(store, machine) { if (it.id == machine.id) link else otherLink }
        app.machineList.load()
        assertEquals(false, link.focused.last())
        assertEquals(false, otherLink.focused.last())
        app.openTerminal(machine.id, sessionA)
        assertEquals(true, link.focused.last())
        assertEquals(false, otherLink.focused.last())
        app.path = emptyList()
        assertEquals(false, link.focused.last())
    }

    @Test fun deletingAMachineLeavesItsPages() = runTest {
        val store = MemoryMachineStore()
        val machine = machineRecord()
        store.upsert(machine)
        val app = app(store, machine) { FakeLink() }
        app.machineList.load()
        app.path = listOf(Route.Machine(machine.id))
        app.sessionList(machine.id)
        app.machineList.delete(machine.id)
        assertTrue(app.path.isEmpty())
        assertTrue(app.machineList.machines.isEmpty())
    }
}

class PresentationTests {
    /** 旁边写着的电脑名字和连上的那台报的一样时，状态里不再重复名字。 */
    @Test fun linkStateSkipsTheNameAlreadyShown() {
        val state = LinkState.Connected("Mac mini", null)
        assertEquals("Connected", Presentation.linkState(state, "Mac mini").english)
        assertEquals("Connected to Mac mini", Presentation.linkState(state, "办公室").english)
        assertEquals("Connected to Mac mini", Presentation.linkState(state).english)
    }

    @Test fun directoriesUnderHomeAreShortened() {
        assertEquals("~/dev/runode", Presentation.directory("/Users/ethan/dev/runode"))
        assertEquals("~", Presentation.directory("/Users/ethan"))
        assertEquals("/tmp", Presentation.directory("/tmp"))
        assertNull(Presentation.directory(null))
    }

    @Test fun sectionDetailSkipsADirectoryThatRepeatsTheTitle() {
        val home = SessionSection(SessionSection.Id.Workspace(1u, 0u), "~", "/Users/ethan", sessions = emptyList())
        assertNull(Presentation.sectionDetail(home))
        val second = home.copy(window = 2u)
        assertEquals("Window 2", Presentation.sectionDetail(second).english)
        val repo = SessionSection(
            SessionSection.Id.Workspace(1u, 1u), "runode", "/Users/ethan/dev/runode", sessions = emptyList(),
        )
        assertEquals("~/dev/runode", Presentation.sectionDetail(repo).english)
        assertNull(Presentation.sectionDirectory(home))
        assertNull(Presentation.sectionWindow(home))
        assertEquals("~/dev/runode", Presentation.sectionDirectory(repo))
        assertEquals("Window 2", Presentation.sectionWindow(second).english)
        val background = SessionSection(SessionSection.Id.Background, sessions = emptyList())
        assertNull(Presentation.sectionDirectory(background))
        assertNull(Presentation.sectionWindow(background))
    }

    @Test fun programLabelsDropTheExeSuffix() {
        assertEquals("powershell", Presentation.programLabel("powershell.exe"))
        assertEquals("Code", Presentation.programLabel("Code.EXE"))
        assertEquals("zsh", Presentation.programLabel("zsh"))
        assertEquals(".exe", Presentation.programLabel(".exe"))
    }

    @Test fun pairingListsTwoAddressesIPv4First() {
        assertEquals("10.0.0.2:7866", Presentation.pairingAddresses(listOf("10.0.0.2"), 7866u).english)
        assertEquals(
            "10.0.0.2:7866, 192.168.1.5:7866 and more (3 addresses)",
            Presentation.pairingAddresses(listOf("fd00::1", "10.0.0.2", "192.168.1.5"), 7866u).english,
        )
        assertEquals(
            "10.0.0.2:7866, [fd00::1]:7866",
            Presentation.pairingAddresses(listOf("fd00::1", "10.0.0.2"), 7866u).english,
        )
    }

    @Test fun sessionDirectoriesSkipTheWorkspaceDirectory() {
        val ws = "/Users/ethan/dev/runode"
        assertNull(Presentation.sessionDirectory(ws, ws))
        assertNull(Presentation.sessionDirectory(ws, "$ws/"))
        assertEquals("./apps/ios", Presentation.sessionDirectory("$ws/apps/ios", ws))
        assertEquals("~/dev/runode-old", Presentation.sessionDirectory("/Users/ethan/dev/runode-old", ws))
        assertEquals("/tmp", Presentation.sessionDirectory("/tmp", null))
        assertNull(Presentation.sessionDirectory(null, ws))
    }

    /** 在等、在干活且满一分钟时说等了、干了多久，否则是名字加状态词；空闲不写时长。 */
    @Test fun agentStatesReadNaturally() {
        val now = Instant.ofEpochMilli(10_000_000)
        fun status(kind: String, state: AgentState, sinceMs: Long? = null) =
            Presentation.agentStatus(Agent(AgentKind(kind), state), sinceMs, now)?.first.english
        assertEquals("Claude Code · Working", status("claude", AgentState.Working))
        assertEquals("Codex · Idle", status("codex", AgentState.Idle))
        assertEquals("new · Waiting for You", status("new", AgentState.Blocked))
        assertNull(Presentation.agentStatus(Agent(AgentKind("x"), AgentState.Unknown("z")), null, now))
        val fiveMinutes = now.toEpochMilli() - 5 * 60_000
        assertEquals("Claude Code has been working for 5 minutes", status("claude", AgentState.Working, fiveMinutes))
        assertEquals("Claude Code has been waiting for 5 minutes", status("claude", AgentState.Blocked, fiveMinutes))
        assertEquals("Codex · Idle", status("codex", AgentState.Idle, fiveMinutes))
        assertEquals("Claude Code · Working", status("claude", AgentState.Working, now.toEpochMilli() - 59_999))
    }

    /** 时长只取小时或分钟里最大的一个、往下取整；不满一分钟、时刻在手机的现在之后时没有。 */
    @Test fun agentElapsedKeepsTheLargestUnit() {
        val now = Instant.ofEpochMilli(100_000_000)
        fun elapsed(ms: Long) = Presentation.agentElapsed(now.toEpochMilli() - ms, now)
        assertNull(Presentation.agentElapsed(null, now))
        assertNull(elapsed(59_999))
        assertNull(elapsed(-60_000))
        assertEquals("1 minute", elapsed(60_000).english)
        assertEquals("59 minutes", elapsed(3_599_999).english)
        assertEquals("1 hour", elapsed(3_600_000).english)
        assertEquals("2 hours", elapsed(3 * 3_600_000 - 1).english)
        assertEquals("30 hours", elapsed(30 * 3_600_000L).english)
        assertEquals("12分钟", elapsed(12 * 60_000L).simplifiedChinese)
        assertEquals("2小时", elapsed(2 * 3_600_000L).simplifiedChinese)
    }

    /** 重画的时刻落在从 agent 进入这个状态起的整分钟上。 */
    @Test fun agentClockTicksOnWholeSteps() {
        assertEquals(1_060_000, AgentClock.following(1_000_000, 1_000_000))
        assertEquals(1_120_000, AgentClock.following(1_060_000, 1_000_000))
        assertEquals(1_120_000, AgentClock.following(1_119_999, 1_000_000))
        assertEquals(1_060_000, AgentClock.following(900_000, 1_000_000))
        assertTrue(AgentClock.ticks(Agent(AgentKind("claude"), AgentState.Blocked), 1))
        assertFalse(AgentClock.ticks(Agent(AgentKind("claude"), AgentState.Idle), 1))
        assertFalse(AgentClock.ticks(Agent(AgentKind("claude"), AgentState.Working), null))
    }

    @Test fun sizeOwners() {
        assertEquals("Size set by Ethan 的 MacBook", Presentation.sizeOwner("Ethan 的 MacBook").english)
        assertEquals("Size not set by anyone", Presentation.sizeOwner(null).english)
        assertEquals("Size set by this phone", Presentation.sizeOwnership(SizeOwnership.Mine).english)
    }

    /** 收了 logo 的各家都对得上打包的图，打包的图也都有一家在用。宿主认得的各家取自 `runode_protocol` 的样例。 */
    @Test fun agentLogosMatchTheBundledImages() {
        val fixture = File(System.getProperty("runode.repo"), "crates/protocol/tests/fixtures/messages.json")
        val labels = Json.parseToJsonElement(fixture.readText()).jsonObject.getValue("agents").jsonArray
            .map { it.jsonObject.getValue("kind").jsonPrimitive.content }
        assertTrue("claude" in labels)
        val bundled = R.drawable::class.java.fields.filter { it.name.startsWith("agent_logo_") }.map { it.getInt(null) }.toSet()
        val mapped = labels.mapNotNull { Presentation.agentLogo(AgentKind(it))?.drawable }.toSet()
        assertEquals(bundled, mapped)
        assertNull(Presentation.agentLogo(AgentKind("aider")))
        // 按原色画、不染色的是这一组，别的 logo 都是单色的。
        val colored = labels.filter { Presentation.agentLogo(AgentKind(it))?.colored == true }.toSet()
        val expected = setOf(
            "claude", "gemini", "devin", "antigravity", "kimi", "kiro", "amp", "qwen", "junie", "trae", "code_buddy", "mistral_vibe",
            "muse",
        )
        assertEquals(expected, colored)
    }
}

class DirectoryPickerTests {
    @get:Rule val main = MainDispatcherRule()

    private val link = FakeLink()

    private fun lastList(): ClientMsg.ListDirs = assertIs<ClientMsg.ListDirs>(link.sent.last())

    private fun listed(path: String, dirs: List<String>): DirectoryPickerModel {
        val picker = DirectoryPickerModel(link)
        picker.load(null)
        val list = lastList()
        assertNull(list.path)
        assertTrue(picker.isLoading)
        assertTrue(picker.handle(HostMsg.Dirs(list.req, path, dirs, false)))
        return picker
    }

    @Test fun startsAtHomeAndHidesDotDirectories() {
        val picker = listed("/Users/ethan", listOf(".config", "dev", "中文"))
        assertFalse(picker.isLoading)
        assertEquals("/Users/ethan", picker.path)
        assertEquals(listOf("dev", "中文"), picker.visibleDirs)
        picker.showsHidden = true
        assertEquals(listOf(".config", "dev", "中文"), picker.visibleDirs)
        assertEquals("/Users", picker.parent)
    }

    @Test fun entersAndGoesUp() {
        val picker = listed("/Users/ethan", listOf("dev"))
        picker.enter("dev")
        assertEquals("/Users/ethan/dev", lastList().path)
        picker.goUp()
        assertEquals("/Users", lastList().path)
    }

    /** 只认最后一次请求的回话：点得快时前面的回话丢掉。 */
    @Test fun onlyTheLatestReplyCounts() {
        val picker = listed("/Users/ethan", listOf("a", "b"))
        picker.enter("a")
        val first = lastList().req
        picker.enter("b")
        val second = lastList().req
        assertFalse(picker.handle(HostMsg.Dirs(first, "/Users/ethan/a", emptyList(), false)))
        assertTrue(picker.handle(HostMsg.Dirs(second, "/Users/ethan/b", listOf("x"), true)))
        assertEquals("/Users/ethan/b", picker.path)
        assertTrue(picker.truncated)
    }

    @Test fun errorsKeepTheLastListingAndCanRetry() {
        val picker = listed("/Users/ethan", listOf("secret"))
        picker.enter("secret")
        val (req, path) = lastList().let { it.req to it.path }
        assertTrue(picker.handle(HostMsg.Error(req, null, "permission denied")))
        assertEquals("permission denied", picker.errorMessage.english)
        assertEquals("/Users/ethan", picker.path)
        // 错误层盖着列表，path 还是上次的目录：这时不能在这里新建，收起提示回到那个目录后才能。
        assertFalse(picker.canChoose)
        picker.dismissError()
        assertTrue(picker.canChoose)
        picker.enter("secret")
        assertTrue(picker.handle(HostMsg.Error(lastList().req, null, "permission denied")))
        picker.retry()
        assertEquals(path, lastList().path)
        assertNull(picker.errorMessage)
    }

    /** 连接断了：在等的回话不会来了，不再转圈、报出错；断着时不发请求，重新连上后能重试。 */
    @Test fun losingTheConnectionStopsTheSpinner() {
        val model = SessionListModel(machineRecord(), link)
        model.handle(HostEvent.Ready(1u))
        model.beginNewWorkspace()
        val picker = assertNotNull(model.directoryPicker)
        picker.load(null)
        assertTrue(picker.isLoading)
        model.handle(HostEvent.State(LinkState.Waiting(LinkFailure.Timeout, Instant.now())))
        assertFalse(picker.isLoading)
        assertEquals("Not connected", picker.errorMessage.english)
        // 先前那次请求晚到的回话不认。
        val list = lastList()
        assertNull(list.path)
        assertFalse(picker.handle(HostMsg.Dirs(list.req, "/Users/ethan", emptyList(), false)))
        link.clearSent()
        picker.retry()
        assertTrue(link.sent.isEmpty())
        assertFalse(picker.isLoading)
        assertEquals("Not connected", picker.errorMessage.english)
        model.handle(HostEvent.Ready(2u))
        picker.retry()
        assertNull(lastList().path)
        assertTrue(picker.isLoading)
        assertNull(picker.errorMessage)
    }

    /** 不带编号的 Error 是同一条连接上别的请求的，不当成列目录的回话，接着等。 */
    @Test fun unnumberedErrorsAreNotClaimed() {
        val picker = DirectoryPickerModel(link)
        picker.load(null)
        val list = lastList()
        assertNull(list.path)
        assertFalse(picker.handle(HostMsg.Error(null, null, "unknown message")))
        assertTrue(picker.isLoading)
        assertNull(picker.errorMessage)
        assertTrue(picker.handle(HostMsg.Dirs(list.req, "/Users/ethan", listOf("dev"), false)))
        assertEquals("/Users/ethan", picker.path)
    }

    @Test fun theRootHasNoParent() {
        val picker = listed("/", listOf("Users"))
        assertNull(picker.parent)
        picker.enter("Users")
        assertEquals("/Users", lastList().path)
    }

    /** Windows 宿主回 C:/Users/me 这种写法：照样按 / 拼子目录、往上走，到盘符根为止。 */
    @Test fun windowsPathsStopAtTheDriveRoot() {
        val picker = listed("C:/Users/me", listOf("dev"))
        assertEquals("C:/Users", picker.parent)
        picker.enter("dev")
        assertEquals("C:/Users/me/dev", lastList().path)
        assertNull(listed("C:/", listOf("Users")).parent)
    }

    /** Windows 宿主回的 UNC 路径 //server/share/…：拼子目录、往上走都留着开头的 //，到共享根为止。 */
    @Test fun uncPathsKeepTheirLeadingSlashes() {
        val picker = listed("//server/share/team", listOf("dev"))
        assertEquals("//server/share", picker.parent)
        picker.enter("dev")
        assertEquals("//server/share/team/dev", lastList().path)
        assertNull(listed("//server/share", listOf("team")).parent)
    }
}
