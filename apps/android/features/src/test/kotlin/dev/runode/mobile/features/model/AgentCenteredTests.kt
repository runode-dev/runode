package dev.runode.mobile.features.model

import dev.runode.mobile.connection.HostEvent
import dev.runode.mobile.connection.LinkFailure
import dev.runode.mobile.connection.LinkState
import dev.runode.mobile.connection.MemoryDeviceKeyStore
import dev.runode.mobile.connection.MemoryMachineStore
import dev.runode.mobile.protocol.Agent
import dev.runode.mobile.protocol.AgentKind
import dev.runode.mobile.protocol.AgentState
import dev.runode.mobile.protocol.ClientMsg
import dev.runode.mobile.protocol.Frame
import dev.runode.mobile.protocol.HostMsg
import dev.runode.mobile.protocol.SessionId
import dev.runode.mobile.protocol.SessionInfo
import dev.runode.mobile.protocol.SessionMeta
import dev.runode.mobile.protocol.TabLayout
import dev.runode.mobile.protocol.PaneLayout
import dev.runode.mobile.protocol.WindowLayout
import dev.runode.mobile.protocol.WorkspaceLayout
import java.time.Instant
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertSame
import kotlin.test.assertTrue
import kotlin.time.Duration.Companion.milliseconds
import kotlin.time.Duration.Companion.seconds
import kotlin.time.TestTimeSource
import kotlinx.coroutines.test.runTest
import org.junit.Rule

/** 一台假电脑上的三个会话：等回答的 Claude、干活的 Codex、普通 shell。 */
private val waiting = SessionId("11111111111111111111111111111111")!!
private val working = SessionId("22222222222222222222222222222222")!!
private val plain = SessionId("33333333333333333333333333333333")!!

private fun info(id: SessionId, state: AgentState?, exited: Boolean = false, owner: String? = null) = SessionInfo(
    id,
    smallGrid,
    SessionMeta(title = id.rawValue, agent = state?.let { Agent(AgentKind("claude"), it) }),
    exited = exited,
    sizeOwner = owner,
)

private fun idleShell(): SessionInfo = info(plain, null).let { it.copy(meta = it.meta.copy(foregroundIsShell = true)) }

private fun readScreens(sent: List<ClientMsg>): List<SessionId> = sent.filterIsInstance<ClientMsg.ReadScreen>().map { it.id }

private fun message(message: HostMsg) = HostEvent.Message(message)

private fun lost() = HostEvent.State(LinkState.Waiting(LinkFailure.Timeout, Instant.now()))

private fun workspace(index: UInt, name: String, tabs: List<List<SessionId>>, activeTab: Int = 0) = WorkspaceLayout(
    index,
    name,
    "/Users/ethan/$name",
    tabs = tabs.mapIndexed { ti, panes ->
        TabLayout(
            (ti + 1).toUInt(),
            ti == activeTab,
            panes.mapIndexed { pi, id -> PaneLayout((pi + 1).toUInt(), id, focused = pi == 0) },
        )
    },
)

class SessionSectionTests {
    @get:Rule val main = MainDispatcherRule()

    private val link = FakeLink()

    private fun model(sessions: List<SessionInfo>): SessionListModel {
        val model = SessionListModel(machineRecord(), link)
        model.handle(HostEvent.Ready(1u))
        model.handle(message(HostMsg.SessionList(sessions)))
        return model
    }

    /** 连上后连同列表一起问布局，编号固定是 0。 */
    @Test fun asksForTheLayoutWithTheList() {
        model(emptyList())
        assertEquals(listOf(ClientMsg.ListSessions, ClientMsg.Layout(0u)), link.sent.take(2))
    }

    /** 按工作区分节，节内按标签、分屏的先后；不在任何窗口里的放在最后的后台一节。 */
    @Test fun sessionsAreGroupedByWorkspace() {
        val background = SessionId("44444444444444444444444444444444")!!
        val model = model(
            listOf(info(plain, null), info(working, AgentState.Working), info(waiting, AgentState.Blocked), info(background, null)),
        )
        val window = WindowLayout(
            1u,
            true,
            listOf(
                workspace(1u, "runode", listOf(listOf(waiting), listOf(plain)), activeTab = 1),
                workspace(2u, "blog", listOf(listOf(working))),
                // 列表里还没有的会话不出现，有标签但其中没有已知会话的工作区也不出现。
                workspace(3u, "empty", listOf(listOf(SessionId("66666666666666666666666666666666")!!))),
            ),
        )
        model.handle(message(HostMsg.Layout(0u, listOf(window))))
        val sections = model.sections
        assertEquals(
            listOf(SessionSection.Id.Workspace(1u, 1u), SessionSection.Id.Workspace(1u, 2u), SessionSection.Id.Background),
            sections.map { it.id },
        )
        assertEquals(listOf(waiting, plain), sections[0].sessions.map { it.id })
        assertEquals("runode", sections[0].name)
        assertEquals("/Users/ethan/runode", sections[0].dir)
        assertNull(sections[0].window)
        // 在工作区里新开终端挨着当前标签里有焦点的分屏。
        assertEquals(plain, sections[0].anchor)
        assertEquals(listOf(working), sections[1].sessions.map { it.id })
        assertEquals(listOf(background), sections[2].sessions.map { it.id })
        assertNull(sections[2].anchor)
        assertTrue(model.hasDesktopWindow)
    }

    /** 开着几个窗口时节上标出第几个窗口。 */
    @Test fun severalWindowsAreTold() {
        val model = model(listOf(info(plain, null), info(working, AgentState.Working)))
        model.handle(
            message(
                HostMsg.Layout(
                    0u,
                    listOf(
                        WindowLayout(2u, workspaces = listOf(workspace(1u, "b", listOf(listOf(working))))),
                        WindowLayout(1u, workspaces = listOf(workspace(1u, "a", listOf(listOf(plain))))),
                    ),
                ),
            ),
        )
        assertEquals(listOf<UInt?>(1u, 2u), model.sections.map { it.window })
        assertEquals(listOf<String?>("a", "b"), model.sections.map { it.name })
    }

    /** 电脑上没有 app 的界面时宿主回 Error：都放进后台，新建工作区退回开后台终端。 */
    @Test fun withoutTheAppEverythingIsInTheBackground() {
        val model = model(listOf(info(plain, null), info(working, AgentState.Working)))
        val window = WindowLayout(1u, workspaces = listOf(workspace(1u, "a", listOf(listOf(plain)))))
        model.handle(message(HostMsg.Layout(0u, listOf(window))))
        model.handle(message(HostMsg.Error(0u, null, "there is no Runode window")))
        assertEquals(listOf<SessionSection.Id>(SessionSection.Id.Background), model.sections.map { it.id })
        assertEquals(listOf(plain, working), model.sections[0].sessions.map { it.id })
        assertFalse(model.hasDesktopWindow)
    }
}

class PreviewTests {
    @get:Rule val main = MainDispatcherRule()

    private val link = FakeLink()
    private val clock = TestTimeSource()

    private fun model(): SessionListModel {
        val model = SessionListModel(machineRecord(), link, 200.milliseconds) { clock.markNow() }
        model.handle(HostEvent.Ready(1u))
        return model
    }

    @Test fun throttleAllowsOneReadPerInterval() {
        val throttle = PreviewThrottle(1.seconds)
        val start = clock.markNow()
        assertEquals(PreviewThrottle.Decision.Now, throttle.request(waiting, start))
        assertEquals(PreviewThrottle.Decision.Now, throttle.request(working, start))
        assertEquals(PreviewThrottle.Decision.Later(700.milliseconds), throttle.request(waiting, start + 300.milliseconds))
        // 已经排着一次了：再来的合并进去。
        assertEquals(PreviewThrottle.Decision.Skip, throttle.request(waiting, start + 400.milliseconds))
        throttle.fire(waiting, start + 1.seconds)
        assertEquals(PreviewThrottle.Decision.Later(500.milliseconds), throttle.request(waiting, start + 1500.milliseconds))
        assertEquals(PreviewThrottle.Decision.Skip, throttle.request(waiting, start + 3.seconds))
    }

    @Test fun enteringTheListReadsEveryScreen() {
        val model = model()
        model.handle(message(HostMsg.SessionList(listOf(info(waiting, AgentState.Blocked), info(plain, null)))))
        assertEquals(setOf(waiting, plain), readScreens(link.sent).toSet())
        assertTrue(ClientMsg.ReadScreen(waiting, SessionListModel.waitingReadLines) in link.sent)
        assertTrue(ClientMsg.ReadScreen(plain, SessionListModel.previewReadLines) in link.sent)
    }

    @Test fun metaAndBellRefreshButAtMostOncePerSecond() = runTest {
        val model = model()
        model.handle(message(HostMsg.SessionList(listOf(info(waiting, AgentState.Blocked)))))
        model.handle(message(HostMsg.ScreenText(waiting, "x", false)))
        link.clearSent()
        // 刚读过：一个间隔内的变化合并成到点时的一次。
        model.handle(message(HostMsg.Meta(waiting, SessionMeta(title = "x"))))
        model.handle(message(HostMsg.Bell(waiting)))
        model.handle(message(HostMsg.Meta(waiting, SessionMeta(title = "y"))))
        assertTrue(readScreens(link.sent).isEmpty())
        clock += 2.seconds
        model.handle(message(HostMsg.Bell(waiting)))
        assertTrue(readScreens(link.sent).isEmpty(), "the deferred read is already scheduled")
        // 排着的那次到点后发出去，只发一次。
        assertTrue(eventually { readScreens(link.sent) == listOf(waiting) })
    }

    /**
     * 同一个会话同时只读一次：读着的时候要的刷新等回话到了（连同退回读屏幕底部的后半）再读，回话不带编号，两次同时在
     * 路上会对错读法。
     */
    @Test fun oneReadAtATimePerSession() {
        val model = model()
        model.handle(message(HostMsg.SessionList(listOf(idleShell()))))
        assertEquals(listOf(plain), readScreens(link.sent))
        link.clearSent()
        clock += 2.seconds
        model.handle(message(HostMsg.Bell(plain)))
        assertTrue(readScreens(link.sent).isEmpty(), "the first read has not been answered")
        // 最近一条命令没有输出：退回读屏幕底部，还是同一次。
        model.handle(message(HostMsg.ScreenText(plain, "", false)))
        assertEquals(listOf<ClientMsg>(ClientMsg.ReadScreen(plain, SessionListModel.previewReadLines)), link.sent)
        link.clearSent()
        model.handle(message(HostMsg.ScreenText(plain, "ok\n~ ❯", false)))
        assertEquals(listOf("ok"), model.previews[plain])
        assertEquals(listOf<ClientMsg>(ClientMsg.ReadScreen(plain, null, 1u)), link.sent)
    }

    @Test fun screenTextBecomesThePreview() {
        val model = model()
        model.handle(message(HostMsg.SessionList(listOf(info(waiting, AgentState.Blocked)))))
        val screen = "● 跑测试\n\n────────\n Do you want to proceed?\n ❯ 1. Yes   \n   2. No\n\n\n"
        model.handle(message(HostMsg.ScreenText(waiting, screen, false)))
        assertEquals(listOf(" Do you want to proceed?", " ❯ 1. Yes", "   2. No"), model.previews[waiting])
        // 不在列表里的会话的回话不收。
        model.handle(message(HostMsg.ScreenText(plain, "x", false)))
        assertNull(model.previews[plain])
        // 不等回答的会话只留两行。
        model.handle(
            message(HostMsg.SessionList(listOf(info(waiting, AgentState.Blocked), info(plain, AgentState.Working)))),
        )
        model.handle(message(HostMsg.ScreenText(plain, screen, false)))
        assertEquals(listOf(" ❯ 1. Yes", "   2. No"), model.previews[plain])
    }

    /** 等回答的会话从读回来的屏幕认出提问；不等了就作废，不等回答的会话不认。 */
    @Test fun waitingScreensArePromptsUntilAnswered() {
        val model = model()
        model.handle(
            message(HostMsg.SessionList(listOf(info(waiting, AgentState.Blocked), info(plain, AgentState.Working)))),
        )
        val screen = "────────\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n\n"
        model.handle(message(HostMsg.ScreenText(waiting, screen, false)))
        model.handle(message(HostMsg.ScreenText(plain, screen, false)))
        assertEquals(listOf("Do you want to proceed?"), model.prompts[waiting]?.question)
        assertEquals(listOf("Yes", "No"), model.prompts[waiting]?.options?.map { it.text })
        assertNull(model.prompts[plain])
        // 屏幕上认不出提问：退回通用的键。
        model.handle(message(HostMsg.ScreenText(waiting, "● 等一下\n", false)))
        assertNull(model.prompts[waiting])
        model.handle(message(HostMsg.ScreenText(waiting, screen, false)))
        assertNotNull(model.prompts[waiting])
        model.handle(
            message(HostMsg.Meta(waiting, SessionMeta(title = "x", agent = Agent(AgentKind("claude"), AgentState.Working)))),
        )
        assertNull(model.prompts[waiting])
    }

    /**
     * shell 在提示符上等输入时读最近一条命令的输出（`command: 1`），宿主切掉了提示符，两行式提示符也不会混进预览；
     * agent 在跑的会话照旧读屏幕底部。
     */
    @Test fun idleShellsPreviewTheirLastCommand() {
        val model = model()
        model.handle(message(HostMsg.SessionList(listOf(idleShell(), info(waiting, AgentState.Blocked)))))
        assertTrue(ClientMsg.ReadScreen(plain, null, 1u) in link.sent)
        assertTrue(ClientMsg.ReadScreen(waiting, SessionListModel.waitingReadLines) in link.sent)
        model.handle(message(HostMsg.ScreenText(plain, "Cargo.lock  apps\n\nREADME.md  crates\n", false)))
        assertEquals(listOf("Cargo.lock  apps", "README.md  crates"), model.previews[plain])
    }

    /** 读命令输出出错（没有 shell 集成）：马上退回读屏幕底部，并去掉光标所在的提示符那一行。 */
    @Test fun lastCommandErrorsFallBackToTheScreen() {
        val model = model()
        model.handle(message(HostMsg.SessionList(listOf(idleShell()))))
        link.clearSent()
        model.handle(message(HostMsg.Error(null, plain, "this needs shell integration")))
        assertEquals(listOf<ClientMsg>(ClientMsg.ReadScreen(plain, SessionListModel.previewReadLines)), link.sent)
        model.handle(message(HostMsg.ScreenText(plain, "ls\nCargo.lock  apps\n~/runode ❯\n", false)))
        assertEquals(listOf("ls", "Cargo.lock  apps"), model.previews[plain])
        assertNotNull(model.session(plain))
    }

    /** 上一条命令没有输出（cd 这类），或者还没跑过命令：同样退回读屏幕底部。 */
    @Test fun emptyLastCommandFallsBackToTheScreen() {
        val model = model()
        model.handle(message(HostMsg.SessionList(listOf(idleShell()))))
        link.clearSent()
        model.handle(message(HostMsg.ScreenText(plain, "\n  \n", false)))
        assertEquals(listOf<ClientMsg>(ClientMsg.ReadScreen(plain, SessionListModel.previewReadLines)), link.sent)
        assertNull(model.previews[plain])
        model.handle(message(HostMsg.ScreenText(plain, "make\nok\n~/runode ❯", false)))
        assertEquals(listOf("make", "ok"), model.previews[plain])
    }

    /** 读屏幕失败（比如超时）不是会话没了，不能把它从列表里拿掉。 */
    @Test fun readErrorsDoNotRemoveSessions() {
        val model = model()
        model.handle(message(HostMsg.SessionList(listOf(info(waiting, AgentState.Blocked)))))
        model.handle(message(HostMsg.Error(null, waiting, "session ${waiting.rawValue} did not answer")))
        assertNotNull(model.session(waiting))
        model.handle(message(HostMsg.Error(null, waiting, "no session ${waiting.rawValue}")))
        assertNull(model.session(waiting))
    }

    @Test fun nothingIsReadWhileDisconnected() {
        val model = model()
        model.handle(message(HostMsg.SessionList(listOf(info(waiting, AgentState.Blocked)))))
        model.handle(lost())
        link.clearSent()
        clock += 5.seconds
        model.refreshPreviews()
        assertTrue(readScreens(link.sent).isEmpty())
    }
}

class QuickReplyTests {
    @get:Rule val main = MainDispatcherRule()

    private val link = FakeLink()

    @Test fun keysGoOutAsSendKeys() {
        val reply = QuickReplyModel(waiting, link)
        reply.press(QuickKey.standard.first { it.key == "enter" })
        val keys = assertIs<ClientMsg.SendKeys>(link.sent.first())
        assertEquals(ClientMsg.SendKeys(keys.req, waiting, listOf("enter")), keys)
        assertTrue(reply.handle(HostMsg.Done(keys.req)))
        assertEquals(1, reply.deliveredCount)
        // 别人的回话不收。
        assertFalse(reply.handle(HostMsg.Done(keys.req + 100u)))
    }

    @Test fun textIsPastedThenEntered() {
        val delivered = mutableListOf<SessionId>()
        val reply = QuickReplyModel(waiting, link) { delivered += it }
        reply.draft = "   "
        assertFalse(reply.canSendDraft)
        reply.sendDraft()
        assertTrue(link.sent.isEmpty())
        reply.draft = "用方案 2"
        reply.sendDraft()
        assertTrue(reply.draft.isEmpty())
        assertEquals(2, link.sent.size)
        val paste = assertIs<ClientMsg.Paste>(link.sent[0])
        assertEquals(ClientMsg.Paste(paste.req, waiting, "用方案 2"), paste)
        val enter = assertIs<ClientMsg.SendKeys>(link.sent[1])
        assertEquals(ClientMsg.SendKeys(enter.req, waiting, listOf("enter")), enter)
        reply.handle(HostMsg.Done(paste.req))
        assertEquals(0, reply.deliveredCount, "only half of the batch is done")
        reply.handle(HostMsg.Done(enter.req))
        assertEquals(1, reply.deliveredCount)
        assertEquals(listOf(waiting), delivered)
    }

    /** 装不进一条控制消息的字不发（电脑会断开连接），草稿留着；刚好装得下的照发。 */
    @Test fun textTooLongForOneMessageIsNotSent() {
        val reply = QuickReplyModel(waiting, link)
        val overhead = Frame.control(ClientMsg.Paste(UInt.MAX_VALUE, waiting, "")).payload.size
        val longest = "a".repeat(Frame.maxControlPayload.toInt() - overhead)
        reply.draft = longest + "a"
        reply.sendDraft()
        assertTrue(link.sent.isEmpty())
        assertEquals(longest + "a", reply.draft)
        assertEquals(1, reply.failedCount)
        assertNotNull(reply.errorMessage)
        reply.draft = longest
        reply.sendDraft()
        assertEquals(2, link.sent.size)
    }

    @Test fun failuresAreShown() {
        val reply = QuickReplyModel(waiting, link)
        reply.press(QuickKey.standard[0])
        val req = assertIs<ClientMsg.SendKeys>(link.sent.first()).req
        reply.handle(HostMsg.Error(req, waiting, "no session"))
        assertEquals(1, reply.failedCount)
        assertEquals("Not sent: no session", reply.errorMessage.english)
        assertEquals(0, reply.deliveredCount)
        // 下一次发送清掉旧的错误。
        reply.press(QuickKey.standard[0])
        assertNull(reply.errorMessage)
        reply.connectionLost()
        assertEquals(2, reply.failedCount)
    }

    /** 粘贴被宿主拒了，或者回话前连接断了：字放回文本框；用户已经打了新的字时不盖掉。 */
    @Test fun unsentTextComesBackToTheDraft() {
        val reply = QuickReplyModel(waiting, link)
        reply.draft = "用方案 2"
        reply.sendDraft()
        val paste = assertIs<ClientMsg.Paste>(link.sent.first()).req
        reply.handle(HostMsg.Error(paste, waiting, "no session"))
        assertEquals("用方案 2", reply.draft)

        reply.draft = "用方案 3"
        reply.sendDraft()
        reply.draft = "新打的"
        reply.connectionLost()
        assertEquals("新打的", reply.draft)
    }

    /** 连着发了两条，都还没回话时连接断了：两段字按发出的先后都放回文本框。 */
    @Test fun everyUnsentTextComesBack() {
        val reply = QuickReplyModel(waiting, link)
        reply.draft = "用方案 2"
        reply.sendDraft()
        reply.draft = "用方案 3"
        reply.sendDraft()
        assertTrue(reply.draft.isEmpty())
        reply.connectionLost()
        assertEquals("用方案 2\n用方案 3", reply.draft)
    }

    /** 连着发了两条，宿主都回了错：两段字按回错的先后都放回文本框，不只剩第一段。 */
    @Test fun everyRefusedTextComesBack() {
        val reply = QuickReplyModel(waiting, link)
        reply.draft = "用方案 2"
        reply.sendDraft()
        reply.draft = "用方案 3"
        reply.sendDraft()
        assertEquals(4, link.sent.size)
        val first = assertIs<ClientMsg.Paste>(link.sent[0]).req
        val second = assertIs<ClientMsg.Paste>(link.sent[2]).req
        reply.handle(HostMsg.Error(first, waiting, "no session"))
        reply.handle(HostMsg.Error(second, waiting, "no session"))
        assertEquals("用方案 2\n用方案 3", reply.draft)
    }

    /** 断着时不发：按失败报出来，草稿留着；列表重新连上、收到新的会话列表后照常发。 */
    @Test fun nothingIsSentWhileDisconnected() {
        val model = SessionListModel(machineRecord(), link)
        val reply = model.quickReply(waiting)
        assertFalse(reply.isConnected)
        reply.draft = "用方案 2"
        reply.sendDraft()
        reply.press(QuickKey.standard[0])
        assertTrue(link.sent.isEmpty())
        assertEquals("用方案 2", reply.draft)
        assertEquals(2, reply.failedCount)
        assertEquals("Not connected", reply.errorMessage.english)
        model.handle(HostEvent.Ready(1u))
        // 连上了、新列表还没到：卡片还是断开前的，不能发。
        assertFalse(reply.isConnected)
        assertFalse(model.quickReply(working).isConnected)
        model.handle(message(HostMsg.SessionList(emptyList())))
        assertTrue(reply.isConnected)
        assertTrue(model.quickReply(working).isConnected)
        link.clearSent()
        reply.sendDraft()
        assertTrue(reply.draft.isEmpty())
        assertTrue(link.sent.any { it is ClientMsg.Paste && it.id == waiting && it.text == "用方案 2" })
        assertNull(reply.errorMessage)
        model.handle(lost())
        assertFalse(reply.isConnected)
    }

    /** 终端页底部的快速回复跟着终端页的连接：连上（Ready）之前、断开以后都不发。 */
    @Test fun terminalRepliesFollowTheTerminalsConnection() {
        val terminal = TerminalModel(waiting, UiText.Raw("claude"), link = link) {}
        assertFalse(terminal.quickReply.isConnected)
        terminal.handle(HostEvent.Ready(1u))
        assertTrue(terminal.quickReply.isConnected)
        terminal.handle(lost())
        assertFalse(terminal.quickReply.isConnected)
        link.clearSent()
        terminal.quickReply.press(QuickKey.standard[0])
        assertTrue(link.sent.isEmpty())
        assertEquals(1, terminal.quickReply.failedCount)
    }

    /** 列表上的快速回复：不用连上会话，送到后刷新这个会话的预览。 */
    @Test fun listRepliesRefreshThePreview() {
        val clock = TestTimeSource()
        val model = SessionListModel(machineRecord(), link, now = { clock.markNow() })
        model.handle(HostEvent.Ready(1u))
        model.handle(message(HostMsg.SessionList(listOf(info(waiting, AgentState.Blocked)))))
        model.handle(message(HostMsg.ScreenText(waiting, "x", false)))
        link.clearSent()
        val reply = model.quickReply(waiting)
        assertSame(reply, model.quickReply(waiting))
        reply.press(QuickKey.standard[0])
        assertTrue(attaches(link.sent).isEmpty())
        val req = assertIs<ClientMsg.SendKeys>(link.sent.first()).req
        clock += 2.seconds
        model.handle(message(HostMsg.Done(req)))
        assertEquals(1, reply.deliveredCount)
        assertEquals(listOf(waiting), readScreens(link.sent))
    }

    /** 答完以后读到的还是所答的提问时不再认出来，提问换了才认新的；提问一直不换，读够次数后照旧认出来。 */
    @Test fun theAnsweredPromptDoesNotComeBackFromTheOldScreen() {
        val clock = TestTimeSource()
        val model = SessionListModel(machineRecord(), link, now = { clock.markNow() })
        model.handle(HostEvent.Ready(1u))
        model.handle(message(HostMsg.SessionList(listOf(info(waiting, AgentState.Blocked)))))
        val first = "────────\n Run the tests?\n ❯ 1. Yes\n   2. No\n\n"
        val second = "────────\n Push to main?\n ❯ 1. Yes\n   2. No\n\n"
        fun answer() {
            link.clearSent()
            model.quickReply(waiting).press(QuickKey.standard[0])
            val req = assertIs<ClientMsg.SendKeys>(link.sent.first(), "the key was not sent").req
            clock += 2.seconds
            model.handle(message(HostMsg.Done(req)))
        }
        model.handle(message(HostMsg.ScreenText(waiting, first, false)))
        assertEquals(listOf("Run the tests?"), model.prompts[waiting]?.question)
        answer()
        assertNull(model.prompts[waiting])
        model.handle(message(HostMsg.ScreenText(waiting, first, false)))
        assertNull(model.prompts[waiting], "the agent has not redrawn yet")
        model.handle(message(HostMsg.ScreenText(waiting, second, false)))
        assertEquals(listOf("Push to main?"), model.prompts[waiting]?.question)
        // 按键没起作用、屏幕一直是那一幅：读够次数后提问回来。
        answer()
        repeat(SessionListModel.answeredRereads) {
            model.handle(message(HostMsg.ScreenText(waiting, second, false)))
            assertNull(model.prompts[waiting])
        }
        model.handle(message(HostMsg.ScreenText(waiting, second, false)))
        assertEquals(listOf("Push to main?"), model.prompts[waiting]?.question)
    }

    /** 答完以后屏幕上只有状态栏的秒数在走，提问还是那一个：不再认出来。 */
    @Test fun theAnsweredPromptDoesNotComeBackWhileOtherLinesChange() {
        val clock = TestTimeSource()
        val model = SessionListModel(machineRecord(), link, now = { clock.markNow() })
        model.handle(HostEvent.Ready(1u))
        model.handle(message(HostMsg.SessionList(listOf(info(waiting, AgentState.Blocked)))))
        val before = "────────\n Run the tests?\n ❯ 1. Yes\n   2. No\n\n 12s elapsed\n"
        val after = "────────\n Run the tests?\n ❯ 1. Yes\n   2. No\n\n 13s elapsed\n"
        model.handle(message(HostMsg.ScreenText(waiting, before, false)))
        assertEquals(listOf("Run the tests?"), model.prompts[waiting]?.question)
        link.clearSent()
        model.quickReply(waiting).press(QuickKey.standard[0])
        val req = assertIs<ClientMsg.SendKeys>(link.sent.first(), "the key was not sent").req
        clock += 2.seconds
        model.handle(message(HostMsg.Done(req)))
        model.handle(message(HostMsg.ScreenText(waiting, after, false)))
        assertNull(model.prompts[waiting], "only the status line changed")
    }

    /**
     * 答完以后光标记号挪到了别的选项上、别的字没变：还是所答的那个提问。agent 进入等回答的时刻变了时是新的一问，提问
     * 一样也认。
     */
    @Test fun theAnsweredPromptComesBackOnlyWithANewWait() {
        val clock = TestTimeSource()
        val model = SessionListModel(machineRecord(), link, now = { clock.markNow() })
        model.handle(HostEvent.Ready(1u))
        val blocked = info(waiting, AgentState.Blocked)
        fun since(ms: Long) = blocked.copy(meta = blocked.meta.copy(agentSinceMs = ms))
        model.handle(message(HostMsg.SessionList(listOf(since(1_000)))))
        val screen = "────────\n Run the tests?\n │ ❯ 1. Yes\n │   2. No\n\n"
        val moved = "────────\n Run the tests?\n │   1. Yes\n │ ❯ 2. No\n\n"
        model.handle(message(HostMsg.ScreenText(waiting, screen, false)))
        assertEquals(listOf("Run the tests?"), model.prompts[waiting]?.question)
        link.clearSent()
        model.quickReply(waiting).press(QuickKey.standard[0])
        val req = assertIs<ClientMsg.SendKeys>(link.sent.first(), "the key was not sent").req
        clock += 2.seconds
        model.handle(message(HostMsg.Done(req)))
        model.handle(message(HostMsg.ScreenText(waiting, moved, false)))
        assertNull(model.prompts[waiting], "only the marker moved")
        model.handle(message(HostMsg.Meta(waiting, since(2_000).meta, null)))
        model.handle(message(HostMsg.ScreenText(waiting, screen, false)))
        assertEquals(listOf("Run the tests?"), model.prompts[waiting]?.question)
    }
}

class NavigationTests {
    @get:Rule val main = MainDispatcherRule()

    /** 从列表打开终端页：列表知道有没有电脑在显示它，据此决定一开始就适配手机还是跟随电脑。 */
    @Test fun terminalsKnowWhetherTheMachineShowsThem() = runTest {
        val store = MemoryMachineStore()
        val machine = machineRecord()
        store.upsert(machine)
        val link = FakeLink()
        val app = AppModel(
            AppDependencies(store, MemoryDeviceKeyStore(), FakePairing { machine }, { link }, "测试手机"),
        )
        app.machineList.load()
        app.path = listOf(Route.Machine(machine.id))
        val list = assertNotNull(app.sessionList(machine.id))
        list.handle(HostEvent.Ready(1u))
        list.handle(message(HostMsg.SessionList(listOf(info(waiting, AgentState.Blocked, owner = "homelab"), info(plain, null)))))
        val shown = assertNotNull(app.terminal(machine.id, waiting))
        val background = assertNotNull(app.terminal(machine.id, plain))
        assertFalse(shown.fitsPhone)
        assertTrue(background.fitsPhone)
        assertTrue(shown.isAwaitingAnswer)
        shown.close()
        background.close()
    }
}

class StatusTextTests {
    @Test fun reconnectCountdown() {
        val now = Instant.ofEpochSecond(1000)
        fun waitingUntil(retryAt: Instant, offline: Boolean = false) =
            LinkState.Waiting(LinkFailure.Timeout, retryAt, offline)
        assertEquals(
            "Disconnected. Reconnecting in 5 seconds",
            Presentation.linkStatus(waitingUntil(now.plusMillis(4200)), now).english,
        )
        assertEquals("Disconnected. Reconnecting", Presentation.linkStatus(waitingUntil(now.minusSeconds(1)), now).english)
        assertEquals(
            "Computer offline. Will connect when it is back",
            Presentation.linkStatus(waitingUntil(now.plusSeconds(300), offline = true), now).english,
        )
        assertEquals("Connecting…", Presentation.linkStatus(LinkState.Connecting).english)
    }

    @Test fun terminalSubtitleNamesTheMachineOrTheTrouble() {
        val connected = LinkState.Connected("homelab", null)
        val live = TerminalModel.Phase.Live
        assertEquals("MacBook Pro", Presentation.terminalSubtitle("MacBook Pro", connected, live).english)
        assertEquals("Connecting…", Presentation.terminalSubtitle("MacBook Pro", LinkState.Connecting, live).english)
        assertEquals("Connected", Presentation.terminalSubtitle(null, connected, live).english)
        assertEquals(
            "Shell exited",
            Presentation.terminalSubtitle("MacBook Pro", LinkState.Connecting, TerminalModel.Phase.Exited(0)).english,
        )
        assertEquals(
            "Session ended",
            Presentation.terminalSubtitle("MacBook Pro", connected, TerminalModel.Phase.Gone(UiText.Raw("没了"))).english,
        )
    }

    @Test fun sessionSubtitleSaysWhatTheSessionIsDoing() {
        val now = Instant.ofEpochMilli(10_000_000)
        fun subtitle(session: SessionInfo, machine: String? = null) = Presentation.sessionSubtitle(session, machine, now).english
        fun since(session: SessionInfo, minutes: Long) =
            session.copy(meta = session.meta.copy(agentSinceMs = now.toEpochMilli() - minutes * 60_000))
        assertEquals("Claude Code is waiting for you", subtitle(info(waiting, AgentState.Blocked)))
        assertEquals("Claude Code is waiting for you on homelab", subtitle(info(waiting, AgentState.Blocked), "homelab"))
        assertEquals("Claude Code is working", subtitle(info(working, AgentState.Working)))
        assertEquals("Claude Code is idle", subtitle(info(working, AgentState.Idle)))
        val vim = info(plain, null).let { it.copy(meta = it.meta.copy(foreground = "vim")) }
        assertEquals("Running vim", subtitle(vim))
        val shell = info(plain, null).let { it.copy(meta = it.meta.copy(foregroundIsShell = true, foreground = "zsh")) }
        assertEquals("Idle", subtitle(shell))
        assertEquals("Exited", subtitle(info(waiting, AgentState.Blocked, exited = true)))
        // 满一分钟以后说等了、干了多久；不满一分钟时照旧。
        val blocked = since(info(waiting, AgentState.Blocked), 12)
        assertEquals("Claude Code has been waiting for 12 minutes", subtitle(blocked))
        assertEquals("Claude Code has been waiting on homelab for 12 minutes", subtitle(blocked, "homelab"))
        assertEquals("Claude Code has been working for 12 minutes", subtitle(since(info(working, AgentState.Working), 12)))
        assertEquals("Claude Code is working", subtitle(since(info(working, AgentState.Working), 0)))
        assertEquals("Claude Code 等了 12分钟", Presentation.sessionSubtitle(blocked, null, now).simplifiedChinese)
    }

    @Test fun linkRouteTellsLocalFromDirectFromTunnelled() {
        fun route(address: String?) = Presentation.linkRoute(LinkState.Connected("homelab", address)).english
        val local = "Connected on the local network"
        val direct = "Connected directly"
        assertEquals(local, route("192.168.1.20"))
        assertEquals(local, route("10.0.0.10%en0"))
        assertEquals(local, route("172.20.1.2"))
        assertEquals(local, route("169.254.3.4"))
        assertEquals(local, route("fe80::1c2:3%en0"))
        assertEquals(local, route("FD7A:115C:A1E0::1"))
        assertEquals(local, route("ethans-mbp.local"))
        assertEquals(local, route("::ffff:192.168.1.20"))
        assertEquals(direct, route("172.32.1.2"))
        assertEquals(direct, route("203.0.113.7"))
        assertEquals(direct, route("2001:db8::1"))
        assertEquals("Connected over the internet", route(null))
        assertEquals("Connecting…", Presentation.linkRoute(LinkState.Connecting).english)
    }

    /** shell 在提示符上等输入时，最后一个有字的行（光标所在的提示符）不进预览。 */
    @Test fun previewsSkipThePromptTheShellWaitsAt() {
        val screen = "$ cargo build\n   Compiling runode v0.1.0\n    Finished dev\n ~/runode  main ❯\n\n"
        assertEquals(
            listOf("$ cargo build", "   Compiling runode v0.1.0", "    Finished dev"),
            Presentation.previewLines(screen, 3, atPrompt = true),
        )
        assertEquals(" ~/runode  main ❯", Presentation.previewLines(screen, 3).last())
        assertTrue(Presentation.previewLines("❯", 3, atPrompt = true).isEmpty())
    }

    @Test fun previewsDropBlankAndRuleLines() {
        assertEquals(listOf("a", "b", "c"), Presentation.previewLines("a\n\n  \nb  \n────\n╭──╮\nc\n", 3))
        assertEquals(listOf("2", "3", "4"), Presentation.previewLines("1\n2\n3\n4", 3))
        assertTrue(Presentation.previewLines("", 3).isEmpty())
    }
}
