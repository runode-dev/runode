package dev.runode.mobile.features.model

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.unit.IntSize
import dev.runode.mobile.connection.LinkFailure
import dev.runode.mobile.connection.MachineRecord
import dev.runode.mobile.connection.ScreenConnection
import dev.runode.mobile.connection.ScreenEvent
import dev.runode.mobile.connection.UnlockPasswordStore
import dev.runode.mobile.features.R
import dev.runode.mobile.protocol.Codec
import dev.runode.mobile.protocol.ScreenClientMsg
import dev.runode.mobile.protocol.ScreenDisplay
import dev.runode.mobile.protocol.ScreenHostMsg
import dev.runode.mobile.protocol.ScreenInput
import dev.runode.mobile.protocol.ScreenMods
import dev.runode.mobile.protocol.ScreenMouseButton
import dev.runode.mobile.protocol.ScreenStopCause
import dev.runode.mobile.protocol.VideoFrame
import javax.crypto.Cipher
import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt
import kotlin.math.roundToLong
import kotlin.time.ComparableTimeMark
import kotlin.time.Duration
import kotlin.time.Duration.Companion.milliseconds
import kotlin.time.Duration.Companion.seconds
import kotlin.time.TimeSource
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/** 连上一台电脑的远程桌面。App 入口给真的（`ScreenConnection.open`），演示模式和测试不给。 */
typealias ScreenOpener = suspend (MachineRecord) -> ScreenConnection

/**
 * 请用户验证身份（BiometricPrompt 的 `CryptoObject`），[reason] 是给用户看的用途。验证过时返回能用的 [Cipher]，用户取消、
 * 没过时为空。
 */
typealias UnlockAuthenticator = suspend (cipher: Cipher, reason: UiText) -> Cipher?

/**
 * 远程桌面页：连上电脑的远程桌面、按屏幕的样子要画面，画面交给解码器（[attachOutput]），手势和键盘变成电脑上的键鼠。在主线程
 * 上用。
 */
class ScreenModel(
    val machine: MachineRecord,
    private val open: ScreenOpener,
    private val passwords: UnlockPasswordStore? = null,
    private val codecs: VideoCodecSupport = MediaCodecSupport,
) {
    sealed class Phase {
        data object Connecting : Phase()

        data object Live : Phase()

        data class Failed(val reason: UiText) : Phase()
    }

    /** 触屏：手指点哪里电脑上就点哪里。触控板：单指挪的是一个虚拟光标，点按、拖动都落在光标上。 */
    enum class PointerMode {
        Touch,
        Trackpad,
    }

    enum class DragPhase {
        Began,
        Moved,
        Ended,
    }

    private class LastTap(val view: Offset, val x: Double, val y: Double, val at: ComparableTimeMark)

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    var phase by mutableStateOf<Phase>(Phase.Connecting)
        private set

    /** 电脑报的各块屏幕，主屏排第一。 */
    var displays by mutableStateOf<List<ScreenDisplay>>(emptyList())
        private set

    /** 正在看的那块屏幕，开始采集后才有。 */
    var display by mutableStateOf<ScreenDisplay?>(null)
        private set

    /** 电脑缺的授权，给人看的名字；都有时为空。 */
    var missingPermissions by mutableStateOf<List<UiText>>(emptyList())
        private set

    /** 电脑缺屏幕录制的授权：缺它时看不了画面，连不上就是因为它。 */
    var lacksScreenCapture by mutableStateOf(false)
        private set

    /** 画面的像素尺寸，开始采集后才有。 */
    var videoSize by mutableStateOf<IntSize?>(null)
        private set

    /** 按住不放的修饰键，下一次按键或输入文字时带上，用完松开。 */
    var stickyMods by mutableStateOf(ScreenMods())

    /** 电脑锁着屏。 */
    var locked by mutableStateOf(false)
        private set

    /** 发了密码，在等电脑解开。 */
    var unlocking by mutableStateOf(false)
        private set

    /** 要用户输登录密码：没存过，存的不对，或者用户没过身份验证。 */
    var askingPassword by mutableStateOf(false)

    /** 上一次解锁为什么没成，给人看。 */
    var unlockError by mutableStateOf<UiText?>(null)
        private set

    var pointerMode by mutableStateOf(PointerMode.Touch)

    /** 触控板模式的光标位置，画面上的比例。 */
    var cursor: Pair<Double, Double> = 0.5 to 0.5
        private set

    /** 光标挪了：显示层把光标画到新位置。 */
    var onCursor: ((Double, Double) -> Unit)? = null

    /** 取、存登录密码前请用户验证身份，界面层给（BiometricPrompt 要 Activity）；为空时存着的密码取不出来，也不存。 */
    var authenticate: UnlockAuthenticator? = null

    /** 发出密码后最多等电脑这么久解开，过了当成密码不对。 */
    internal var unlockWait: Duration = 8.seconds

    /** 这次锁屏已经自动解过一次：没解开时不再自动试，免得一直拿错的密码去敲。 */
    private var triedAutomatically = false

    /** 解开以后要存下的密码（用户勾了记住）。 */
    private var passwordToSave: String? = null

    /** 这次发的是存着的密码：没解开时删掉它。 */
    private var sentStoredPassword = false

    /** 在等身份验证或等电脑解开；解开、没解开、断开时取消，迟到的结果不再算数。 */
    private var unlockJob: Job? = null
    private var connection: ScreenConnection? = null
    private var task: Job? = null

    /** 第几次 [start]，旧的那次结束时据此认出自己已经过时。 */
    private var attempt = 0

    /** 用户选的屏幕，断线重连后接着看它。 */
    private var chosenDisplay: UInt? = null
    private var lastTap: LastTap? = null

    /** 画面画到哪里，界面层给；[decoder] 按它建，断开时放掉，surface 还在就下次连上再建。 */
    private var output: VideoOutput? = null
    private var decoder: VideoDecoder? = null

    /** 连上并开始采集；已经在连或者连着时什么都不做。 */
    fun start() {
        if (task != null) return
        phase = Phase.Connecting
        // 授权按这次连上时电脑报的算，上一次的不沿用。
        setPermissions(screenCapture = true, postEvent = true)
        attempt += 1
        val attempt = attempt
        task = scope.launch {
            val failure = try {
                val connection = open(machine)
                if (!isActive || this@ScreenModel.attempt != attempt) {
                    connection.close()
                    return@launch
                }
                run(connection)
                null
            } catch (_: CancellationException) {
                null
            } catch (error: Exception) {
                describe(error)
            }
            // retry 已经开了新的一次：这次的结果不算数，别把新的状态冲掉。
            if (this@ScreenModel.attempt != attempt) return@launch
            // 电脑说了为什么停（Stopped）以后紧接着断开连接，断开的错误不盖掉它说的原因。
            if (failure != null && phase !is Phase.Failed) phase = Phase.Failed(failure)
            connection = null
            task = null
            releaseDecoder()
            forgetLock()
        }
    }

    private suspend fun run(connection: ScreenConnection) {
        this.connection = connection
        displays = connection.displays
        setPermissions(connection.screenCapture, connection.postEvent)
        request(displays.firstOrNull { it.id == chosenDisplay } ?: displays[0])
        connection.events().collect { handle(it) }
    }

    internal fun handle(event: ScreenEvent) {
        when (event) {
            is ScreenEvent.Video -> show(event.frame)
            is ScreenEvent.Message -> when (val message = event.message) {
                is ScreenHostMsg.Started -> {
                    display = displays.firstOrNull { it.id == message.display } ?: display
                    val size = IntSize(message.width.toInt(), message.height.toInt())
                    videoSize = size
                    decoder?.reset(size.width, size.height)
                    phase = Phase.Live
                }
                is ScreenHostMsg.Stopped ->
                    phase = Phase.Failed(message.cause?.let(::explanation) ?: UiText.Raw(message.reason))
                is ScreenHostMsg.Permissions -> setPermissions(message.screenCapture, message.postEvent)
                is ScreenHostMsg.Lock -> {
                    locked = message.locked
                    if (!message.locked) {
                        unlocked()
                    } else if (!triedAutomatically) {
                        triedAutomatically = true
                        unlock()
                    }
                }
                is ScreenHostMsg.UnlockFailed -> unlockFailed(UiText.res(R.string.screen_unlock_unsupported))
                else -> {}
            }
        }
    }

    // 画面

    /** 画面画到 [output] 上，换掉原来的；surface 建好时调。 */
    fun attachOutput(output: VideoOutput) {
        releaseDecoder()
        this.output = output
    }

    /** 不再往原来的 surface 上画；surface 销毁前调，返回时解码器已经停了。 */
    fun detachOutput() {
        releaseDecoder()
        output = null
    }

    private fun show(frame: VideoFrame) {
        val decoder = decoder ?: output?.let { output ->
            output.decoder { scope.launch { requestKeyframe() } }.also { made ->
                videoSize?.let { made.reset(it.width, it.height) }
                decoder = made
            }
        } ?: return
        decoder.decode(frame)
    }

    private fun releaseDecoder() {
        decoder?.release()
        decoder = null
    }

    /** 解不出画面（还没收到关键帧、解码出错）时要一个关键帧。 */
    fun requestKeyframe() {
        connection?.send(ScreenClientMsg.Keyframe)
    }

    // 解锁

    /** 用存着的密码解锁（要过身份验证）；没存过、没过验证时请用户输。 */
    fun unlock() {
        if (!locked || unlocking) return
        val passwords = passwords
        if (passwords == null || !passwords.hasPassword(machine.id)) {
            askingPassword = true
            return
        }
        // 等身份验证时也算在解锁：再点一次按钮、电脑又报一次锁屏时不再弹一次。
        unlocking = true
        unlockJob = scope.launch {
            val password = storedPassword(passwords)
            if (!isActive) return@launch
            if (password != null) {
                sendUnlock(password, save = null, stored = true)
            } else {
                unlocking = false
                askingPassword = true
            }
        }
    }

    /** 取出存着的密码；没过验证、取不出来时为空。 */
    private suspend fun storedPassword(passwords: UnlockPasswordStore): String? {
        val authenticate = authenticate ?: return null
        return try {
            val cipher = passwords.cipherForReading(machine.id) ?: return null
            val verified = authenticate(cipher, UiText.res(R.string.screen_unlock_reason)) ?: return null
            passwords.password(machine.id, verified)
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            null
        }
    }

    /**
     * 用用户输的密码解锁；[remember] 时解开以后存下（存时要过身份验证，之后取也要）。电脑没锁着（包括已经断开）时什么都
     * 不发。
     */
    fun unlock(password: String, remember: Boolean) {
        askingPassword = false
        if (!locked) return
        sendUnlock(password, save = if (remember) password else null, stored = false)
    }

    private fun sendUnlock(password: String, save: String?, stored: Boolean) {
        unlocking = true
        unlockError = null
        passwordToSave = save
        sentStoredPassword = stored
        connection?.send(ScreenClientMsg.Unlock(password))
        unlockJob?.cancel()
        val wait = unlockWait
        unlockJob = scope.launch {
            delay(wait)
            unlockFailed(UiText.res(R.string.screen_still_locked))
        }
    }

    private fun unlocked() {
        unlockJob?.cancel()
        val save = passwordToSave.takeIf { unlocking }
        unlocking = false
        passwordToSave = null
        unlockError = null
        askingPassword = false
        triedAutomatically = false
        if (save != null) savePassword(save)
    }

    /** 解开了，存下用户要记住的密码；没过验证就不存。 */
    private fun savePassword(password: String) {
        val passwords = passwords ?: return
        val authenticate = authenticate ?: return
        val machine = machine.id
        scope.launch {
            try {
                val cipher = passwords.cipherForSaving(machine)
                val verified = authenticate(cipher, UiText.res(R.string.screen_remember_reason)) ?: return@launch
                passwords.save(password, machine, verified)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
            }
        }
    }

    private fun unlockFailed(message: UiText) {
        if (!unlocking) return
        unlockJob?.cancel()
        if (sentStoredPassword) passwords?.deletePassword(machine.id)
        unlocking = false
        passwordToSave = null
        unlockError = message
        askingPassword = true
    }

    /** 换到另一块屏幕看。 */
    fun switchDisplay(display: ScreenDisplay) {
        if (display.id == this.display?.id) return
        chosenDisplay = display.id
        request(display)
    }

    /** 屏幕在列表里的名字：主屏、副屏 1、副屏 2…，带上分辨率。 */
    fun name(display: ScreenDisplay): UiText {
        val size = "${display.width}×${display.height}"
        val index = displays.indexOf(display)
        if (index <= 0) return UiText.res(R.string.screen_main_display, size)
        return UiText.res(R.string.screen_other_display, index, size)
    }

    private fun request(display: ScreenDisplay) {
        val connection = connection ?: return
        val (width, height) = captureSize(display)
        connection.send(startMessage(display, width, height, codecs.decodable(width.toInt(), height.toInt())))
    }

    /** 断开；离开页面、退到后台时调。解码器跟着停掉。 */
    fun close() {
        attempt += 1
        task?.cancel()
        task = null
        connection?.close()
        connection = null
        releaseDecoder()
        forgetLock()
    }

    /** 断开不算密码不对：作废在等的解锁、收起输密码的框，新的连接会重新报锁屏。 */
    private fun forgetLock() {
        unlockJob?.cancel()
        unlocking = false
        passwordToSave = null
        askingPassword = false
        unlockError = null
        locked = false
        triedAutomatically = false
    }

    /** 断了以后重新连。 */
    fun retry() {
        close()
        start()
    }

    // 输入。坐标是画面上的比例，(0, 0) 是左上角。

    /** 点一下：挪过去再按下松开左键。[view] 是手机上点的位置，用来认双击。 */
    fun tap(x: Double, y: Double, view: Offset) {
        val (targetX, targetY) = tapTarget(x, y, view)
        click(ScreenMouseButton.Left, targetX, targetY)
    }

    /** 这次点按落在电脑上的哪里：紧跟着上一次、离得近时落在上一次的位置上。 */
    internal fun tapTarget(
        x: Double,
        y: Double,
        view: Offset,
        now: ComparableTimeMark = TimeSource.Monotonic.markNow(),
    ): Pair<Double, Double> {
        var target = x to y
        val last = lastTap
        if (last != null && now - last.at <= doubleTapInterval && (view - last.view).getDistance() <= doubleTapSlop) {
            target = last.x to last.y
        }
        lastTap = LastTap(view, target.first, target.second, now)
        return target
    }

    fun rightClick(x: Double, y: Double) {
        click(ScreenMouseButton.Right, x, y)
    }

    private fun click(button: ScreenMouseButton, x: Double, y: Double) {
        val mods = takeMods()
        send(ScreenInput.MouseMove(x, y))
        send(ScreenInput.MouseButton(button, down = true, x = x, y = y, mods = mods))
        send(ScreenInput.MouseButton(button, down = false, x = x, y = y, mods = mods))
    }

    /** 拖动：按下、挪动、松开左键。 */
    fun drag(phase: DragPhase, x: Double, y: Double) {
        when (phase) {
            DragPhase.Began -> {
                send(ScreenInput.MouseMove(x, y))
                send(ScreenInput.MouseButton(ScreenMouseButton.Left, down = true, x = x, y = y))
            }
            DragPhase.Moved -> send(ScreenInput.MouseMove(x, y))
            DragPhase.Ended -> send(ScreenInput.MouseButton(ScreenMouseButton.Left, down = false, x = x, y = y))
        }
    }

    /** 触控板模式：光标挪 ([dx], [dy])，是画面上的比例；到边为止。 */
    fun moveCursor(dx: Double, dy: Double) {
        cursor = clamped(cursor.first + dx) to clamped(cursor.second + dy)
        onCursor?.invoke(cursor.first, cursor.second)
        send(ScreenInput.MouseMove(cursor.first, cursor.second))
    }

    /** 触控板模式：在光标处点一下。 */
    fun clickAtCursor(button: ScreenMouseButton) {
        click(button, cursor.first, cursor.second)
    }

    /** 触控板模式：在光标处按下、松开左键；中间用 [moveCursor] 拖。 */
    fun dragAtCursor(down: Boolean) {
        send(ScreenInput.MouseButton(ScreenMouseButton.Left, down = down, x = cursor.first, y = cursor.second))
    }

    /**
     * 两指滑动，[dx]、[dy] 是手机上的像素，和手指拖动的方向一致；按画面在手机上的宽度（[viewWidth]，同样是像素）换成电脑上
     * 的点数。
     */
    fun scroll(dx: Float, dy: Float, viewWidth: Float) {
        val display = display ?: return
        if (viewWidth <= 0f) return
        val scale = display.width.toFloat() / viewWidth
        val x = (dx * scale).roundToInt()
        val y = (dy * scale).roundToInt()
        if (x == 0 && y == 0) return
        send(ScreenInput.Scroll(x, y))
    }

    /** 键盘上输入的文字；带着按住的修饰键时变成按键（Ctrl+C 这类快捷键）。 */
    fun type(text: String) {
        val mods = takeMods()
        send(input(text, mods))
    }

    /** 按一个键，[key] 是电脑那边 `Key` 的名字。 */
    fun press(key: String) {
        send(ScreenInput.Key(key, takeMods()))
    }

    private fun takeMods(): ScreenMods {
        val mods = stickyMods
        stickyMods = ScreenMods()
        return mods
    }

    private fun send(input: ScreenInput) {
        connection?.send(ScreenClientMsg.Input(input))
    }

    private fun setPermissions(screenCapture: Boolean, postEvent: Boolean) {
        missingPermissions = missing(screenCapture, postEvent)
        lacksScreenCapture = !screenCapture
    }

    private fun describe(error: Exception): UiText = when (error) {
        is LinkFailure -> Presentation.linkFailure(error)
        else -> UiText.Raw(error.message ?: error.toString())
    }

    // 不碰连接的计算
    companion object {
        /** 画面最长的一边最多这么多像素：再大手机屏幕也显示不出来，只多占带宽。 */
        val maxSide: UInt = 2560u
        val fps: UInt = 60u

        /** 平均码率，单位 bit/s。 */
        val bitrate: UInt = 10_000_000u

        /**
         * 两次点按隔得这么近（时间和手机上的像素）时算双击，第二次点在第一次的位置上：手指点不准，电脑认双击要求两次落在
         * 几个点以内。
         */
        val doubleTapInterval: Duration = 400.milliseconds
        const val doubleTapSlop: Float = 24f

        /** 要的画面尺寸：电脑屏幕的像素数，最长的一边超过 [maxSide] 时等比缩小，取偶数。 */
        internal fun captureSize(display: ScreenDisplay): Pair<UInt, UInt> {
            val width = max(display.pixelWidth, 2u)
            val height = max(display.pixelHeight, 2u)
            val scale = min(1.0, maxSide.toDouble() / max(width, height).toDouble())
            fun even(value: UInt): UInt = (value.toDouble() * scale).roundToLong().toUInt() and 1u.inv()
            return even(width) to even(height)
        }

        /** 开始采集 [display]，画面 [width]×[height]，告诉电脑这部手机解得了的 [codecs]。 */
        internal fun startMessage(display: ScreenDisplay, width: UInt, height: UInt, codecs: List<Codec>) =
            ScreenClientMsg.Start(display.id, width, height, fps, bitrate, codecs)

        /** 文字和修饰键变成输入：带修饰键的单个字母、数字按键处理，别的照文字输入。 */
        internal fun input(text: String, mods: ScreenMods): ScreenInput {
            if (text == "\n") return ScreenInput.Key("Enter", mods)
            if (mods.isEmpty || text.length != 1 || text[0].code >= 0x80) return ScreenInput.Text(text)
            val char = text[0]
            if (char.isLetter()) return ScreenInput.Key(char.uppercase(), mods)
            if (char.isDigit()) return ScreenInput.Key("Digit$char", mods)
            return ScreenInput.Text(text)
        }

        /** 电脑归了类的停下原因给人看的说明和办法；手机上停下后不自己重连，都说办好了再重新连接。 */
        internal fun explanation(cause: ScreenStopCause): UiText = UiText.res(
            when (cause) {
                ScreenStopCause.SignedOut -> R.string.screen_stopped_signed_out
                ScreenStopCause.NoDesktop -> R.string.screen_stopped_no_desktop
                ScreenStopCause.OtherUser -> R.string.screen_stopped_other_user
                ScreenStopCause.NoScreenCapture -> R.string.screen_stopped_no_screen_capture
                ScreenStopCause.Declined -> R.string.screen_stopped_declined
                ScreenStopCause.NoEncoder -> R.string.screen_stopped_no_encoder
            },
        )

        internal fun missing(screenCapture: Boolean, postEvent: Boolean): List<UiText> = buildList {
            if (!screenCapture) add(UiText.res(R.string.screen_permission_screen_recording))
            if (!postEvent) add(UiText.res(R.string.screen_permission_accessibility))
        }

        internal fun clamped(fraction: Double): Double = min(max(fraction, 0.0), 1.0)
    }
}
