package dev.runode.mobile.connection

import java.io.IOException
import java.net.Socket
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong
import kotlin.coroutines.cancellation.CancellationException
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNull
import kotlin.test.fail
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.job
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Test

/** 原生库的拨号取消了也不停：拨号要在取消时马上返回，晚连上的交给 `discard` 关掉。 */
class AbandoningOnCancelTests {
    @Test
    fun cancellingDoesNotWaitForTheWork() = runBlocking {
        val started = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()
        val discarded = CompletableDeferred<Int>()
        val dial = async(Dispatchers.Default) {
            abandoningOnCancel(
                work = {
                    started.complete(Unit)
                    release.await()
                    7
                },
                discard = { discarded.complete(it) },
            )
        }
        started.await()
        dial.cancel()
        assertFailsWith<CancellationException> { withTimeout(5.seconds) { dial.await() } }
        release.complete(Unit)
        assertEquals(7, withTimeout(5.seconds) { discarded.await() })
    }

    @Test
    fun cancelledBeforeStartingThrows() = runBlocking {
        val ran = AtomicBoolean(false)
        val dial = async {
            coroutineContext.job.cancel()
            abandoningOnCancel(work = { ran.set(true) }, discard = {})
        }
        assertFailsWith<CancellationException> { dial.await() }
        assertFalse(ran.get())
    }

    @Test
    fun passesTheResultThrough() = runBlocking {
        assertEquals(3, abandoningOnCancel(work = { 3 }, discard = { fail("nothing to discard") }))
    }

    @Test
    fun passesTheFailureThrough() = runBlocking {
        val error = assertFailsWith<IOException> {
            abandoningOnCancel<Int>(work = { throw IOException("no route") }, discard = {})
        }
        assertEquals("no route", error.message)
    }
}

/** [IrohDialer] 接原生库、本机代理和 TLS 的那几步；原生库和 TLS 都换成假的，代理是真的。 */
class IrohDialerTests {
    private val iroh = FakeIroh()
    private val fingerprint = sampleMachine().fingerprint

    /** TLS 换成直接连代理的 TCP：代理两头搬的字节原样过来。 */
    private val plainOpener: TransportOpener = { target, _ -> PlainTransport(Socket(target.host, target.port.toInt())) }

    private fun dialer(open: TransportOpener = plainOpener) = IrohDialer(iroh, open)

    /** 锁定代理两个方向原样搬字节，iroh 那边说完时 TLS 那边读到结尾，关掉连接时流也关掉。 */
    @Test
    fun carriesBytesBothWaysOverTheStream() = runBlocking {
        val transport = withTimeout(5.seconds) { dialer().open(node, fingerprint) }
        assertNull(transport.remoteAddress, "the proxy's address is not the computer's")
        transport.send("hello".encodeToByteArray())
        assertContentEquals("hello".encodeToByteArray(), iroh.written(5))
        iroh.reply("back".encodeToByteArray())
        assertContentEquals("back".encodeToByteArray(), withTimeout(5.seconds) { transport.receive() })
        iroh.finish()
        assertNull(withTimeout(5.seconds) { transport.receive() })
        transport.close()
        iroh.awaitClosed(FakeIroh.firstStream)
    }

    /** 锁定几次拨号共用一个端点。 */
    @Test
    fun sharesOneEndpoint() = runBlocking {
        val dialer = dialer()
        dialer.open(node, fingerprint).close()
        dialer.open(node, fingerprint).close()
        assertEquals(1, iroh.binds.get())
    }

    /** 锁定端点建不起来时报连不上，下次拨号重建。 */
    @Test
    fun aFailedBindIsRetried() = runBlocking {
        val dialer = dialer()
        iroh.bindFailure = IOException("no network")
        assertIs<LinkFailure.ConnectionFailed>(runCatching { dialer.open(node, fingerprint) }.exceptionOrNull())
        iroh.bindFailure = null
        dialer.open(node, fingerprint).close()
        assertEquals(2, iroh.binds.get())
    }

    /** 锁定电脑报的公钥不对时当成协议错，别的拨号失败当成连不上。 */
    @Test
    fun mapsDialFailures() = runBlocking {
        val dialer = dialer()
        iroh.connectFailure = IllegalArgumentException("not a key")
        assertIs<LinkFailure.ProtocolViolation>(runCatching { dialer.open(node, fingerprint) }.exceptionOrNull())
        iroh.connectFailure = IOException("timed out")
        assertEquals(
            LinkFailure.ConnectionFailed("timed out"),
            runCatching { dialer.open(node, fingerprint) }.exceptionOrNull(),
        )
    }

    /** 锁定 TLS 没连上时关掉已经开出的流，不留着没人读的连接。 */
    @Test
    fun closesTheStreamWhenTlsFails() = runBlocking {
        val dialer = dialer(open = { _, _ -> throw LinkFailure.FingerprintMismatch })
        assertEquals(LinkFailure.FingerprintMismatch, runCatching { dialer.open(node, fingerprint) }.exceptionOrNull())
        iroh.awaitClosed(FakeIroh.firstStream)
    }

    private companion object {
        const val node = "ae58ff8833241ac82d6ff7611046ed67b5072d142c588d0063e942d9a75502b6"
    }
}

/** 不加载原生库的 iroh：手机写进流的进 [written]，[reply] 的由流读出来。 */
private class FakeIroh : IrohNative {
    val binds = AtomicInteger()

    @Volatile
    var bindFailure: IOException? = null

    @Volatile
    var connectFailure: Exception? = null

    private val nextStream = AtomicLong(firstStream - 1)
    private val incoming = LinkedBlockingQueue<ByteArray>()
    private val outgoing = LinkedBlockingQueue<ByteArray>()
    private val closed = ConcurrentHashMap.newKeySet<Long>()

    fun reply(data: ByteArray) = incoming.put(data)

    /** 对面说完：空块代表结尾。 */
    fun finish() = incoming.put(ByteArray(0))

    /** 等手机写够 [count] 字节，返回它们。 */
    fun written(count: Int): ByteArray {
        var got = ByteArray(0)
        while (got.size < count) got += outgoing.poll(5, TimeUnit.SECONDS) ?: fail("nothing written")
        return got
    }

    suspend fun awaitClosed(stream: Long) = withTimeout(5.seconds) {
        while (stream !in closed) delay(10)
    }

    override fun bind(): Long {
        binds.incrementAndGet()
        bindFailure?.let { throw it }
        return 1
    }

    override fun connect(endpoint: Long, node: String, alpn: ByteArray, timeoutMillis: Long): Long {
        connectFailure?.let { throw it }
        return nextStream.incrementAndGet()
    }

    override fun read(stream: Long, buffer: ByteArray, offset: Int, length: Int): Int {
        while (true) {
            if (stream in closed) throw IOException("already closed")
            val chunk = incoming.poll(10, TimeUnit.MILLISECONDS) ?: continue
            if (chunk.isEmpty()) return -1
            check(chunk.size <= length)
            chunk.copyInto(buffer, offset)
            return chunk.size
        }
    }

    override fun write(stream: Long, buffer: ByteArray, offset: Int, length: Int) {
        if (stream in closed) throw IOException("already closed")
        outgoing.put(buffer.copyOfRange(offset, offset + length))
    }

    override fun close(stream: Long) {
        closed += stream
    }

    override fun closeEndpoint(endpoint: Long) {}

    companion object {
        const val firstStream = 100L
    }
}

/** 不加密的 TCP 连接，替 [TlsChannel] 连本机代理。 */
private class PlainTransport(private val socket: Socket) : FrameTransport {
    override suspend fun receive(): ByteArray? = blocking(socket::close) {
        val buffer = ByteArray(1024)
        val count = socket.getInputStream().read(buffer)
        if (count < 0) null else buffer.copyOf(count)
    }

    override fun send(data: ByteArray) = socket.getOutputStream().write(data)

    override fun exporter(): ByteArray = ByteArray(32)

    override val remoteAddress: String? = socket.inetAddress.hostAddress

    override fun close() = socket.close()
}
