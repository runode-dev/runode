package dev.runode.mobile.features.model

import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.unit.IntSize
import dev.runode.mobile.connection.MemoryUnlockPasswords
import dev.runode.mobile.connection.ScreenEvent
import dev.runode.mobile.connection.UnlockPasswordStore
import dev.runode.mobile.protocol.Codec
import dev.runode.mobile.protocol.ScreenClientMsg
import dev.runode.mobile.protocol.ScreenDisplay
import dev.runode.mobile.protocol.ScreenHostMsg
import dev.runode.mobile.protocol.ScreenInput
import dev.runode.mobile.protocol.ScreenMods
import dev.runode.mobile.protocol.ScreenStopCause
import dev.runode.mobile.protocol.VideoFrame
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlin.time.Duration.Companion.milliseconds
import kotlin.time.Duration.Companion.seconds
import kotlin.time.TimeSource
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.test.runTest
import org.junit.Rule

/** 记下交来的帧和调用的假解码器。 */
private class FakeDecoder : VideoDecoder {
    val frames = mutableListOf<VideoFrame>()
    val resets = mutableListOf<IntSize>()
    var released = false

    override fun reset(width: Int, height: Int) {
        resets += IntSize(width, height)
    }

    override fun decode(frame: VideoFrame) {
        frames += frame
    }

    override fun release() {
        released = true
    }
}

/** 记下建过的假解码器。 */
private class FakeOutput : VideoOutput {
    val decoders = mutableListOf<FakeDecoder>()

    override fun decoder(onNeedKeyframe: () -> Unit): VideoDecoder = FakeDecoder().also { decoders += it }
}

private val unopenable: ScreenOpener = { throw CancellationException() }

class ScreenModelTests {
    @get:Rule val main = MainDispatcherRule()

    /** 画面不比手机能显示的大：长边超过上限时等比缩小，宽高取偶数（4:2:0 要偶数）。 */
    @Test fun captureSizeFitsTheLongSide() {
        val retina = ScreenDisplay(id = 1u, width = 1728u, height = 1117u, pixelWidth = 3456u, pixelHeight = 2234u)
        assertEquals(2560u to 1654u, ScreenModel.captureSize(retina))
        val small = ScreenDisplay(id = 1u, width = 1280u, height = 801u, pixelWidth = 1280u, pixelHeight = 801u)
        assertEquals(1280u to 800u, ScreenModel.captureSize(small))
    }

    /** 开始采集时带上手机解得了的编码，照想要的先后。 */
    @Test fun startListsTheDecodableCodecs() {
        val display = ScreenDisplay(id = 2u, width = 1280u, height = 800u, pixelWidth = 2560u, pixelHeight = 1600u)
        val start = ScreenModel.startMessage(display, 2560u, 1600u, listOf(Codec.H264))
        assertEquals(ScreenClientMsg.Start(2u, 2560u, 1600u, 60u, 10_000_000u, listOf(Codec.H264)), start)
    }

    /** 带着按住的修饰键输入单个字母、数字时按快捷键发，别的照文字发。 */
    @Test fun textWithModsBecomesKeys() {
        val cmd = ScreenMods(cmd = true)
        assertEquals(ScreenInput.Key("C", cmd), ScreenModel.input("c", cmd))
        assertEquals(ScreenInput.Key("Digit1", cmd), ScreenModel.input("1", cmd))
        assertEquals(ScreenInput.Text("你"), ScreenModel.input("你", cmd))
        assertEquals(ScreenInput.Text("c"), ScreenModel.input("c", ScreenMods()))
        assertEquals(ScreenInput.Key("Enter", ScreenMods()), ScreenModel.input("\n", ScreenMods()))
    }

    @Test fun missingPermissionsAreNamed() {
        assertTrue(ScreenModel.missing(screenCapture = true, postEvent = true).isEmpty())
        assertEquals(
            listOf("Screen Recording", "Accessibility"),
            ScreenModel.missing(screenCapture = false, postEvent = false).map { English.render(it) },
        )
    }

    /** 电脑说了为什么停：归了类的给说明，没归类的照原文。 */
    @Test fun stoppedExplainsTheCause() {
        val model = ScreenModel(machineRecord(), unopenable)
        model.handle(ScreenEvent.Message(ScreenHostMsg.Stopped("no encoder", ScreenStopCause.NoEncoder)))
        val failed = assertIs<ScreenModel.Phase.Failed>(model.phase)
        assertTrue(English.render(failed.reason).startsWith("That computer has no usable video encoder"))
        model.handle(ScreenEvent.Message(ScreenHostMsg.Stopped("display unplugged", null)))
        assertEquals(ScreenModel.Phase.Failed(UiText.Raw("display unplugged")), model.phase)
    }
}

class ScreenVideoTests {
    @get:Rule val main = MainDispatcherRule()

    private val started = ScreenEvent.Message(ScreenHostMsg.Started(display = 1u, width = 1280u, height = 800u))

    private fun frame(keyframe: Boolean) =
        VideoFrame(byteArrayOf(0, 0, 0, 1, 0x26), keyframe, if (keyframe) listOf(byteArrayOf(0x40)) else emptyList(), 0)

    /** 第一帧来时才建解码器，按已经知道的画面尺寸配好；之后的帧交给同一个。 */
    @Test fun framesGoToTheDecoderOfTheAttachedOutput() {
        val model = ScreenModel(machineRecord(), unopenable)
        val output = FakeOutput()
        model.handle(ScreenEvent.Video(frame(keyframe = true)))
        model.attachOutput(output)
        assertTrue(output.decoders.isEmpty())
        model.handle(started)
        assertEquals(IntSize(1280, 800), model.videoSize)
        assertEquals(ScreenModel.Phase.Live, model.phase)
        model.handle(ScreenEvent.Video(frame(keyframe = true)))
        model.handle(ScreenEvent.Video(frame(keyframe = false)))
        val decoder = output.decoders.single()
        assertEquals(listOf(IntSize(1280, 800)), decoder.resets)
        assertEquals(listOf(true, false), decoder.frames.map { it.keyframe })
    }

    /** 重新开始采集（切了屏幕）时解码器丢掉旧格式，按新尺寸等关键帧。 */
    @Test fun restartingTheCaptureResetsTheDecoder() {
        val model = ScreenModel(machineRecord(), unopenable)
        val output = FakeOutput()
        model.attachOutput(output)
        model.handle(ScreenEvent.Video(frame(keyframe = true)))
        model.handle(ScreenEvent.Message(ScreenHostMsg.Started(display = 2u, width = 2560u, height = 1440u)))
        assertEquals(listOf(IntSize(2560, 1440)), output.decoders.single().resets)
    }

    /** surface 没了、断开时解码器停掉；surface 还在时重新连上再建一个。 */
    @Test fun closingAndDetachingReleaseTheDecoder() {
        val model = ScreenModel(machineRecord(), unopenable)
        val output = FakeOutput()
        model.attachOutput(output)
        model.handle(ScreenEvent.Video(frame(keyframe = true)))
        model.close()
        assertTrue(output.decoders.single().released)
        model.handle(ScreenEvent.Video(frame(keyframe = true)))
        assertEquals(2, output.decoders.size)
        model.detachOutput()
        assertTrue(output.decoders.last().released)
        model.handle(ScreenEvent.Video(frame(keyframe = true)))
        assertEquals(2, output.decoders.size)
    }

    /** 换了 surface：旧的解码器停掉，之后的帧画到新的上。 */
    @Test fun attachingAnotherOutputReplacesTheDecoder() {
        val model = ScreenModel(machineRecord(), unopenable)
        val first = FakeOutput()
        model.attachOutput(first)
        model.handle(ScreenEvent.Video(frame(keyframe = true)))
        val second = FakeOutput()
        model.attachOutput(second)
        assertTrue(first.decoders.single().released)
        model.handle(ScreenEvent.Video(frame(keyframe = false)))
        assertEquals(1, second.decoders.single().frames.size)
    }
}

class ScreenTapTests {
    @get:Rule val main = MainDispatcherRule()

    @Test fun cursorStopsAtTheEdges() {
        assertEquals(0.0, ScreenModel.clamped(-0.2))
        assertEquals(1.0, ScreenModel.clamped(1.3))
        assertEquals(0.4, ScreenModel.clamped(0.4))
    }

    /** 第二次点在第一次附近、隔得不久时落到第一次的位置上，电脑才认成双击。 */
    @Test fun aQuickNearbySecondTapLandsOnTheFirst() {
        val model = ScreenModel(machineRecord(), unopenable)
        val start = TimeSource.Monotonic.markNow()
        assertEquals(0.5 to 0.5, model.tapTarget(0.5, 0.5, Offset(100f, 100f), start))
        assertEquals(0.5 to 0.5, model.tapTarget(0.51, 0.5, Offset(110f, 100f), start + 200.milliseconds))
        assertEquals(0.51 to 0.5, model.tapTarget(0.51, 0.5, Offset(110f, 100f), start + 2.seconds))
    }
}

class ScreenRetryTests {
    @get:Rule val main = MainDispatcherRule()

    private class Refused : Exception()

    /** 重连以后，上一次迟到的失败不把新的这次冲成失败。 */
    @Test fun aStaleFailureDoesNotOverwriteTheRetry() = runTest {
        val model = ScreenModel(machineRecord(), {
            // 被 retry 取消的那次不理会取消，照样报失败；新的那次一直连着不返回。
            try {
                delay(50)
            } catch (_: CancellationException) {
            }
            if (!currentCoroutineContext().isActive) throw Refused()
            delay(60.seconds)
            throw Refused()
        })
        model.start()
        model.retry()
        delay(200)
        assertEquals(ScreenModel.Phase.Connecting, model.phase)
        model.close()
    }

    /** 连不上时报连接的错误。 */
    @Test fun aFailedConnectionIsReported() = runTest {
        val model = ScreenModel(machineRecord(), { throw Refused() })
        model.start()
        assertTrue(eventually { model.phase is ScreenModel.Phase.Failed })
    }
}

class ScreenUnlockTests {
    @get:Rule val main = MainDispatcherRule()

    private val machine = machineRecord()
    private val passwords = MemoryUnlockPasswords()

    /** 身份验证一律通过。 */
    private val approve: UnlockAuthenticator = { cipher, _ -> cipher }

    private fun store(password: String, into: UnlockPasswordStore = passwords) =
        into.save(password, machine.id, into.cipherForSaving(machine.id))

    private fun model(store: UnlockPasswordStore = passwords, authenticate: UnlockAuthenticator? = approve): ScreenModel {
        val model = ScreenModel(machine, unopenable, store)
        model.authenticate = authenticate
        model.unlockWait = 50.milliseconds
        return model
    }

    private fun lock(locked: Boolean) = ScreenEvent.Message(ScreenHostMsg.Lock(locked))

    /** 锁着屏、存过密码时自动发出去；电脑解开以后收尾，不请用户输。 */
    @Test fun aStoredPasswordUnlocksAutomatically() = runTest {
        store("secret")
        val model = model()
        model.handle(lock(true))
        assertTrue(eventually { model.unlocking })
        model.handle(lock(false))
        assertTrue(!model.unlocking && !model.askingPassword && !model.locked)
    }

    /** 没存过时请用户输；勾了记住的，电脑解开以后过了身份验证才存下。 */
    @Test fun aTypedPasswordIsSavedOnlyAfterItWorks() = runTest {
        val model = model()
        model.handle(lock(true))
        assertTrue(model.askingPassword)
        model.unlock("secret", remember = true)
        assertTrue(model.unlocking && !passwords.hasPassword(machine.id))
        model.handle(lock(false))
        assertTrue(eventually { passwords.hasPassword(machine.id) })
        val cipher = assertNotNull(passwords.cipherForReading(machine.id))
        assertEquals("secret", passwords.password(machine.id, cipher))
    }

    /** 存的时候没过身份验证：不存。 */
    @Test fun aTypedPasswordIsNotSavedWithoutVerification() = runTest {
        val model = model(authenticate = { _, _ -> null })
        model.handle(lock(true))
        model.unlock("secret", remember = true)
        model.handle(lock(false))
        delay(100)
        assertFalse(passwords.hasPassword(machine.id))
    }

    /** 存着密码、但用户没过身份验证：不发，请用户输；存着的密码留着。 */
    @Test fun aDeclinedVerificationAsksForThePassword() = runTest {
        store("secret")
        val model = model(authenticate = { _, _ -> null })
        model.handle(lock(true))
        assertTrue(eventually { model.askingPassword })
        assertFalse(model.unlocking)
        assertNull(model.unlockError)
        assertTrue(passwords.hasPassword(machine.id))
    }

    /** 发了存着的密码、电脑一直没解开：删掉它、请用户重输，同一次锁屏里不再自动试。 */
    @Test fun aWrongStoredPasswordIsForgotten() = runTest {
        store("stale")
        val model = model()
        model.handle(lock(true))
        assertTrue(eventually { model.askingPassword })
        assertTrue(!model.unlocking && model.unlockError != null && !passwords.hasPassword(machine.id))
        model.askingPassword = false
        model.handle(lock(true))
        assertFalse(model.askingPassword)
    }

    /** 取密码（等身份验证）时电脑已经解开了：回来的密码不再发，过了等待时间也不当成密码不对。 */
    @Test fun aPasswordArrivingAfterTheUnlockIsDropped() = runTest {
        store("secret")
        val slow: UnlockAuthenticator = { cipher, _ ->
            delay(100)
            cipher
        }
        val model = model(authenticate = slow)
        model.handle(lock(true))
        model.handle(lock(false))
        delay(300)
        assertTrue(!model.unlocking && !model.askingPassword && passwords.hasPassword(machine.id))
    }

    /** 解锁途中断开不算密码不对：存着的密码留着，也不请用户输。 */
    @Test fun closingMidUnlockKeepsTheStoredPassword() = runTest {
        store("secret")
        val model = model()
        model.handle(lock(true))
        assertTrue(eventually { model.unlocking })
        model.close()
        delay(200)
        assertTrue(!model.unlocking && !model.askingPassword && passwords.hasPassword(machine.id))
    }

    /** 等用户输密码时断开：收起输密码的框，断开以后再交上来的密码不发，也不过一会儿报密码不对。 */
    @Test fun closingWhileAskingDropsThePasswordSheet() = runTest {
        store("stale")
        val model = model()
        model.handle(lock(true))
        assertTrue(eventually { model.askingPassword && model.unlockError != null })
        model.close()
        assertTrue(!model.askingPassword && model.unlockError == null)
        model.unlock("secret", remember = false)
        assertFalse(model.unlocking)
        delay(200)
        assertTrue(!model.askingPassword && model.unlockError == null)
    }

    /** 电脑敲不了这个密码：直接报出来，请用户在电脑前解锁。 */
    @Test fun unsupportedCharactersAreReported() = runTest {
        val model = model()
        model.handle(lock(true))
        model.unlock("пароль", remember = false)
        model.handle(ScreenEvent.Message(ScreenHostMsg.UnlockFailed("not on a US keyboard")))
        assertFalse(model.unlocking)
        assertTrue(model.askingPassword)
        assertEquals(
            "The password has characters a US keyboard can't type; unlock it at the computer",
            model.unlockError.english,
        )
    }

    /** 没有 BiometricPrompt（界面层没给验证）时存着的密码取不出来，请用户输。 */
    @Test fun withoutAnAuthenticatorTheUserTypesThePassword() = runTest {
        store("secret")
        val model = model(authenticate = null)
        model.handle(lock(true))
        assertTrue(eventually { model.askingPassword })
    }
}
