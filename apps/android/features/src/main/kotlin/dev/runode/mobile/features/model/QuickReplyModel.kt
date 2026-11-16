package dev.runode.mobile.features.model

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import dev.runode.mobile.connection.HostLink
import dev.runode.mobile.features.R
import dev.runode.mobile.protocol.ClientMsg
import dev.runode.mobile.protocol.Frame
import dev.runode.mobile.protocol.HostMsg
import dev.runode.mobile.protocol.SessionId

/**
 * 快速回复栏上的一个键：显示的字、键名（`send_keys` 的写法）、给读屏读的名字。功能键另带 [symbol]，
 * 界面按它画图标、不显示字。
 */
data class QuickKey(
    val label: String,
    val key: String,
    val accessibilityLabel: UiText,
    /** 图标的名字（`return`、`arrowtriangle.up.fill` 这类），界面换成自己的图标。 */
    val symbol: String? = null,
    /** 选提问里一项的键：所答的提问。 */
    val answers: AgentPrompt.Asked? = null,
) {
    val id: String get() = key

    companion object {
        /** 回答 agent 提问最常用的几个键：选项编号、是否、回车、Esc、上下移动选项。 */
        val standard: List<QuickKey> = listOf(
            QuickKey("1", "1", UiText.Raw("1")),
            QuickKey("2", "2", UiText.Raw("2")),
            QuickKey("3", "3", UiText.Raw("3")),
            QuickKey("y", "y", UiText.Raw("y")),
            QuickKey("n", "n", UiText.Raw("n")),
            QuickKey("⏎", "enter", UiText.res(R.string.key_return), "return"),
            QuickKey("esc", "esc", UiText.Raw("Esc")),
            QuickKey("↑", "up", UiText.res(R.string.key_up), "arrowtriangle.up.fill"),
            QuickKey("↓", "down", UiText.res(R.string.key_down), "arrowtriangle.down.fill"),
        )

        /** 终端页底部回答那一行的键：选项编号。终端本身能打字，别的键不放。 */
        val options: List<QuickKey> = standard.take(3)
    }
}

/**
 * 一个会话的快速回复：按键经 `send_keys`、文字经 `paste` 加 `send_keys ["enter"]` 发给宿主，不用连上会话（宿主按它
 * 那份 VT 当前的模式编码）。宿主回 `Done` 算送到，回带着请求编号的 `Error` 算失败。会话列表上等回答的每一行、终端页
 * 底部各有一个，连接断开、连上由它们告诉这里。
 *
 * [onDelivered] 在一批请求都送到后调，比如刷新这个会话的预览。[onStale] 在回答前重读到的屏幕上换了提问、没有发键时调，
 * 界面上的提问该跟着刷新了。
 */
class QuickReplyModel(
    val sessionId: SessionId,
    private val link: HostLink,
    connected: Boolean = true,
    private val onStale: (SessionId) -> Unit = {},
    private val onDelivered: (SessionId) -> Unit = {},
) {
    /** 文本框里的字。 */
    var draft by mutableStateOf("")

    /** 连着电脑：断开时视图禁用回复栏，按键和发送也不发。 */
    var isConnected by mutableStateOf(connected)
        internal set

    /** 每送到一批加一，视图据此轻震一下。 */
    var deliveredCount by mutableIntStateOf(0)
        private set

    /** 每失败一批加一，视图据此震一下失败的样子。 */
    var failedCount by mutableIntStateOf(0)
        private set

    var errorMessage by mutableStateOf<UiText?>(null)

    private val pending = mutableSetOf<UInt>()

    /** 还没回话的粘贴请求发的字：没送到时放回文本框，用户不用重打。 */
    private val unsent = mutableMapOf<UInt, String>()

    /** 上次放回文本框的字：文本框还是它时，再有没送到的接在后面，几条都失败时不只剩第一条。 */
    private var restored: String? = null
    private var batchFailed = false

    /** 回答前重读屏幕的请求：所答的提问和要选的编号。 */
    private val rereads = mutableMapOf<UInt, Pair<AgentPrompt.Asked, Int>>()

    /**
     * 在路上的那个回答眼下等的请求：先是重读，再是按键。同时只有一个回答在路上，前一个的键还没送到时重读到的还是所答的
     * 提问，看不出它已经答过了。
     */
    private var answering: UInt? = null

    /** 有回答在路上时又点的选项，按点的先后排着。 */
    private val queued = ArrayDeque<Pair<AgentPrompt.Asked, Int>>()

    /**
     * 在路上的是单选题的按键：它一送到这一问就答完了，agent 却未必已经重画，这时又点的对的是答过的这一问，作废。多选题点
     * 一项只切换勾选、不离开这一问，排着的照答。
     */
    private var answeredOnce = false

    val canSendDraft: Boolean get() = draft.isNotBlank()

    /**
     * 按一个键。选提问里一项的键（[QuickKey.answers]）先重读屏幕，上面还是所答的提问才按读到的提问上选这一项的键
     * （[AgentPrompt.keys]）；换了提问就不按，算失败。有回答在路上时先排着，它的键送到或失败了再重读。
     */
    fun press(key: QuickKey) {
        if (!ensureConnected()) return
        val number = key.key.toIntOrNull()
        if (key.answers != null && number != null) {
            if (answeredOnce) return
            begin(emptyList())
            if (answering == null) answer(key.answers, number) else queued.addLast(key.answers to number)
            return
        }
        val req = link.nextRequestId()
        begin(listOf(req))
        link.send(ClientMsg.SendKeys(req, sessionId, listOf(key.key)))
    }

    private fun answer(asked: AgentPrompt.Asked, number: Int) {
        val req = link.nextRequestId()
        pending += req
        rereads[req] = asked to number
        answering = req
        link.send(ClientMsg.ReadScreen(sessionId, SessionListModel.waitingReadLines, req = req))
    }

    /** 在路上的回答完了（送到、失败或换了提问）：排着的下一个接着答。 */
    private fun answerEnded() {
        answering = null
        answeredOnce = false
        val (asked, number) = queued.removeFirstOrNull() ?: return
        answer(asked, number)
    }

    /** 把文本框里的字粘贴进去再按回车。 */
    fun sendDraft() {
        val text = draft
        if (!canSendDraft || !fits(text) || !ensureConnected()) return
        draft = ""
        restored = null
        val paste = link.nextRequestId()
        val enter = link.nextRequestId()
        begin(listOf(paste, enter))
        unsent[paste] = text
        link.send(ClientMsg.Paste(paste, sessionId, text))
        link.send(ClientMsg.SendKeys(enter, sessionId, listOf("enter")))
    }

    /**
     * 这段字装得进一条控制消息：超了电脑断开连接，放回文本框重发又断。不拆成几次粘贴，那样程序收到的是几段分开的粘贴。
     * 装不下时按失败报出来，草稿留着。
     */
    private fun fits(text: String): Boolean {
        // 请求编号取最长的，算出来的就是发出去时最大的样子。
        val size = runCatching { Frame.control(ClientMsg.Paste(UInt.MAX_VALUE, sessionId, text)).payload.size }.getOrNull()
        if (size != null && size <= Frame.maxControlPayload) return true
        failedCount += 1
        errorMessage = UiText.res(R.string.reply_too_long)
        return false
    }

    /** 断着时 `HostConnection` 会把消息丢掉：不发，按失败报出来，草稿留着。 */
    private fun ensureConnected(): Boolean {
        if (isConnected) return true
        failedCount += 1
        errorMessage = UiText.res(R.string.not_connected)
        return false
    }

    /** 没送到的字放回文本框；用户已经打了新的字时不盖掉，文本框里还是刚放回的字时接在后面。 */
    private fun restoreDraft(text: String?) {
        if (text == null) return
        when {
            draft.isEmpty() -> draft = text
            draft == restored -> draft += "\n" + text
            else -> return
        }
        restored = draft
    }

    private fun begin(requests: List<UInt>) {
        if (pending.isEmpty()) batchFailed = false
        pending += requests
        errorMessage = null
    }

    /** 看是不是回给这里的请求的；是的话处理掉，返回 true。 */
    fun handle(message: HostMsg): Boolean {
        when (message) {
            is HostMsg.Done -> {
                if (message.req !in pending) return false
                pending -= message.req
                unsent.remove(message.req)
                if (message.req == answering) answerEnded()
            }
            is HostMsg.ScreenText -> {
                val req = message.req ?: return false
                val (asked, number) = rereads.remove(req) ?: return false
                val found = AgentPrompt.parse(message.text.split("\n"))?.takeIf { it.choices == asked }
                val keys = found?.keys(number)
                if (found != null && keys != null) {
                    val send = link.nextRequestId()
                    pending += send
                    answering = send
                    if (!found.isMultiSelect) {
                        answeredOnce = true
                        queued.clear()
                    }
                    link.send(ClientMsg.SendKeys(send, sessionId, keys))
                } else {
                    batchFailed = true
                    errorMessage = UiText.res(R.string.reply_question_changed)
                    onStale(sessionId)
                    answerEnded()
                }
                pending -= req
            }
            is HostMsg.Error -> {
                val req = message.req
                if (req == null || req !in pending) return false
                pending -= req
                rereads.remove(req)
                restoreDraft(unsent.remove(req))
                batchFailed = true
                errorMessage = UiText.res(R.string.reply_not_sent, Presentation.hostError(message))
                if (req == answering) answerEnded()
            }
            else -> return false
        }
        if (pending.isEmpty()) {
            if (batchFailed) {
                failedCount += 1
            } else {
                deliveredCount += 1
                onDelivered(sessionId)
            }
        }
        return true
    }

    /** 连接断了：还没回话的请求不会再有回音，算失败。 */
    fun connectionLost() {
        isConnected = false
        if (pending.isEmpty()) return
        pending.clear()
        rereads.clear()
        answering = null
        answeredOnce = false
        queued.clear()
        // 连着发了几条都没回话时按发出的先后拼回去，只放回一段的话别的就丢了。
        if (unsent.isNotEmpty()) restoreDraft(unsent.keys.sorted().mapNotNull { unsent[it] }.joinToString("\n"))
        unsent.clear()
        failedCount += 1
        errorMessage = UiText.res(R.string.reply_connection_lost)
    }
}
