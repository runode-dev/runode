package dev.runode.mobile.connection

import dev.runode.mobile.protocol.Frame
import dev.runode.mobile.protocol.FrameKind
import dev.runode.mobile.protocol.GateMessage
import dev.runode.mobile.protocol.ScreenClientMsg
import dev.runode.mobile.protocol.ScreenDisplay
import dev.runode.mobile.protocol.ScreenHostMsg
import dev.runode.mobile.protocol.VideoFrame
import dev.runode.mobile.protocol.screenVersion
import java.nio.ByteBuffer
import java.nio.ByteOrder
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertTrue
import kotlinx.coroutines.async
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Test

/** 远程桌面的连接：门禁后先发 `screen_hello`，等电脑的 `ready`，之后收消息和画面、发消息。 */
class ScreenConnectionTests {
    private val machine = sampleMachine()
    private val keys = MemoryDeviceKeyStore().also { it.generate(machine.id) }
    private val supply = TransportSupply()

    /** 扮演电脑走完门禁，收下 `screen_hello` 后回 [reply]。 */
    private suspend fun admit(host: FakeHost, reply: String = ready) {
        host.challenge()
        assertNotNull(host.readControl())
        host.send(GateMessage.Accepted(machine.deviceId))
        val hello = assertNotNull(host.readControl())
        assertEquals("screen_hello", hello["type"]?.jsonPrimitive?.content)
        assertEquals(screenVersion.toInt(), hello["version"]?.jsonPrimitive?.int)
        host.send(reply)
    }

    private suspend fun open() = ScreenConnection.open(machine, keys, NoDiscovery, open = supply.opener)

    @Test
    fun handshakesThenStreamsMessagesAndVideo() = test { scope ->
        val transport = FakeTransport()
        supply.add(transport)
        val host = FakeHost(transport)
        val opened = scope.async { open() }
        admit(host)
        val connection = opened.await()
        assertEquals(listOf(ScreenDisplay(1u, 1728u, 1117u, 3456u, 2234u)), connection.displays)
        assertTrue(connection.screenCapture)
        assertFalse(connection.postEvent)

        connection.send(ScreenClientMsg.Start(display = null, width = 1280u, height = 720u, fps = 60u, bitrate = 8_000_000u))
        assertEquals("start", host.readControl()?.get("type")?.jsonPrimitive?.content)
        host.send("""{"type":"started","display":1,"width":1280,"height":720}""")
        val video = VideoFrame(
            data = byteArrayOf(0, 0, 0, 1, 0x26),
            keyframe = true,
            parameterSets = listOf(byteArrayOf(0x40)),
            ptsMicros = 7,
        )
        val payload = byteArrayOf(1) + ByteBuffer.allocate(8).order(ByteOrder.LITTLE_ENDIAN).putLong(7).array() +
            byteArrayOf(1, 1, 0, 0, 0, 0x40, 0, 0, 0, 1, 0x26)
        host.send(Frame(FrameKind.Video, 0u, payload))

        val received = mutableListOf<ScreenEvent>()
        val ended = runCatching {
            connection.events().collect { event ->
                received += event
                // 收齐两个以后电脑断开：流以错误结束。
                if (received.size == 2) transport.close()
            }
        }
        assertEquals(
            listOf(ScreenEvent.Message(ScreenHostMsg.Started(1u, 1280u, 720u)), ScreenEvent.Video(video)),
            received,
        )
        assertIs<LinkFailure>(ended.exceptionOrNull())
    }

    @Test
    fun aDifferentVersionIsIncompatible() = test { scope ->
        val transport = FakeTransport()
        supply.add(transport)
        val host = FakeHost(transport)
        val opened = scope.async { runCatching { open() } }
        admit(host, reply = """{"type":"incompatible","version":9}""")
        assertIs<LinkFailure.Incompatible>(opened.await().exceptionOrNull())
    }

    private companion object {
        const val ready =
            """{"type":"ready","displays":[{"id":1,"width":1728,"height":1117,"pixel_width":3456,"pixel_height":2234}],"screen_capture":true,"post_event":false}"""
    }
}
