package dev.runode.mobile.features.model

import dev.runode.mobile.connection.FrameTransport
import dev.runode.mobile.connection.HostDiscovery
import dev.runode.mobile.connection.MachineRecord
import dev.runode.mobile.connection.MemoryDeviceKeyStore
import dev.runode.mobile.connection.ScreenConnection
import dev.runode.mobile.connection.TransportTarget
import dev.runode.mobile.protocol.CertificateFingerprint
import dev.runode.mobile.protocol.Frame
import dev.runode.mobile.protocol.FrameDecoder
import dev.runode.mobile.protocol.FrameKind
import dev.runode.mobile.protocol.GateMessage
import dev.runode.mobile.protocol.gateVersion
import kotlin.time.Duration
import kotlinx.coroutines.channels.Channel
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

/**
 * 扮演电脑上远程桌面的一头：过门禁（不验签名）、报一块 [width]×[height] 的屏幕，收到 `start` 回 `started`。手机发来的
 * 控制消息记在 [received]。
 */
class FakeScreenTransport(
    private val deviceId: String,
    private val width: Int = 1728,
    private val height: Int = 1117,
) : FrameTransport {
    private val inbound = Channel<ByteArray>(Channel.UNLIMITED)
    private val decoder = FrameDecoder()
    private val lock = Any()
    private val receivedList = mutableListOf<JsonObject>()

    override val remoteAddress: String = "192.168.1.20"

    init {
        push(Frame.control(GateMessage.Challenge(gateVersion, ByteArray(32) { it.toByte() }, "Ethan 的 MacBook Pro")))
    }

    val received: List<JsonObject> get() = synchronized(lock) { receivedList.toList() }

    /** 收到过的 `input` 里的输入，按类型名。 */
    fun inputs(type: String): List<JsonObject> = received
        .filter { it["type"]?.jsonPrimitive?.content == "input" }
        .mapNotNull { it["input"]?.jsonObject }
        .filter { it["type"]?.jsonPrimitive?.content == type }

    override suspend fun receive(): ByteArray? = inbound.receiveCatching().getOrNull()

    override fun send(data: ByteArray) {
        val messages = synchronized(lock) {
            decoder.append(data)
            buildList {
                while (true) {
                    val frame = decoder.next() ?: break
                    if (frame.kind != FrameKind.Control) continue
                    val message = Json.parseToJsonElement(frame.payload.decodeToString()).jsonObject
                    receivedList += message
                    add(message)
                }
            }
        }
        for (message in messages) {
            when (message["type"]?.jsonPrimitive?.content) {
                "remote_auth" -> push(Frame.control(GateMessage.Accepted(deviceId)))
                "screen_hello" -> push(
                    """{"type":"ready","displays":[{"id":1,"width":$width,"height":$height,"pixel_width":${width * 2},""" +
                        """"pixel_height":${height * 2}}],"screen_capture":true,"post_event":true}""",
                )
                "start" -> push("""{"type":"started","display":1,"width":$width,"height":$height}""")
            }
        }
    }

    /** 电脑那边发一条控制消息。 */
    fun push(json: String) = push(Frame(FrameKind.Control, 0u, json.encodeToByteArray()))

    private fun push(frame: Frame) {
        inbound.trySend(frame.encoded())
    }

    override fun exporter(): ByteArray = ByteArray(32)

    override fun close() {
        inbound.close()
    }
}

/** 连 [transport] 的 [ScreenOpener]：每次连都交出同一个。 */
fun fakeScreenOpener(transport: FakeScreenTransport): ScreenOpener {
    val keys = MemoryDeviceKeyStore()
    return { machine: MachineRecord ->
        if (keys.key(machine.id) == null) keys.generate(machine.id)
        ScreenConnection.open(machine, keys, NoDiscovery, open = { _, _ -> transport }, tunnel = null)
    }
}

private object NoDiscovery : HostDiscovery {
    override suspend fun locate(fingerprint: CertificateFingerprint, timeout: Duration): TransportTarget? = null
}
