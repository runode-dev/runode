package dev.runode.mobile.protocol

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement

/** 交给电脑识别的这套消息和录音格式的版本，宿主的 `protocol::speech::SPEECH_VERSION`。 */
const val speechVersion: UInt = 1u

/** [FrameKind.Audio] 的采样率：载荷是这个采样率的单声道 16 位小端 PCM。 */
const val speechSampleRate: Int = 16_000

/** 一段录音最长这么多秒，超了电脑回 [SpeechHostMsg.Failed]。 */
const val maxSpeechSeconds: Int = 120

/** 一段录音（[speechSampleRate] 的单声道，-1 到 1，超出的截到两头）编成 [FrameKind.Audio] 的载荷。 */
fun speechAudioPayload(samples: FloatArray): ByteArray {
    val payload = ByteArray(samples.size * 2)
    for ((i, sample) in samples.withIndex()) {
        val value = (sample.coerceIn(-1f, 1f) * Short.MAX_VALUE).toInt()
        payload[2 * i] = value.toByte()
        payload[2 * i + 1] = (value shr 8).toByte()
    }
    return payload
}

/** 手机发给电脑的识别消息。 */
@Serializable(with = SpeechClientMsgSerializer::class)
sealed class SpeechClientMsg {
    /** 过了门禁后的第一条，这条连接就接到电脑的识别上。 */
    data class SpeechHello(val version: UInt) : SpeechClientMsg()

    /** 录完了，识别之前发去的录音。 */
    data object Finish : SpeechClientMsg()
}

internal object SpeechClientMsgSerializer : EncodeOnlySerializer<SpeechClientMsg>("SpeechClientMsg") {
    override fun toJson(value: SpeechClientMsg): JsonElement = when (value) {
        is SpeechClientMsg.SpeechHello -> typed("speech_hello") { putUInt("version", value.version) }
        SpeechClientMsg.Finish -> typed("finish")
    }
}

/** 电脑发给手机的识别消息。 */
@Serializable(with = SpeechHostMsgSerializer::class)
sealed class SpeechHostMsg {
    /** 握手后的第一条。[installed] 为假时电脑还没下载它设置的识别模型，识别只会失败。 */
    data class Ready(val installed: Boolean) : SpeechHostMsg()

    /** 电脑的版本和这个 app 不同，随后断开。 */
    data class Incompatible(val version: UInt) : SpeechHostMsg()

    /** 识别出的文字；没听出话来时是空的。 */
    data class Text(val text: String) : SpeechHostMsg()

    /** 识别不了，[reason] 是给人看的英文。 */
    data class Failed(val reason: String) : SpeechHostMsg()

    /** 这个 app 不认识的消息，忽略。 */
    data object Unknown : SpeechHostMsg()
}

internal object SpeechHostMsgSerializer : DecodeOnlySerializer<SpeechHostMsg>("SpeechHostMsg") {
    override fun fromJson(element: JsonElement): SpeechHostMsg {
        val c = element.asObject("speech message")
        return when (c.type()) {
            "ready" -> SpeechHostMsg.Ready(c.bool("installed"))
            "incompatible" -> SpeechHostMsg.Incompatible(c.uint("version"))
            "text" -> SpeechHostMsg.Text(c.string("text"))
            "failed" -> SpeechHostMsg.Failed(c.string("reason"))
            else -> SpeechHostMsg.Unknown
        }
    }
}
