package dev.runode.mobile.features.model

import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.runode.mobile.connection.HostEvent
import dev.runode.mobile.connection.LinkFailure
import dev.runode.mobile.connection.LinkState
import dev.runode.mobile.features.R
import dev.runode.mobile.protocol.Agent
import dev.runode.mobile.protocol.AgentKind
import dev.runode.mobile.protocol.AgentState
import dev.runode.mobile.protocol.AttachMode
import dev.runode.mobile.protocol.ClientMsg
import dev.runode.mobile.protocol.Frame
import dev.runode.mobile.protocol.FrameKind
import dev.runode.mobile.protocol.GridSize
import dev.runode.mobile.protocol.HostMsg
import dev.runode.mobile.protocol.Rgb
import dev.runode.mobile.protocol.SessionId
import dev.runode.mobile.protocol.SessionMeta
import dev.runode.mobile.protocol.TermSettings
import dev.runode.mobile.terminal.KeyInput
import dev.runode.mobile.terminal.KeyModifiers
import dev.runode.mobile.terminal.TerminalInput
import java.time.Instant
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlin.time.Duration.Companion.milliseconds
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import org.junit.Test
import org.junit.runner.RunWith

/** 在主线程上跑测试（[TerminalModel] 持有的 VT 只能在主线程上用），视图模型自己的协程也在主线程上接着跑。 */
private fun onMain(block: suspend CoroutineScope.() -> Unit) = runBlocking(Dispatchers.Main, block)

/** 连上会话、喂重放和输出都经真的 libghostty-vt。 */
@RunWith(AndroidJUnit4::class)
class TerminalModelTests {
    private val link = FakeLink()
    private val display = FakeDisplay()
    private val fit = GridSize(45u, 30u, 24u, 48u)
    private val rotated = GridSize(90u, 15u, 24u, 48u)

    private fun model(): TerminalModel {
        val model = TerminalModel(sessionA, UiText.Raw("zsh"), link = link) {}
        model.attachDisplay(display)
        return model
    }

    private fun message(message: HostMsg) = HostEvent.Message(message)

    private fun frame(kind: FrameKind, channel: UInt, text: String, generation: ULong) =
        HostEvent.Frame(Frame(kind, channel, text.encodeToByteArray()), generation)

    private fun lost() = HostEvent.State(LinkState.Waiting(LinkFailure.Timeout, Instant.now()))

    private fun attached(channel: UInt, size: GridSize = smallGrid, meta: SessionMeta = SessionMeta(title = "构建")) =
        message(HostMsg.Attached(sessionA, channel, size, AttachMode.VtReplay, meta, TermSettings.default))

    private fun attach(size: GridSize? = null) = ClientMsg.Attach(sessionA, size, AttachMode.VtReplay)

    private fun claim(size: GridSize) = listOf(ClientMsg.Resize(sessionA, size), ClientMsg.Focus(sessionA, true))

    /** 连上、收完重放，返回模型。 */
    private fun live(): TerminalModel {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        model.handle(attached(5u))
        model.handle(frame(FrameKind.Snapshot, 5u, "replayed\r\n", 1u))
        model.handle(message(HostMsg.SnapshotEnd(sessionA)))
        return model
    }

    /**
     * 有电脑在显示的会话（列表里有尺寸 owner）：不带尺寸连上，跟随电脑。宿主有 owner 时重放完会补发 `SizeOwner`，这里
     * 照样补上，免得等不到它的探测把页面改成适配手机。
     */
    private fun followingModel(): TerminalModel {
        val model = TerminalModel(sessionA, UiText.Raw("zsh"), link = link, ownerHint = SizeOwnerHint.Someone) {}
        model.attachDisplay(display)
        model.updateFitSize(fit)
        model.handle(HostEvent.Ready(1u))
        model.handle(attached(5u))
        model.handle(message(HostMsg.SnapshotEnd(sessionA)))
        model.handle(message(HostMsg.SizeOwner(sessionA, false, "Ethan 的 MacBook")))
        return model
    }

    private fun probing(hint: SizeOwnerHint = SizeOwnerHint.Unknown): TerminalModel {
        val model = TerminalModel(
            sessionA, UiText.Raw("zsh"), link = link, ownerHint = hint, ownerProbeDelay = 10.milliseconds,
        ) {}
        model.attachDisplay(display)
        model.updateFitSize(fit)
        return model
    }

    /** 终端视图改接到别的会话后，原来那个会话晚到的 Attached 不再把视图换回去。 */
    @Test fun aDetachedModelNoLongerDrawsOnTheView() = onMain {
        val old = model()
        val new = TerminalModel(sessionB, UiText.Raw("zsh"), link = link) {}
        old.detachDisplay(display)
        new.attachDisplay(display)
        old.handle(HostEvent.Ready(1u))
        old.handle(attached(5u))
        assertNotNull(old.terminal)
        assertNull(display.terminal)
    }

    @Test fun attachesWithoutTakingTheSize() = onMain {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        assertEquals(listOf<ClientMsg>(attach()), link.sent)
        assertEquals(TerminalModel.Phase.Connecting, model.phase)
    }

    @Test fun replaysThenFeedsOutputOnItsChannel() = onMain {
        val model = live()
        assertEquals(TerminalModel.Phase.Live, model.phase)
        assertEquals(UiText.Raw("构建"), model.title)
        assertEquals(smallGrid, model.gridSize)
        assertNotNull(display.terminal)
        model.handle(frame(FrameKind.Output, 5u, "live", 1u))
        // 别的通道、旧连接的输出不喂。
        model.handle(frame(FrameKind.Output, 6u, "other", 1u))
        model.handle(frame(FrameKind.Output, 5u, "stale", 0u))
        val lines = assertNotNull(model.terminal).screenLines()
        assertEquals("replayed", lines[0])
        assertEquals("live", lines[1])
    }

    @Test fun resizeAndThemeHappenAtTheirMarkers() = onMain {
        val model = live()
        val bigger = GridSize(30u, 6u, 8u, 16u)
        model.handle(message(HostMsg.Resized(sessionA, bigger)))
        assertEquals(bigger, model.terminal?.size)
        assertEquals(bigger, model.gridSize)
        val theme = TermSettings.default.copy(background = Rgb(1u, 2u, 3u))
        model.handle(message(HostMsg.ThemeApplied(sessionA, theme)))
        assertEquals(theme, model.terminal?.settings)
        assertEquals(theme, display.settings)
        // 别的会话的标记不碰这里的 VT。
        model.handle(message(HostMsg.Resized(sessionB, smallGrid)))
        assertEquals(bigger, model.terminal?.size)
    }

    @Test fun chromeFollowsTheTheme() = onMain {
        val model = live()
        assertEquals(AppTheme(TermSettings.default), model.chromeTheme)
        val theme = TermSettings.default.copy(background = Rgb(0xFFFFFFu), foreground = Rgb(0x101010u))
        model.handle(message(HostMsg.ThemeApplied(sessionA, theme)))
        assertEquals(Rgb(0xFFFFFFu), model.chromeTheme.background)
        assertEquals(Rgb(0x101010u), model.chromeTheme.foreground)
        assertFalse(model.chromeTheme.isDark)
    }

    @Test fun answerRowOffersTheFirstThreeStandardKeys() {
        assertEquals(QuickKey.standard.take(3), QuickKey.options)
        assertEquals(listOf("1", "2", "3"), QuickKey.options.map { it.key })
    }

    @Test fun resyncDropsTheOldStreamUntilAttached() = onMain {
        val model = live()
        link.clearSent()
        model.handle(message(HostMsg.Resync(sessionA, "slow")))
        assertEquals(listOf<ClientMsg>(attach()), link.sent)
        assertEquals(TerminalModel.Phase.Connecting, model.phase)
        // 新的 Attached 之前到的旧通道输出和标记都丢掉。
        model.handle(frame(FrameKind.Output, 5u, "lost", 1u))
        model.handle(message(HostMsg.Resized(sessionA, GridSize(50u, 9u, 8u, 16u))))
        model.handle(attached(9u))
        assertEquals(smallGrid, model.terminal?.size)
        model.handle(frame(FrameKind.Snapshot, 9u, "fresh", 1u))
        model.handle(message(HostMsg.SnapshotEnd(sessionA)))
        assertEquals("fresh", assertNotNull(model.terminal).screenLines()[0])
        assertTrue(display.resets >= 2)
    }

    @Test fun inputIsEncodedForTheTerminal() = onMain {
        val model = live()
        model.send(TerminalInput.Text("ls"))
        model.send(TerminalInput.Key(assertNotNull(KeyInput.typing("c", KeyModifiers.control))))
        model.send(TerminalInput.Paste("a\nb"))
        val inputs = link.inputs
        assertEquals(
            listOf("ls".encodeToByteArray().toList(), listOf<Byte>(0x03), "a\rb".encodeToByteArray().toList()),
            inputs.map { it.data.toList() },
        )
        assertTrue(inputs.all { it.channel == 5u && it.generation == 1uL })
    }

    @Test fun noInputBeforeAttached() = onMain {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        model.send(TerminalInput.Text("ls"))
        assertTrue(link.inputs.isEmpty())
    }

    /** 没有 owner 的会话（电脑上没有窗口在显示它）：自动适配手机，连上时就带着尺寸。 */
    @Test fun sessionsWithoutAnOwnerFitThePhone() = onMain {
        val model = TerminalModel(sessionA, UiText.Raw("zsh"), link = link, ownerHint = SizeOwnerHint.None) {}
        model.attachDisplay(display)
        model.updateFitSize(fit)
        model.handle(HostEvent.Ready(1u))
        assertEquals(listOf<ClientMsg>(attach(fit)), link.sent)
        assertTrue(model.fitsPhone)
        assertEquals(true, display.sizeModes.last())
    }

    /** 视图还没排好、不知道手机的尺寸时先不带尺寸连上，Attached 以后补发 Resize 加 Focus。 */
    @Test fun fitIsSentOnceTheViewKnowsItsSize() = onMain {
        val model = TerminalModel(sessionA, UiText.Raw("zsh"), link = link, ownerHint = SizeOwnerHint.None) {}
        model.handle(HostEvent.Ready(1u))
        assertEquals(listOf<ClientMsg>(attach()), link.sent)
        model.handle(attached(5u))
        link.clearSent()
        model.updateFitSize(fit)
        assertEquals(claim(fit), link.sent)
        // 同样的尺寸不重复发。
        link.clearSent()
        model.updateFitSize(fit)
        assertTrue(link.sent.isEmpty())
    }

    @Test fun sessionsShownOnTheMachineFollowIt() = onMain {
        val model = followingModel()
        assertEquals(listOf<ClientMsg>(attach()), link.sent)
        model.handle(message(HostMsg.SizeOwner(sessionA, false, "Ethan 的 MacBook")))
        assertFalse(model.fitsPhone)
        assertEquals(SizeOwnership.Other("Ethan 的 MacBook"), model.sizeOwnership)
    }

    /** 不知道有没有 owner 时：重放完以后宿主没报 SizeOwner，就当作没有，改成适配手机。 */
    @Test fun noOwnerReportMeansFitThePhone() = onMain {
        val model = probing()
        model.handle(HostEvent.Ready(1u))
        assertEquals(listOf<ClientMsg>(attach()), link.sent)
        model.handle(attached(5u))
        model.handle(message(HostMsg.SnapshotEnd(sessionA)))
        assertTrue(eventually { model.fitsPhone })
        assertEquals(claim(fit), link.sent.takeLast(2))
    }

    /** 列表说有 owner（记录已经过时，比如手机自己刚关掉这个会话的页面），宿主却没报 SizeOwner：等一会儿改成适配手机。 */
    @Test fun aStaleOwnerHintFitsThePhone() = onMain {
        val model = probing(SizeOwnerHint.Someone)
        model.handle(HostEvent.Ready(1u))
        assertEquals(listOf<ClientMsg>(attach()), link.sent)
        model.handle(attached(5u))
        model.handle(message(HostMsg.SnapshotEnd(sessionA)))
        assertFalse(model.fitsPhone)
        assertTrue(eventually { model.fitsPhone })
        assertEquals(claim(fit), link.sent.takeLast(2))
    }

    /** 宿主报了 owner 就不再自动适配。 */
    @Test fun anOwnerReportStopsTheProbe() = onMain {
        val model = probing()
        model.handle(HostEvent.Ready(1u))
        model.handle(attached(5u))
        model.handle(message(HostMsg.SnapshotEnd(sessionA)))
        model.handle(message(HostMsg.SizeOwner(sessionA, false, "homelab")))
        delay(50)
        assertFalse(model.fitsPhone)
        assertFalse(ClientMsg.Resize(sessionA, fit) in link.sent)
    }

    /**
     * 自动模式下 owner 变了跟着重新判断：电脑关了窗口、尺寸轮到手机时适配手机；电脑又接管时放手，不带尺寸重新连上，免得
     * 手机这边打字把尺寸抢回来。
     */
    @Test fun automaticFollowsOwnerChanges() = onMain {
        val model = followingModel()
        model.handle(message(HostMsg.SizeOwner(sessionA, false, "homelab")))
        link.clearSent()
        model.handle(message(HostMsg.SizeOwner(sessionA, true, "测试手机")))
        assertTrue(model.fitsPhone)
        assertEquals(claim(fit), link.sent)
        link.clearSent()
        model.handle(message(HostMsg.SizeOwner(sessionA, false, "homelab")))
        assertFalse(model.fitsPhone)
        assertEquals(listOf(ClientMsg.Detach(sessionA), attach()), link.sent)
    }

    /** 用户手动选了以后以用户为准：owner 再怎么变也不自动切换。 */
    @Test fun manualChoiceWins() = onMain {
        val model = followingModel()
        link.clearSent()
        model.sizePreference = SizePreference.FitPhone
        assertTrue(model.fitsPhone)
        assertEquals(claim(fit), link.sent)
        assertEquals(true, display.sizeModes.last())
        link.clearSent()
        model.handle(message(HostMsg.SizeOwner(sessionA, false, "homelab")))
        assertTrue(model.fitsPhone)
        assertTrue(link.sent.isEmpty())
        // 适配着的时候视图大小变了（转屏、弹键盘）跟着改。
        model.handle(message(HostMsg.SizeOwner(sessionA, true, "测试手机")))
        link.clearSent()
        model.updateFitSize(rotated)
        assertEquals(ClientMsg.Resize(sessionA, rotated), link.sent.first())
        // 换回跟随电脑：放手尺寸。
        link.clearSent()
        model.sizePreference = SizePreference.FollowMachine
        assertFalse(model.fitsPhone)
        assertEquals(listOf(ClientMsg.Detach(sessionA), attach()), link.sent)
        model.handle(message(HostMsg.SizeOwner(sessionA, true, "测试手机")))
        assertFalse(model.fitsPhone)
    }

    /** 手动选了适配手机的断线重连：不带尺寸连上，只发 Resize 让宿主记下，不发 Focus 抢尺寸。 */
    @Test fun reconnectingKeepsTheFitWithoutClaiming() = onMain {
        val model = followingModel()
        model.sizePreference = SizePreference.FitPhone
        model.handle(lost())
        assertEquals(TerminalModel.Phase.Disconnected(Presentation.linkFailure(LinkFailure.Timeout)), model.phase)
        link.clearSent()
        model.handle(HostEvent.Ready(2u))
        assertEquals(listOf<ClientMsg>(attach()), link.sent)
        model.handle(attached(7u))
        assertEquals(listOf<ClientMsg>(ClientMsg.Resize(sessionA, fit)), link.sent.takeLast(1))
        assertTrue(model.fitsPhone)
        // 之后视图报的尺寸（键盘、转屏）也只让宿主记下。
        link.clearSent()
        model.updateFitSize(rotated)
        assertEquals(listOf<ClientMsg>(ClientMsg.Resize(sessionA, rotated)), link.sent)
        // 用户在手机上打过字，再变尺寸时照常要。
        model.send(TerminalInput.Text("a"))
        link.clearSent()
        model.updateFitSize(fit)
        assertEquals(claim(fit), link.sent)
    }

    /** 攒着的 Claude Code 滚轮是给那次连上的 VT 的：断线重连后不发到新的 VT 上。 */
    @Test fun pendingClaudeWheelIsDroppedOnReconnect() = onMain {
        val model = model()
        val claude = SessionMeta(title = "claude", agent = Agent(AgentKind("claude"), AgentState.Working))
        fun attachClaude(channel: UInt, generation: ULong) {
            model.handle(HostEvent.Ready(generation))
            model.handle(attached(channel, meta = claude))
            model.handle(frame(FrameKind.Snapshot, channel, "\u001b[?1000h\u001b[?1006h", generation))
            model.handle(message(HostMsg.SnapshotEnd(sessionA)))
        }
        attachClaude(5u, 1u)
        // 一下滚 5 行：先发 4 个事件，剩下 1 行等过一会儿再发。
        model.send(TerminalInput.Wheel(5, 0, 0))
        assertEquals(1, link.inputs.size)
        model.handle(lost())
        attachClaude(7u, 2u)
        delay(100)
        assertEquals(1, link.inputs.size)
        model.send(TerminalInput.Wheel(1, 0, 0))
        val terminal = assertNotNull(model.terminal)
        assertEquals(terminal.encodeWheel(1, 0, 0).toList(), link.inputs.last().data.toList())
        assertEquals(7u, link.inputs.last().channel)
    }

    /** 手机适配着，断线期间电脑接过了尺寸：重连后不带尺寸连上，宿主报 owner 是电脑就跟随，不抢回来。 */
    @Test fun reconnectingAfterTheMachineTookOverFollowsIt() = onMain {
        val model = probing(SizeOwnerHint.None)
        model.handle(HostEvent.Ready(1u))
        model.handle(attached(5u))
        model.handle(message(HostMsg.SnapshotEnd(sessionA)))
        model.handle(message(HostMsg.SizeOwner(sessionA, true, "测试手机")))
        model.handle(lost())
        link.clearSent()
        model.handle(HostEvent.Ready(2u))
        model.handle(attached(7u))
        model.handle(message(HostMsg.SnapshotEnd(sessionA)))
        model.handle(message(HostMsg.SizeOwner(sessionA, false, "Ethan 的 MacBook")))
        assertFalse(model.fitsPhone)
        assertEquals(listOf<ClientMsg>(attach()), link.sent)
    }

    /** 重连后宿主没报 owner（没有别的前端在决定尺寸）：照旧适配手机。 */
    @Test fun reconnectingWithoutAnOwnerFitsThePhoneAgain() = onMain {
        val model = probing(SizeOwnerHint.None)
        model.handle(HostEvent.Ready(1u))
        model.handle(attached(5u))
        model.handle(lost())
        link.clearSent()
        model.handle(HostEvent.Ready(2u))
        assertEquals(listOf<ClientMsg>(attach()), link.sent)
        model.handle(attached(7u))
        model.handle(message(HostMsg.SnapshotEnd(sessionA)))
        assertTrue(eventually { model.fitsPhone })
        assertEquals(claim(fit), link.sent.takeLast(2))
    }

    /** Resync 后重新连时宿主说这条连接还积压着、不接订阅：过一会儿再连，不停在「连接中」。 */
    @Test fun resyncRetriesWhileTheConnectionIsBackedUp() = onMain {
        val model = live()
        model.handle(message(HostMsg.Resync(sessionA, "slow")))
        link.clearSent()
        model.handle(message(HostMsg.Error(null, sessionA, "the connection is backed up; read first")))
        assertTrue(link.sent.isEmpty())
        // 主线程忙时重试会晚到：等到它发出来，不按固定的时间看。
        assertTrue(eventually { link.sent.isNotEmpty() })
        assertEquals(listOf<ClientMsg>(attach()), link.sent)
    }

    /** 适配着的时候 Resync：宿主还记着这条连接要过的尺寸，不带尺寸重新订阅，也不再发 Focus。 */
    @Test fun resyncWhileFittingDoesNotClaimAgain() = onMain {
        val model = followingModel()
        model.sizePreference = SizePreference.FitPhone
        link.clearSent()
        model.handle(message(HostMsg.Resync(sessionA, "slow")))
        model.handle(attached(9u))
        assertEquals(listOf<ClientMsg>(attach()), link.sent)
    }

    /**
     * 跟随电脑时手机不带尺寸：打字、视图大小变了、Resync 后重新连上，都不发 Resize、Focus，也不带尺寸 Attach。宿主据此
     * 不让这条连接的打字抢走尺寸归属。
     */
    @Test fun followingTheMachineNeverAsksForASize() = onMain {
        val model = followingModel()
        link.clearSent()
        model.updateFitSize(GridSize(50u, 20u, 24u, 48u))
        model.send(TerminalInput.Text("ls\r"))
        model.handle(message(HostMsg.SizeOwner(sessionA, false, "Ethan 的 MacBook")))
        model.handle(message(HostMsg.Resync(sessionA, "slow")))
        model.handle(HostEvent.Ready(2u))
        for (sent in link.sent) {
            assertFalse(sent is ClientMsg.Resize || sent is ClientMsg.Focus, "asked for a size: $sent")
            if (sent is ClientMsg.Attach) assertNull(sent.size)
        }
        assertTrue(link.inputs.isNotEmpty())
    }

    /** agent 停下来等回答时出现快速回复；快速回复的回话由它自己收，不当作这个会话的错误。 */
    @Test fun quickReplyAppearsWhileTheAgentWaits() = onMain {
        val model = followingModel()
        assertFalse(model.isAwaitingAnswer)
        model.handle(message(HostMsg.Meta(sessionA, SessionMeta(agent = Agent(AgentKind("claude"), AgentState.Blocked)))))
        assertTrue(model.isAwaitingAnswer)
        link.clearSent()
        model.quickReply.press(QuickKey.standard[0])
        val keys = assertIs<ClientMsg.SendKeys>(link.sent.first())
        assertEquals(ClientMsg.SendKeys(keys.req, sessionA, listOf("1")), keys)
        model.handle(message(HostMsg.Done(keys.req)))
        assertEquals(1, model.quickReply.deliveredCount)
        assertEquals(TerminalModel.Phase.Live, model.phase)
        model.handle(message(HostMsg.Meta(sessionA, SessionMeta(agent = Agent(AgentKind("claude"), AgentState.Working)))))
        assertFalse(model.isAwaitingAnswer)
    }

    @Test fun keyboardStateAndRequests() = onMain {
        val model = followingModel()
        model.showKeyboard()
        assertEquals(1, display.keyboardRequests)
        model.keyboardVisible = true
        assertTrue(model.keyboardVisible)
    }

    @Test fun exitBellAndGone() = onMain {
        val model = live()
        model.handle(message(HostMsg.Bell(sessionA)))
        assertEquals(1, display.bells)
        model.handle(message(HostMsg.Exited(sessionA, 3)))
        assertEquals(TerminalModel.Phase.Exited(3), model.phase)

        val gone = TerminalModel.Phase.Gone(UiText.res(R.string.terminal_gone))
        val other = TerminalModel(sessionB, UiText.Raw("x"), link = link) {}
        other.handle(HostEvent.Ready(1u))
        other.handle(message(HostMsg.Error(null, sessionB, "no session")))
        assertEquals(gone, other.phase)
        assertTrue(other.isEnded)
        // 重新连上时照样再问一次，但不回到「连接中」，印章不闪。
        other.handle(HostEvent.Ready(2u))
        assertEquals(gone, other.phase)
    }

    /**
     * 等 Attached 时，只有宿主说没有这个会话才算结束：同一条连接上会话列表读预览出错时，宿主回的也是不带编号、带着这个
     * 会话的 Error。
     */
    @Test fun onlyNoSessionErrorsEndTheSession() = onMain {
        val model = model()
        model.handle(HostEvent.Ready(1u))
        model.handle(message(HostMsg.Error(null, sessionA, "session ${sessionA.rawValue} did not answer")))
        model.handle(message(HostMsg.Error(null, sessionA, "failed to read the screen: busy")))
        model.handle(message(HostMsg.Error(null, sessionA, "too many requests are waiting for an answer")))
        assertEquals(TerminalModel.Phase.Connecting, model.phase)
        assertFalse(model.isEnded)
        // 重放来了照常接上。
        model.handle(attached(5u))
        assertEquals(TerminalModel.Phase.Replaying, model.phase)
        // 连上以后的 no session 是读预览之类的回话，订阅还在，也不算。
        model.handle(message(HostMsg.Error(null, sessionA, "no session ${sessionA.rawValue}")))
        assertEquals(TerminalModel.Phase.Replaying, model.phase)
    }

    @Test fun openingSubscribesAndClosingHandsBack() = onMain {
        val opened = mutableListOf<SessionId>()
        val closed = mutableListOf<SessionId>()
        val model = TerminalModel(sessionA, UiText.Raw("zsh"), link = link, onOpen = { opened += it }) { closed += it }
        model.open()
        assertEquals(listOf(sessionA), opened)
        assertTrue(eventually { link.subscriberCount == 1 })
        link.emit(HostEvent.Ready(1u))
        assertTrue(eventually { attach() in link.sent })
        // 开着时一直发 KeepAwake，关掉就停。
        assertTrue(eventually { link.keepAwakes >= 2 })
        model.close()
        assertEquals(listOf(sessionA), closed)
        val stopped = link.keepAwakes
        delay(TerminalModel.keepAwakeInterval * 2)
        assertEquals(stopped, link.keepAwakes)
    }
}
