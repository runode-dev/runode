package dev.runode.mobile.connection

import dev.runode.mobile.protocol.CertificateFingerprint
import dev.runode.mobile.protocol.Frame
import dev.runode.mobile.protocol.FrameDecoder
import dev.runode.mobile.protocol.FrameKind
import dev.runode.mobile.protocol.GateMessage
import java.io.File
import java.util.UUID
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.channels.Channel
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject

/** 一个方向上的字节块队列：一边 [push]，另一边 [pop] 等着拿。关了以后已经推进来的照样能取，再推的丢掉。 */
class ChunkQueue {
    private val chunks = Channel<ByteArray>(Channel.UNLIMITED)
    private val count = AtomicInteger(0)

    fun push(data: ByteArray) {
        if (chunks.trySend(data).isSuccess) count.incrementAndGet()
    }

    /** 推进来、还没被取走的块数。 */
    val pending: Int get() = count.get()

    fun close() {
        chunks.close()
    }

    suspend fun pop(): ByteArray? = chunks.receiveCatching().getOrNull()?.also { count.decrementAndGet() }
}

/** 不碰网络的连接：宿主那头由测试脚本扮演。 */
class FakeTransport(override val remoteAddress: String? = "192.168.1.20") : FrameTransport {
    /** 宿主发给客户端的。 */
    val inbound = ChunkQueue()

    /** 客户端发给宿主的。 */
    val outbound = ChunkQueue()
    val exporterBytes = ByteArray(32) { 0x5A }

    override suspend fun receive(): ByteArray? = inbound.pop()

    override fun send(data: ByteArray) = outbound.push(data)

    override fun exporter(): ByteArray = exporterBytes

    override fun close() {
        inbound.close()
        outbound.close()
    }
}

/** 扮演宿主：读客户端发来的帧，回消息。 */
class FakeHost(val transport: FakeTransport) {
    private val decoder = FrameDecoder()

    suspend fun readFrame(): Frame? {
        while (true) {
            decoder.next()?.let { return it }
            decoder.append(transport.outbound.pop() ?: return null)
        }
    }

    /** 下一条控制消息的 JSON 对象。 */
    suspend fun readControl(): JsonObject? {
        while (true) {
            val frame = readFrame() ?: return null
            if (frame.kind == FrameKind.Control) return Json.parseToJsonElement(frame.payload.decodeToString()).jsonObject
        }
    }

    fun send(json: String) = transport.inbound.push(Frame(FrameKind.Control, 0u, json.encodeToByteArray()).encoded())

    fun send(message: GateMessage) = transport.inbound.push(Frame.control(message).encoded())

    fun send(frame: Frame) = transport.inbound.push(frame.encoded())

    fun challenge(version: UInt = 1u, node: String? = null) =
        send(GateMessage.Challenge(version, nonce, "Ethan 的 MacBook Pro", node))

    companion object {
        val nonce = ByteArray(32) { it.toByte() }

        const val welcome =
            """{"type":"welcome","protocol":4,"build":"0.1.0+abc","host_pid":1,"snapshot_format":1,"standalone":true,"handoff":1}"""
    }
}

/** 交出预先准备好的连接，记下被要了几次、要的是哪个地址。 */
class TransportSupply {
    private val transports = ArrayDeque<FakeTransport>()
    private val targets = mutableListOf<String>()

    fun add(transport: FakeTransport) = synchronized(this) { transports.addLast(transport) }

    val requestedTargets: List<String> get() = synchronized(this) { targets.toList() }

    val opener: TransportOpener = { target, _ ->
        val next = synchronized(this) {
            targets += target.label
            transports.removeFirstOrNull()
        }
        next ?: throw LinkFailure.ConnectionFailed("no more fake transports")
    }
}

fun sampleMachine(id: UUID = UUID.randomUUID()) = MachineRecord(
    id = id,
    name = "MacBook",
    hostName = "Ethan 的 MacBook Pro",
    fingerprint = CertificateFingerprint(ByteArray(32) { 1 })!!,
    port = 7866u,
    addresses = listOf("192.168.1.20", "fd7a::1"),
    lastAddress = null,
    deviceId = "00112233445566778899aabbccddeeff",
)

/** 宿主那边 `runode_protocol` 的样例，仓库根由构建经 `runode.repo` 传进来；找不到时测试失败，不跳过。 */
fun remoteFixture(name: String): File {
    val repository = System.getProperty("runode.repo") ?: error("runode.repo is not set; run the tests through Gradle")
    return File(repository, "crates/protocol/tests/fixtures/remote/$name").also { check(it.isFile) { "no sample $it" } }
}
