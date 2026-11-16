package dev.runode.mobile.connection

import dev.runode.mobile.protocol.FrameKind
import dev.runode.mobile.protocol.GateMessage
import dev.runode.mobile.protocol.maxMediaBytes
import dev.runode.mobile.protocol.maxMediaFrame
import dev.runode.mobile.protocol.mediaVersion
import java.io.ByteArrayInputStream
import java.io.EOFException
import java.nio.file.Files
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlinx.coroutines.async
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Test

/** 交给电脑图片和视频的连接：门禁后先发 `media_hello`，等电脑的 `ready`；一个文件先报长度，再分块发，等电脑回路径。 */
class MediaConnectionTests {
    private val machine = sampleMachine()
    private val keys = MemoryDeviceKeyStore().also { it.generate(machine.id) }
    private val supply = TransportSupply()

    /** 扮演电脑走完门禁，收下 `media_hello` 后回 [reply]。 */
    private suspend fun admit(host: FakeHost, reply: String = """{"type":"ready"}""") {
        host.challenge()
        assertNotNull(host.readControl())
        host.send(GateMessage.Accepted(machine.deviceId))
        val hello = assertNotNull(host.readControl())
        assertEquals("media_hello", hello["type"]?.jsonPrimitive?.content)
        assertEquals(mediaVersion.toInt(), hello["version"]?.jsonPrimitive?.int)
        host.send(reply)
    }

    private suspend fun open() = MediaConnection.open(machine, keys, NoDiscovery, open = supply.opener)

    /** 收下一个文件：先是带长度的 `media`，再是一块块 `Media` 帧，拼起来返回。 */
    private suspend fun FakeHost.readMedia(): ByteArray {
        val header = assertNotNull(readControl())
        assertEquals("media", header["type"]?.jsonPrimitive?.content)
        val length = assertNotNull(header["len"]?.jsonPrimitive?.int)
        val data = ByteArray(length)
        var received = 0
        while (received < length) {
            val frame = assertNotNull(readFrame())
            assertEquals(FrameKind.Media, frame.kind)
            assertEquals(0u, frame.channel)
            assertTrue(frame.payload.size <= MediaConnection.chunkBytes)
            frame.payload.copyInto(data, received)
            received += frame.payload.size
        }
        assertEquals(length, received)
        return data
    }

    @Test
    fun theChunksFitInAFrame() {
        assertTrue(MediaConnection.chunkBytes <= maxMediaFrame)
    }

    @Test
    fun sendsTheBytesInChunksAndReturnsThePath() = test { scope ->
        val transport = FakeTransport()
        supply.add(transport)
        val host = FakeHost(transport)
        val opened = scope.async { open() }
        admit(host)
        val connection = opened.await()

        // 比一块多一点，要分成两帧。
        val image = ByteArray(MediaConnection.chunkBytes + 10) { it.toByte() }
        val sending = scope.async { connection.send(image) }
        assertContentEquals(image, host.readMedia())
        host.send("""{"type":"saved","path":"/tmp/截图 1.png"}""")
        assertEquals("/tmp/截图 1.png", sending.await())

        // 同一条连接接着发下一个。
        val second = scope.async { connection.send(byteArrayOf(1, 2, 3)) }
        assertContentEquals(byteArrayOf(1, 2, 3), host.readMedia())
        host.send("""{"type":"saved","path":"/tmp/b.png"}""")
        assertEquals("/tmp/b.png", second.await())
        connection.close()
    }

    @Test
    fun streamsAFileAndReportsProgress() = test { scope ->
        val transport = FakeTransport()
        supply.add(transport)
        val host = FakeHost(transport)
        val opened = scope.async { open() }
        admit(host)
        val connection = opened.await()

        val video = ByteArray(MediaConnection.chunkBytes * (MediaConnection.drainEvery + 1) + 7) { (it * 31).toByte() }
        val file = Files.createTempFile("media", ".mp4").toFile()
        try {
            file.writeBytes(video)
            val progress = mutableListOf<Pair<Long, Long>>()
            val sending = scope.async { connection.send(file) { sent, total -> progress += sent to total } }
            assertContentEquals(video, host.readMedia())
            host.send("""{"type":"saved","path":"/tmp/clip.mp4"}""")
            assertEquals("/tmp/clip.mp4", sending.await())
            val total = video.size.toLong()
            val chunk = MediaConnection.chunkBytes.toLong()
            val expected = (1..MediaConnection.drainEvery + 2).map { minOf(it * chunk, total) to total }
            assertEquals(expected, progress)
        } finally {
            file.delete()
        }
    }

    @Test
    fun aStreamThatEndsEarlyClosesTheConnection() = test { scope ->
        val transport = FakeTransport()
        supply.add(transport)
        val host = FakeHost(transport)
        val opened = scope.async { open() }
        admit(host)
        val connection = opened.await()

        val result = scope.async { runCatching { connection.send(ByteArrayInputStream(byteArrayOf(1, 2)), 5) } }
        assertIs<EOFException>(result.await().exceptionOrNull())
        // 已经报过长度，帧对不齐了：连接关掉。
        assertNotNull(host.readControl())
        while (true) host.readFrame() ?: break
        assertNull(transport.inbound.pop())
    }

    @Test
    fun aFileLargerThanTheLimitIsRefusedBeforeSending() = test { scope ->
        val transport = FakeTransport()
        supply.add(transport)
        val host = FakeHost(transport)
        val opened = scope.async { open() }
        admit(host)
        val connection = opened.await()
        assertFailsWith<IllegalArgumentException> {
            connection.send(ByteArrayInputStream(ByteArray(0)), maxMediaBytes.toLong() + 1)
        }
        assertEquals(0, transport.outbound.pending)
    }

    @Test
    fun aFailureCarriesTheReason() = test { scope ->
        val transport = FakeTransport()
        supply.add(transport)
        val host = FakeHost(transport)
        val opened = scope.async { open() }
        admit(host)
        val connection = opened.await()
        val sending = scope.async { runCatching { connection.send(byteArrayOf(9)) } }
        host.readMedia()
        host.send("""{"type":"failed","reason":"disk full"}""")
        assertEquals(MediaFailure("disk full"), sending.await().exceptionOrNull())
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
