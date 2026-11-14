package dev.runode.mobile.protocol

import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlinx.serialization.json.jsonObject
import org.junit.Test

/** 宿主 `runode_protocol::screen` 照规格写出的远程桌面样例，两边的测试读同一批文件。 */
class ScreenTests {
    @Test
    fun clientMessagesMatchTheSamples() {
        val cases = listOf(
            "screen_hello.json" to ScreenClientMsg.ScreenHello(version = 2u),
            "start.json" to ScreenClientMsg.Start(display = 2u, width = 2556u, height = 1179u, fps = 60u, bitrate = 8_000_000u),
            "keyframe.json" to ScreenClientMsg.Keyframe,
            "bitrate.json" to ScreenClientMsg.Bitrate(2_000_000u),
            "input_mouse_move.json" to ScreenClientMsg.Input(ScreenInput.MouseMove(x = 0.5, y = 0.25)),
            "input_mouse_button.json" to ScreenClientMsg.Input(
                ScreenInput.MouseButton(ScreenMouseButton.Right, down = true, x = 0.75, y = 1.0, mods = ScreenMods(cmd = true)),
            ),
            "input_scroll.json" to ScreenClientMsg.Input(ScreenInput.Scroll(dx = 0, dy = -120)),
            "input_key.json" to ScreenClientMsg.Input(ScreenInput.Key("ArrowUp", mods = ScreenMods(shift = true))),
            "input_text.json" to ScreenClientMsg.Input(ScreenInput.Text("你好 ok")),
            "unlock.json" to ScreenClientMsg.Unlock(password = "p@ss word"),
        )
        for ((name, message) in cases) {
            val encoded = encodeToJson(ScreenClientMsg.serializer(), message)
            assertTrue(sameJson(encoded, Samples.json("screen", name)), "$name: $encoded")
        }
    }

    @Test
    fun hostMessagesMatchTheSamples() {
        fun read(name: String) = decode(ScreenHostMsg.serializer(), Samples.json("screen", name))
        val displays = listOf(
            ScreenDisplay(id = 1u, width = 1728u, height = 1117u, pixelWidth = 3456u, pixelHeight = 2234u),
            ScreenDisplay(id = 2u, width = 2560u, height = 1440u, pixelWidth = 2560u, pixelHeight = 1440u),
        )
        assertEquals(ScreenHostMsg.Ready(displays, screenCapture = true, postEvent = false), read("ready.json"))
        assertEquals(ScreenHostMsg.Incompatible(version = 2u), read("incompatible.json"))
        assertEquals(ScreenHostMsg.Started(display = 2u, width = 2556u, height = 1652u), read("started.json"))
        assertEquals(ScreenHostMsg.Stopped(reason = "display unplugged", cause = null), read("stopped.json"))
        assertEquals(
            ScreenHostMsg.Stopped(reason = "no hardware encoder on the display's GPU", cause = ScreenStopCause.NoEncoder),
            read("stopped_no_encoder.json"),
        )
        assertEquals(
            ScreenHostMsg.Stopped(
                reason = "nobody is signed in on this computer's screen",
                cause = ScreenStopCause.SignedOut,
            ),
            read("stopped_signed_out.json"),
        )
        assertEquals(
            ScreenHostMsg.Stopped(
                reason = "nobody is signed in to a desktop on this screen",
                cause = ScreenStopCause.NoDesktop,
            ),
            read("stopped_no_desktop.json"),
        )
        assertEquals(
            ScreenHostMsg.Stopped(
                reason = "remote desktop was not allowed on this computer's screen: cancelled",
                cause = ScreenStopCause.Declined,
            ),
            read("stopped_declined.json"),
        )
        assertEquals(
            ScreenHostMsg.Stopped(
                reason = "cannot create a hardware encoder: hevc (-12902)",
                cause = ScreenStopCause.NoEncoder,
            ),
            read("stopped_cause_no_encoder.json"),
        )
        // 更新的电脑分出来的、这边不认得的类退回看标志。
        assertEquals(
            ScreenHostMsg.Stopped(reason = "the display is asleep", cause = ScreenStopCause.SignedOut),
            read("stopped_future_cause.json"),
        )
        assertEquals(ScreenHostMsg.Lock(locked = true), read("lock.json"))
        assertEquals(ScreenHostMsg.Permissions(screenCapture = true, postEvent = true), read("permissions.json"))
        assertEquals(
            ScreenHostMsg.UnlockFailed(reason = "the password has keys a US keyboard cannot type"),
            read("unlock_failed.json"),
        )
    }

    @Test
    fun videoPayloadsMatchTheSamples() {
        val keyframe = assertNotNull(VideoFrame(Samples.hex("screen", "video_keyframe.hex")))
        assertEquals(
            VideoFrame(
                data = byteArrayOf(0, 0, 0, 2, 0x26, 1),
                keyframe = true,
                parameterSets = listOf(byteArrayOf(0x40, 1), byteArrayOf(0x42), byteArrayOf(0x44, 1, 2)),
                ptsMicros = -5,
            ),
            keyframe,
        )
        val delta = assertNotNull(VideoFrame(Samples.hex("screen", "video_delta.hex")))
        assertEquals(
            VideoFrame(data = byteArrayOf(0, 0, 0, 2, 2, 0x0a), keyframe = false, parameterSets = emptyList(), ptsMicros = 1000),
            delta,
        )
        val h264 = assertNotNull(VideoFrame(Samples.hex("screen", "video_h264.hex")))
        assertEquals(
            VideoFrame(
                data = byteArrayOf(0, 0, 0, 2, 0x65, 0x88.toByte()),
                keyframe = true,
                parameterSets = listOf(byteArrayOf(0x67, 0x64), byteArrayOf(0x68, 0xee.toByte())),
                ptsMicros = 7,
                codec = Codec.H264,
            ),
            h264,
        )
    }

    /** 列了编码时照想要的先后写上；没列时不写，电脑当成只有 HEVC。 */
    @Test
    fun startListsTheDecodableCodecs() {
        val start = ScreenClientMsg.Start(
            display = null,
            width = 1920u,
            height = 1080u,
            fps = 60u,
            bitrate = 6_000_000u,
            codecs = listOf(Codec.Hevc, Codec.H264),
        )
        val sample = Samples.json("screen", "start_codecs.json").jsonObject
        val encoded = encodeToJson(ScreenClientMsg.serializer(), start).jsonObject
        assertEquals(sample["codecs"], encoded["codecs"])
        assertNull(encodeToJson(ScreenClientMsg.serializer(), start.copy(codecs = emptyList())).jsonObject["codecs"])
    }

    /** 参数集的长度越过载荷结尾时整帧不认，不读出界。 */
    @Test
    fun truncatedParameterSetsAreRejected() {
        val payload = byteArrayOf(1) + ByteArray(8) + byteArrayOf(1) + byteArrayOf(9, 0, 0, 0, 1, 2)
        assertNull(VideoFrame(payload))
    }
}
