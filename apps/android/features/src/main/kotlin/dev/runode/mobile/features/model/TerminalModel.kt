package dev.runode.mobile.features.model

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import dev.runode.mobile.connection.HostEvent
import dev.runode.mobile.connection.HostLink
import dev.runode.mobile.connection.LinkState
import dev.runode.mobile.features.R
import dev.runode.mobile.protocol.Agent
import dev.runode.mobile.protocol.AgentState
import dev.runode.mobile.protocol.AttachMode
import dev.runode.mobile.protocol.ClientMsg
import dev.runode.mobile.protocol.GridSize
import dev.runode.mobile.protocol.HostMsg
import dev.runode.mobile.protocol.SessionId
import dev.runode.mobile.protocol.TermSettings
import dev.runode.mobile.terminal.TerminalDisplay
import dev.runode.mobile.terminal.TerminalInput
import dev.runode.mobile.terminal.VTerminal
import java.lang.ref.WeakReference
import kotlin.math.abs
import kotlin.math.sign
import kotlin.time.ComparableTimeMark
import kotlin.time.Duration
import kotlin.time.Duration.Companion.milliseconds
import kotlin.time.Duration.Companion.seconds
import kotlin.time.TimeSource
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/** 谁的视图尺寸决定这个会话的尺寸，见宿主的 `HostMsg::SizeOwner`。 */
sealed class SizeOwnership {
    data object Unknown : SizeOwnership()

    /** 这部手机。 */
    data object Mine : SizeOwnership()

    /** 别的前端；带着它在 `Hello` 里报的设备名，没报时为空。 */
    data class Other(val name: String?) : SizeOwnership()
}

/** 用户在终端页上选的尺寸方式。 */
@Serializable
enum class SizePreference {
    /** 按情况自动选：没有别的前端在决定尺寸时适配手机，有电脑在显示时跟随电脑。 */
    @SerialName("automatic") Automatic,

    /** 按手机屏幕决定尺寸。 */
    @SerialName("fitPhone") FitPhone,

    /** 跟随电脑的尺寸，手机这边缩放、平移着看。 */
    @SerialName("followMachine") FollowMachine,
}

/** 打开终端页时从会话列表知道的、这个会话有没有别的前端在决定尺寸。 */
enum class SizeOwnerHint {
    /** 没有：电脑上没有窗口在显示它（手机新开的、后台会话）。 */
    None,

    /** 有，比如电脑上的窗口。列表的记录可能已经过时，连上后以宿主报的 `SizeOwner` 为准，等不到就当作没有。 */
    Someone,

    /** 不知道（不是从列表打开的）：连上后等宿主的 `SizeOwner`，等不到就当作没有。 */
    Unknown,
}

/**
 * 终端页的视图模型：连上一个会话、持有手机这边的那份 VT、把用户的输入编码后发出去。在主线程上用（[VTerminal] 只能在
 * 主线程上用）。
 *
 * 尺寸：自动模式下，会话没有尺寸 owner 时（[SizeOwnerHint.None]，或连上后宿主没报 `SizeOwner`，列表说有 owner 也
 * 一样，列表的记录可能已经过时）按手机屏幕适配，带尺寸 `Attach` 或发 `Resize` 加 `Focus` 接管；有电脑在决定尺寸时不带
 * 尺寸 `Attach`，跟随电脑，视图按可读的最小字号缩放、横向平移。owner 变了（`SizeOwner`）自动模式跟着重新判断；用户手动
 * 选过以后以用户为准。从适配换成跟随时 `Detach` 后不带尺寸重新 `Attach`，丢掉宿主记着的这条连接请求过的尺寸：宿主对
 * 没请求过尺寸的连接，打字不算交互，不会把尺寸抢回来。
 *
 * 重新连上时不抢尺寸：带尺寸的 `Attach` 和 `Focus` 宿主都算交互。`Resync` 后连接还在，宿主记着这条连接请求过的尺寸，
 * 不带尺寸重新 `Attach` 就够了。断线重连是一条新连接，断着的时候电脑可能已经接过尺寸，手机却没收到那条 `SizeOwner`：
 * 自动模式先当作不知道 owner，等宿主报、等不到才适配手机；手动选了适配手机的只发 `Resize` 让宿主记下，不发 `Focus`，
 * 等用户在手机上打字时才轮到手机。
 *
 * 帧的先后：宿主保证一个会话的控制消息和输出在连接上按发生的先后到达，`Resized`、`ThemeApplied` 就是 VT 要改的位置；
 * 这里严格按事件的先后处理。发出 `Attach` 到收到 `Attached` 之间，旧订阅剩下的输出和标记都丢掉，`Attached` 时按宿主
 * 给的尺寸和主题新建一份 VT 再喂重放。
 *
 * [onOpen]、[onClose] 告诉会话列表终端页开始、不再看这个会话（[SessionListModel.screenOpened]）。[ownerProbeDelay] 是
 * [ownerHint] 不为 [SizeOwnerHint.None] 时，重放完以后等宿主报 `SizeOwner` 的时间，等不到就当作没有 owner。
 * [linkState] 是这台电脑的连接状态，订阅到第一个事件之前标题下面照它显示。[onAnswerScreenChange] 在等回答
 * （[isAwaitingAnswer]）时每收到一段输出调一次，来得很勤，要自己节流。[onAnswered] 在 [quickReply] 的一批按键或文字
 * 送到后调。
 */
class TerminalModel(
    val sessionId: SessionId,
    title: UiText,
    agent: Agent? = null,
    private val link: HostLink,
    linkState: LinkState = LinkState.Idle,
    private val ownerHint: SizeOwnerHint = SizeOwnerHint.Unknown,
    sizePreference: SizePreference = SizePreference.Automatic,
    private val ownerProbeDelay: Duration = 400.milliseconds,
    private val onOpen: (SessionId) -> Unit = {},
    private val onAnswerScreenChange: (SessionId) -> Unit = {},
    onAnswered: (SessionId) -> Unit = {},
    private val onClose: (SessionId) -> Unit,
) {
    sealed class Phase {
        /** 在等宿主回 `Attached`。 */
        data object Connecting : Phase()

        /** 在喂 VT 重放，还没到 `SnapshotEnd`。 */
        data object Replaying : Phase()
        data object Live : Phase()

        /** shell 退出了，带着退出码（拿不到时为空）。 */
        data class Exited(val status: Int?) : Phase()

        /** 连接断了，正在重连；带着原因。 */
        data class Disconnected(val reason: UiText) : Phase()

        /** 宿主说没有这个会话了，或者建不了；带着给用户看的说明。 */
        data class Gone(val reason: UiText) : Phase()
    }

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    var title by mutableStateOf(title)
        private set
    var agent by mutableStateOf(agent)
        private set
    var phase by mutableStateOf<Phase>(Phase.Connecting)
        private set
    var linkState by mutableStateOf(linkState)
        private set

    /** 宿主那边会话现在的网格尺寸。 */
    var gridSize by mutableStateOf<GridSize?>(null)
        private set
    var sizeOwnership by mutableStateOf<SizeOwnership>(SizeOwnership.Unknown)
        private set
    private var sizePreferenceValue by mutableStateOf(sizePreference)

    /** 尺寸方式；用户在顶栏上选了换上，自动模式按宿主报的 owner 重新判断。 */
    var sizePreference: SizePreference
        get() = sizePreferenceValue
        set(value) {
            val wasFitting = fitsPhone
            sizePreferenceValue = value
            passiveFit = false
            if (value == SizePreference.Automatic) {
                when (sizeOwnership) {
                    SizeOwnership.Mine -> autoFit = true
                    is SizeOwnership.Other -> autoFit = false
                    SizeOwnership.Unknown -> {}
                }
            }
            applySizeMode(wasFitting)
        }

    /** 视口在看回滚历史，没跟着最新的输出；视图滚动时设。 */
    var scrolledBack by mutableStateOf(false)

    /** 软键盘（或硬件键盘的输入焦点）在终端上；视图设。 */
    var keyboardVisible by mutableStateOf(false)

    /** 终端的背景色和前景色，顶栏、底栏、空白区按它们配色（[chromeTheme]）。 */
    var background by mutableStateOf(TermSettings.default.background)
        private set
    var foreground by mutableStateOf(TermSettings.default.foreground)
        private set

    /** 终端页四周（顶栏、分屏标签、底栏、按键栏）的配色，跟着这个终端的主题走。 */
    val chromeTheme: AppTheme get() = AppTheme(background, foreground)

    /** 在等用户确认结束会话。 */
    var isConfirmingKill by mutableStateOf(false)

    /** 底部的快速回复：agent 停下来等回答时出现。 */
    val quickReply = QuickReplyModel(sessionId, link, connected = false, onDelivered = onAnswered)

    /** 手机这边的 VT。归这个模型管，换下来、页面关掉时由它 [VTerminal.close]；视图只借来读和滚视口。 */
    var terminal: VTerminal? = null
        private set

    private var settings = TermSettings.default
    private var display: WeakReference<TerminalDisplay>? = null
    private var task: Job? = null
    private var generation: ULong? = null
    private var channel: UInt? = null
    private var awaitingAttach = false

    /** 这一次 `Attach` 是怎么来的，见类型文档的「重新连上时不抢尺寸」。 */
    private var rejoin: Rejoin? = null

    /** 收到过 `Attached`：之后的 `Ready` 是断线重连。 */
    private var everAttached = false
    private var fitSize: GridSize? = null

    /** 这次连接上已经替手机要过的尺寸；不要重复发。 */
    private var requestedFit: GridSize? = null

    /**
     * 断线重连后适配手机却还没轮到手机：之后视图报的尺寸（布局、键盘、转屏）只让宿主记下，不发 `Focus` 抢尺寸；用户在
     * 手机上打字或者选尺寸方式时才不再这样。
     */
    private var passiveFit = false

    /** 自动模式下现在要不要适配手机。 */
    private var autoFit = ownerHint == SizeOwnerHint.None

    /** Claude Code 全屏界面的滚轮：按它的加速规则攒着发，见 [ClaudeWheelPacer]。 */
    private var claudeWheel = ClaudeWheelPacer()
    private var claudeWheelAnchor = 0 to 0
    private var claudeWheelRetry: Job? = null

    /** 宿主因为积压不接订阅时，过一会儿再连的那次；关页面、断线、重新连时撤掉。 */
    private var attachRetry: Job? = null

    /** 页面开着时按 [keepAwakeInterval] 发 `KeepAwake`。 */
    private var keepAwake: Job? = null

    private val currentDisplay: TerminalDisplay? get() = display?.get()

    /** 现在按手机屏幕决定尺寸。 */
    val fitsPhone: Boolean
        get() = when (sizePreference) {
            SizePreference.Automatic -> autoFit
            SizePreference.FitPhone -> true
            SizePreference.FollowMachine -> false
        }

    /** agent 停下来等用户回答。 */
    val isAwaitingAnswer: Boolean get() = agent?.state == AgentState.Blocked && !isExited

    private val isExited: Boolean get() = phase is Phase.Exited

    /** 会话结束了（shell 退出，或者宿主那边已经没有它）：不能再往里打字。 */
    val isEnded: Boolean get() = phase is Phase.Exited || phase is Phase.Gone

    /** 开始看这个会话：订阅连接的事件，连上了就 `Attach`。连接本身的开、停归会话列表管。 */
    fun open() {
        if (task != null) return
        onOpen(sessionId)
        task = scope.launch { link.events().collect { handle(it) } }
        keepAwake = scope.launch {
            while (isActive) {
                link.send(ClientMsg.KeepAwake)
                delay(keepAwakeInterval)
            }
        }
    }

    /** 不看了：停止订阅，会话交回列表（改回只看状态），关掉手机这边的 VT。 */
    fun close() {
        task?.cancel()
        task = null
        keepAwake?.cancel()
        keepAwake = null
        attachRetry?.cancel()
        attachRetry = null
        claudeWheelRetry?.cancel()
        claudeWheelRetry = null
        onClose(sessionId)
        val old = terminal
        terminal = null
        channel = null
        currentDisplay?.terminalDidReset(null, settings)
        old?.close()
    }

    /** 画终端的一方改接到别的会话上了：以后不再往它上面画，不然这个会话晚到的回话会把它换回来。 */
    fun detachDisplay(display: TerminalDisplay) {
        if (currentDisplay === display) this.display = null
    }

    /** 接上画终端的一方；已经有 VT 时马上画出来。 */
    fun attachDisplay(display: TerminalDisplay) {
        this.display = WeakReference(display)
        display.terminalDidReset(terminal, settings)
        display.terminalSizeModeDidChange(fitsPhone)
    }

    // 用户操作

    /** 把用户的输入按 VT 当前的模式编码后发给宿主。 */
    fun send(input: TerminalInput) {
        val terminal = terminal ?: return
        val channel = channel ?: return
        val generation = generation ?: return
        if (awaitingAttach) return
        if (phase != Phase.Live && phase != Phase.Replaying) return
        passiveFit = false
        if (input is TerminalInput.Wheel && agent?.kind?.label == "claude" && terminal.mouseTracking) {
            claudeWheel.add(input.lines)
            claudeWheelAnchor = input.column to input.row
            sendClaudeWheel()
            return
        }
        val bytes = when (input) {
            is TerminalInput.Key -> terminal.encode(input.input)
            is TerminalInput.Text -> input.text.encodeToByteArray()
            is TerminalInput.Paste -> terminal.encodePaste(input.text)
            is TerminalInput.Wheel -> terminal.encodeWheel(input.lines, input.column, input.row)
            is TerminalInput.Click -> terminal.encodeClick(input.column, input.row)
        }
        if (bytes.isEmpty()) return
        link.sendInput(bytes, channel, generation)
    }

    /** 发出 [claudeWheel] 现在该发的滚轮事件；剩下的不够下一个事件滚的，过一会儿再来。 */
    private fun sendClaudeWheel() {
        claudeWheelRetry?.cancel()
        claudeWheelRetry = null
        val terminal = terminal ?: return
        val channel = channel ?: return
        val generation = generation ?: return
        val (events, retryAfter) = claudeWheel.take(TimeSource.Monotonic.markNow())
        val bytes = terminal.encodeWheel(events, claudeWheelAnchor.first, claudeWheelAnchor.second)
        if (bytes.isNotEmpty()) link.sendInput(bytes, channel, generation)
        retryAfter ?: return
        claudeWheelRetry = scope.launch {
            delay(retryAfter)
            sendClaudeWheel()
        }
    }

    /** 攒着的滚轮是给这次连上的 VT 的：重新 `Attach` 或断线后丢掉，不发到新的 VT 上去。 */
    private fun resetClaudeWheel() {
        claudeWheelRetry?.cancel()
        claudeWheelRetry = null
        claudeWheel = ClaudeWheelPacer()
    }

    /** 视图按自己的大小算出来的「适配手机」的网格。正在适配时跟着改（转屏、弹键盘）。 */
    fun updateFitSize(size: GridSize) {
        fitSize = size
        if (!fitsPhone) return
        if (passiveFit) rememberFit() else requestFit()
    }

    fun showKeyboard() {
        currentDisplay?.terminalShowKeyboard()
    }

    /** 回到最底下，跟着新输出走。 */
    fun scrollToBottom() {
        terminal?.scrollToBottom()
        currentDisplay?.terminalContentDidChange()
    }

    /** 结束会话（要先让用户确认）。 */
    fun kill() {
        link.send(ClientMsg.Kill(sessionId))
    }

    /** 手动重连：等着重试的马上试，停着的重新开始。 */
    fun reconnect() {
        scope.launch { link.reconnectNow() }
    }

    // 尺寸

    /** 适配手机和跟随电脑之间换了：适配时要尺寸，跟随时放手。 */
    private fun applySizeMode(wasFitting: Boolean) {
        val fitting = fitsPhone
        currentDisplay?.terminalSizeModeDidChange(fitting)
        if (fitting == wasFitting) return
        if (fitting) requestFit() else releaseSize()
    }

    /** 要按手机屏幕的尺寸：先告诉宿主想要的尺寸，再算一次交互，轮到这部手机决定。 */
    private fun requestFit() {
        val fitSize = fitSize ?: return
        if (channel == null || awaitingAttach || fitSize == requestedFit) return
        requestedFit = fitSize
        link.send(ClientMsg.Resize(sessionId, fitSize))
        link.send(ClientMsg.Focus(sessionId, true))
    }

    /** 不再决定尺寸：断开后不带尺寸重新连上，宿主忘掉这条连接请求过的尺寸，尺寸交还给电脑上最近用过它的前端。 */
    private fun releaseSize() {
        if (generation == null || (requestedFit == null && channel == null)) return
        link.send(ClientMsg.Detach(sessionId))
        attach()
    }

    /** 重新连上后仍要适配手机：只让宿主记下手机想要的尺寸，不算交互，不从正在用的电脑那里抢。 */
    private fun rememberFit() {
        val fitSize = fitSize ?: return
        if (channel == null || awaitingAttach || fitSize == requestedFit) return
        requestedFit = fitSize
        link.send(ClientMsg.Resize(sessionId, fitSize))
    }

    // 事件

    private enum class Rejoin {
        /** `Resync` 后在同一条连接上重新订阅。 */
        Resync,

        /** 断线后的新连接。 */
        Reconnect,
    }

    /** [rejoin] 为空时适配手机就带上尺寸，算一次交互；重新连上时一律不带。 */
    private fun attach(rejoin: Rejoin? = null) {
        attachRetry?.cancel()
        attachRetry = null
        resetClaudeWheel()
        if (rejoin == null) passiveFit = false
        awaitingAttach = true
        channel = null
        this.rejoin = rejoin
        val size = if (rejoin == null && fitsPhone) fitSize else null
        if (rejoin != Rejoin.Resync) requestedFit = size
        // 结束了的会话重连时不回到「连接中」，印章不闪、按键不复活。
        if (!isEnded) phase = Phase.Connecting
        link.send(ClientMsg.Attach(sessionId, size, AttachMode.VtReplay))
    }

    internal fun handle(event: HostEvent) {
        when (event) {
            is HostEvent.State -> {
                val state = event.state
                linkState = state
                when (state) {
                    is LinkState.Connected -> {}
                    is LinkState.Waiting -> lostConnection(Presentation.linkFailure(state.reason))
                    is LinkState.Failed -> lostConnection(Presentation.linkFailure(state.failure))
                    LinkState.Connecting, LinkState.Idle ->
                        if (generation != null) lostConnection(UiText.res(R.string.link_reconnecting))
                }
            }
            is HostEvent.Ready -> {
                generation = event.generation
                quickReply.isConnected = true
                if (!everAttached) {
                    attach()
                    return
                }
                // 不改视图的显示方式：宿主报了 owner 或者等不到时 applySizeMode 再定。
                if (sizePreference == SizePreference.Automatic) {
                    sizeOwnership = SizeOwnership.Unknown
                    autoFit = false
                }
                attach(Rejoin.Reconnect)
            }
            is HostEvent.Message -> {
                val message = event.message
                if (quickReply.handle(message)) return
                if (message.sessionId != sessionId) return
                handle(message)
            }
            is HostEvent.Frame -> {
                val terminal = terminal ?: return
                if (event.generation != generation || event.frame.channel != channel || awaitingAttach) return
                terminal.feed(event.frame.payload)
                currentDisplay?.terminalContentDidChange()
                if (isAwaitingAnswer) onAnswerScreenChange(sessionId)
            }
        }
    }

    private fun lostConnection(reason: UiText) {
        resetClaudeWheel()
        generation = null
        channel = null
        awaitingAttach = false
        requestedFit = null
        quickReply.connectionLost()
        if (!isEnded) phase = Phase.Disconnected(reason)
    }

    private fun attached(attached: HostMsg.Attached) {
        val settings = attached.settings ?: TermSettings.default
        this.settings = settings
        val created = try {
            VTerminal(attached.size, settings)
        } catch (e: Exception) {
            // 没有 VT 就画不出、也打不了字：按结束了处理，页面盖上印章、写出原因，不停在「连接中」。
            awaitingAttach = false
            phase = Phase.Gone(UiText.res(R.string.terminal_create_failed, e.message ?: e.toString()))
            return
        }
        val old = terminal
        terminal = created
        channel = attached.channel
        awaitingAttach = false
        everAttached = true
        gridSize = attached.size
        background = settings.background
        foreground = settings.foreground
        attached.meta.displayTitle?.let { title = UiText.Raw(it) }
        agent = attached.meta.agent
        if (phase !is Phase.Exited) phase = Phase.Replaying
        currentDisplay?.terminalDidReset(created, settings)
        old?.close()
        // 适配手机，但 Attach 时还不知道视图多大（或者重新连上时没带）：现在补上。
        if (fitsPhone && requestedFit == null) {
            if (rejoin == Rejoin.Reconnect) {
                passiveFit = true
                rememberFit()
            } else {
                requestFit()
            }
        }
    }

    private fun handle(message: HostMsg) {
        when (message) {
            is HostMsg.Attached -> if (message.mode != AttachMode.MetaOnly) attached(message)
            is HostMsg.SnapshotEnd -> {
                if (awaitingAttach || channel == null) return
                if (phase == Phase.Replaying) phase = Phase.Live
                probeOwnerIfNeeded()
            }
            is HostMsg.Resized -> {
                val terminal = terminal ?: return
                if (awaitingAttach) return
                terminal.resize(message.size)
                gridSize = message.size
                currentDisplay?.terminalContentDidChange()
            }
            is HostMsg.ThemeApplied -> {
                val terminal = terminal ?: return
                if (awaitingAttach) return
                val settings = message.settings
                this.settings = settings
                background = settings.background
                foreground = settings.foreground
                terminal.applyTheme(settings)
                currentDisplay?.terminalSettingsDidChange(settings)
            }
            is HostMsg.Meta -> {
                message.meta.displayTitle?.let { title = UiText.Raw(it) }
                agent = message.meta.agent
            }
            is HostMsg.Resync -> {
                if (generation == null) return
                attach(Rejoin.Resync)
            }
            is HostMsg.Exited -> phase = Phase.Exited(message.status)
            is HostMsg.Bell -> currentDisplay?.terminalDidRingBell()
            is HostMsg.SizeOwner -> {
                sizeOwnership = if (message.mine) SizeOwnership.Mine else SizeOwnership.Other(message.owner)
                if (sizePreference == SizePreference.Automatic) {
                    val wasFitting = fitsPhone
                    autoFit = message.mine
                    applySizeMode(wasFitting)
                }
            }
            is HostMsg.Error -> when {
                awaitingAttach && message.message.startsWith(HostMsg.backedUpPrefix) -> {
                    // 宿主那边这条连接还积压着（多半刚因此发来 Resync），不接新的订阅：等它发完再连。
                    val rejoin = rejoin
                    val generation = generation
                    attachRetry?.cancel()
                    attachRetry = scope.launch {
                        delay(100.milliseconds)
                        if (!awaitingAttach || generation == null || this@TerminalModel.generation != generation) {
                            return@launch
                        }
                        attach(rejoin)
                    }
                }
                awaitingAttach && message.message.startsWith(HostMsg.noSessionPrefix) -> {
                    // 宿主说没有这个会话。同一条连接上会话列表读预览出错（读屏幕超时之类）也回不带编号、带着会话的
                    // Error，那些不算会话结束。
                    awaitingAttach = false
                    phase = Phase.Gone(UiText.res(R.string.terminal_gone))
                }
            }
            else -> {}
        }
    }

    /**
     * 列表说有 owner 或不知道有没有、或者断线重连了：重放完以后宿主若有 owner 会马上报 `SizeOwner`；等一会儿没等到就
     * 当作没有，自动模式改成适配手机。
     */
    private fun probeOwnerIfNeeded() {
        if (ownerHint == SizeOwnerHint.None && rejoin != Rejoin.Reconnect) return
        if (sizePreference != SizePreference.Automatic || sizeOwnership != SizeOwnership.Unknown) return
        scope.launch {
            delay(ownerProbeDelay)
            if (sizePreference != SizePreference.Automatic || sizeOwnership != SizeOwnership.Unknown) return@launch
            if (awaitingAttach || channel == null) return@launch
            val wasFitting = fitsPhone
            autoFit = true
            applySizeMode(wasFitting)
        }
    }

    companion object {
        /** 间隔够短，手机的 Wi-Fi 才一直醒着，不会省电到收发卡一两秒。 */
        internal val keepAwakeInterval: Duration = 200.milliseconds
    }
}

/**
 * Claude Code 全屏界面滚轮加速的镜像：把「想滚几行」换成「该发几个滚轮事件」，让它滚的行数跟着手指走。
 *
 * 它（2.1 版）对 runode 这种不算 wheelFlood 的 macOS 终端的规则：事件间隔短时倍率累加、一次滚好几行，间隔长时只滚
 * 1 行；换方向的头一个事件被吞掉，吞掉后很快又折回原方向会换成猛得多的另一套加速。手机按一行发一个事件会被放大好几倍，
 * 所以这里按同样的规则算每个事件会滚几行，攒够了才发，折回时等过那段时间。
 *
 * shortcut: 照抄 Claude Code 的常量，假定用户没改滚动速度、没关加速，它改了算法时这里要跟着改
 */
internal class ClaudeWheelPacer {
    /** 还没发出去的行数，负数往上。 */
    private var pending = 0

    /** 它记着的上一个方向（吞掉的换向事件不改它）、上一个事件的时刻和倍率。 */
    private var direction = 0
    private var last: ComparableTimeMark? = null
    private var multiplier = 1.0

    /** 换方向的头一个事件刚被吞掉。 */
    private var flipPending = false

    fun add(lines: Int) {
        pending += lines
    }

    /**
     * 现在该发的滚轮事件数（负数往上）；剩下的行数不够下一个事件滚的，或者要等过折回的 200 毫秒时，带上过多久再来取。
     */
    fun take(now: ComparableTimeMark): Pair<Int, Duration?> {
        var events = 0
        while (pending != 0) {
            val step = pending.sign
            val gap = last?.let { now - it } ?: 1.seconds
            val rows: Int
            if (flipPending) {
                if (step == direction && gap <= 200.milliseconds) return events to (210.milliseconds - gap)
                rows = 1
            } else if (direction != 0 && step != direction) {
                rows = 0
            } else {
                rows = if (gap > 40.milliseconds) 1 else minOf(multiplier + 0.3, 6.0).toInt()
            }
            // 不到 40 毫秒才会一次滚两行以上；等过了 40 毫秒，下一个事件就只滚 1 行。
            if (rows > abs(pending)) return events to (50.milliseconds - gap)
            if (flipPending) {
                flipPending = false
                direction = step
                multiplier = 1.0
            } else if (rows == 0) {
                flipPending = true
            } else {
                multiplier = if (gap > 40.milliseconds) 1.0 else minOf(multiplier + 0.3, 6.0)
                direction = step
            }
            last = now
            events += step
            pending -= step * rows
        }
        return events to null
    }
}
