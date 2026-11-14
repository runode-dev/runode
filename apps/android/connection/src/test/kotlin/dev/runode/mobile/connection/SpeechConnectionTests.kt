package dev.runode.mobile.connection

import dev.runode.mobile.protocol.FrameKind
import dev.runode.mobile.protocol.GateMessage
import dev.runode.mobile.protocol.speechAudioPayload
import dev.runode.mobile.protocol.speechVersion
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertTrue
import kotlinx.coroutines.async
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Test

/** 交给电脑识别的连接：门禁后先发 `speech_hello`，等电脑的 `ready`，录音按 16 位 PCM 发，`finish` 后等文字。 */
class SpeechConnectionTests {
    private val machine = sampleMachine()
    private val keys = MemoryDeviceKeyStore().also { it.generate(machine.id) }
    private val supply = TransportSupply()

    /** 扮演电脑走完门禁，收下 `speech_hello` 后回 [reply]。 */
    private suspend fun admit(host: FakeHost, reply: String = """{"type":"ready","installed":true}""") {
        host.challenge()
        assertNotNull(host.readControl())
        host.send(GateMessage.Accepted(machine.deviceId))
        val hello = assertNotNull(host.readControl())
        assertEquals("speech_hello", hello["type"]?.jsonPrimitive?.content)
        assertEquals(speechVersion.toInt(), hello["version"]?.jsonPrimitive?.int)
        host.send(reply)
    }

    private suspend fun open() = SpeechConnection.open(machine, keys, NoDiscovery, open = supply.opener)

    @Test
    fun sendsAudioThenReturnsTheText() = test { scope ->
        val transport = FakeTransport()
        supply.add(transport)
        val host = FakeHost(transport)
        val opened = scope.async { open() }
        admit(host)
        val connection = opened.await()
        assertTrue(connection.installed)

        connection.send(floatArrayOf(0f, 1f, -1f, 2f))
        val audio = assertNotNull(host.readFrame())
        assertEquals(FrameKind.Audio, audio.kind)
        assertEquals(0u, audio.channel)
        assertContentEquals(speechAudioPayload(floatArrayOf(0f, 1f, -1f, 2f)), audio.payload)
        val finishing = scope.async { connection.finish() }
        assertEquals("finish", host.readControl()?.get("type")?.jsonPrimitive?.content)
        host.send("""{"type":"text","text":"你好"}""")
        assertEquals("你好", finishing.await())
    }

    @Test
    fun aFailureCarriesTheReason() = test { scope ->
        val transport = FakeTransport()
        supply.add(transport)
        val host = FakeHost(transport)
        val opened = scope.async { open() }
        admit(host, reply = """{"type":"ready","installed":false}""")
        val connection = opened.await()
        assertFalse(connection.installed)
        val finishing = scope.async { runCatching { connection.finish() } }
        host.readControl()
        host.send("""{"type":"failed","reason":"no model"}""")
        assertEquals(SpeechFailure("no model"), finishing.await().exceptionOrNull())
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
}
