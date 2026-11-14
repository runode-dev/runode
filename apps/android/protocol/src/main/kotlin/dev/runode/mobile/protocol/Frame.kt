package dev.runode.mobile.protocol

import kotlinx.serialization.DeserializationStrategy
import kotlinx.serialization.SerializationStrategy

/**
 * 帧的类型，和宿主的 `runode_protocol::FrameKind` 一一对应。帧头格式是冻结的：
 * `u32 LE 载荷长度 | u8 类型 | u32 LE 通道 | 载荷`。
 */
enum class FrameKind(val rawValue: UByte) {
    /** 宿主发来的 PTY 输出，原样的字节。 */
    Output(0u),

    /** 发给宿主、要写进 PTY 的输入。 */
    Input(1u),

    /** 控制消息，JSON。 */
    Control(2u),

    /** 快照或 VT 重放的一段，以 [HostMsg.SnapshotEnd] 结束。 */
    Snapshot(3u),

    /** 远程桌面编好的一帧画面，载荷见 [VideoFrame.Companion.invoke]。 */
    Video(4u),

    /** 交给电脑识别的一段录音，载荷见 [speechSampleRate]。 */
    Audio(5u),
    ;

    companion object {
        /** 不认识的类型为空。 */
        fun fromRaw(rawValue: UByte): FrameKind? = entries.firstOrNull { it.rawValue == rawValue }
    }
}

class Frame(
    val kind: FrameKind,
    /** 会话在这条连接上的通道；控制消息一律是 0。 */
    val channel: UInt,
    val payload: ByteArray,
) {
    /** 帧头加载荷。载荷超过 [maxPayload] 时抛 [FrameError.TooLong]，什么都不写。 */
    fun encoded(): ByteArray {
        if (payload.size.toLong() > maxPayload.toLong()) throw FrameError.TooLong(payload.size.toULong())
        val out = ByteArray(headerLength + payload.size)
        writeU32(out, 0, payload.size.toUInt())
        out[4] = kind.rawValue.toByte()
        writeU32(out, 5, channel)
        payload.copyInto(out, headerLength)
        return out
    }

    /** 把控制帧的载荷解成消息。 */
    fun <M> message(deserializer: DeserializationStrategy<M>): M =
        protocolJson.decodeFromString(deserializer, payload.decodeToString(throwOnInvalidSequence = true))

    override fun equals(other: Any?): Boolean =
        other is Frame && kind == other.kind && channel == other.channel && payload.contentEquals(other.payload)

    override fun hashCode(): Int = (kind.hashCode() * 31 + channel.hashCode()) * 31 + payload.contentHashCode()

    override fun toString(): String = "Frame(kind=$kind, channel=$channel, payload=${payload.size} bytes)"

    companion object {
        /** 帧头的字节数：载荷长度、类型、通道。 */
        const val headerLength: Int = 9

        /** 载荷上限，和宿主的 `MAX_PAYLOAD` 一样；再长就是对面出了错。 */
        const val maxPayload: UInt = 67_108_864u

        /** 手机发的控制帧载荷的上限，和电脑那边远程访问的 `MAX_CONTROL_PAYLOAD` 一样；超了电脑断开整条连接。 */
        const val maxControlPayload: Int = 1 shl 20

        /** 门禁阶段单帧载荷的上限，宿主的 `remote::GATE_MAX_PAYLOAD`。 */
        const val gateMaxPayload: UInt = 16_384u

        /** 一条控制消息，走通道 0。 */
        fun <T> control(serializer: SerializationStrategy<T>, message: T): Frame =
            Frame(FrameKind.Control, 0u, protocolJson.encodeToString(serializer, message).encodeToByteArray())

        fun control(message: ClientMsg): Frame = control(ClientMsg.serializer(), message)

        fun control(message: GateMessage): Frame = control(GateMessage.serializer(), message)

        fun control(message: ScreenClientMsg): Frame = control(ScreenClientMsg.serializer(), message)

        fun control(message: SpeechClientMsg): Frame = control(SpeechClientMsg.serializer(), message)

        private fun writeU32(out: ByteArray, at: Int, value: UInt) {
            for (i in 0 until 4) out[at + i] = (value shr (8 * i)).toByte()
        }
    }
}

/** 读帧失败的原因。出现任何一种时连接上的数据都已经对不齐了，只能断开。 */
sealed class FrameError(message: String) : Exception(message) {
    /** 载荷超过上限，带着对面声明的长度。 */
    data class TooLong(val length: ULong) : FrameError("frame payload of $length bytes is too long")

    /** 不认识的帧类型，对面多半是别的协议版本。 */
    data class UnknownKind(val kind: UByte) : FrameError("unknown frame kind $kind")

    /** 连接在一帧中间断了。 */
    data object Truncated : FrameError("the connection closed in the middle of a frame")
}

/**
 * 从字节流里切帧：网络上收到多少就 [append] 多少，再反复 [next] 直到返回空。
 * 先查长度再查类型，载荷按实际到达的字节攒，不照着声明的长度预先分配。
 */
class FrameDecoder(
    /** 单帧载荷的上限：门禁阶段是 [Frame.gateMaxPayload]，之后是 [Frame.maxPayload]。 */
    var maxPayload: UInt = Frame.maxPayload,
) {
    private var buffer = ByteArray(0)

    /** [buffer] 里有效的字节到这里为止。 */
    private var end = 0

    /** [buffer] 里已经切走的字节数，攒多了再一起挪掉，免得每帧都搬一次。 */
    private var start = 0

    /** 还没切成帧的字节数。 */
    val bufferedCount: Int get() = end - start

    fun append(data: ByteArray, offset: Int = 0, length: Int = data.size - offset) {
        if (end + length > buffer.size) {
            buffer = buffer.copyOf(maxOf(end + length, buffer.size * 2))
        }
        data.copyInto(buffer, end, offset, offset + length)
        end += length
    }

    /** 切出下一帧；数据还不够一帧时返回空。 */
    fun next(): Frame? {
        val available = end - start
        if (available < Frame.headerLength) return null
        val length = readU32(start)
        if (length > maxPayload) throw FrameError.TooLong(length.toULong())
        val kindByte = buffer[start + 4].toUByte()
        val kind = FrameKind.fromRaw(kindByte) ?: throw FrameError.UnknownKind(kindByte)
        val channel = readU32(start + 5)
        val total = Frame.headerLength + length.toInt()
        if (available < total) return null
        val payloadStart = start + Frame.headerLength
        val payload = buffer.copyOfRange(payloadStart, payloadStart + length.toInt())
        start += total
        compactIfNeeded()
        return Frame(kind, channel, payload)
    }

    /** 连接正常关闭时调：两帧之间关闭没事，帧读到一半就关了算截断，抛 [FrameError.Truncated]。 */
    fun finish() {
        if (bufferedCount > 0) throw FrameError.Truncated
    }

    private fun readU32(at: Int): UInt =
        (0 until 4).fold(0u) { value, i -> value or ((buffer[at + i].toUInt() and 0xFFu) shl (8 * i)) }

    private fun compactIfNeeded() {
        if (start == end) {
            start = 0
            end = 0
        } else if (start > 64 shl 10 && start * 2 > end) {
            buffer.copyInto(buffer, 0, start, end)
            end -= start
            start = 0
        }
    }
}
