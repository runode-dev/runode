package dev.runode.mobile.protocol

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement

/**
 * 交给电脑图片和视频的这套消息的版本，要等于宿主的 `runode_protocol::media::MEDIA_VERSION`，
 * 否则电脑回 `Incompatible` 后断开。
 */
const val mediaVersion: UInt = 1u

/**
 * 一个文件最多这么多字节，要等于宿主的 `runode_protocol::media::MAX_MEDIA_BYTES`，否则手机放行的文件可能
 * 被电脑回 [MediaHostMsg.Failed] 后断开。
 */
const val maxMediaBytes: Int = 200 shl 20

/**
 * 一帧 [FrameKind.Media] 的载荷最多这么多字节，要等于宿主的 `runode_protocol::media::MAX_MEDIA_FRAME`，否则
 * 手机发的帧可能超了电脑的上限，被电脑断开。
 */
const val maxMediaFrame: Int = 1 shl 20

/** 手机发给电脑的图片、视频消息。 */
@Serializable(with = MediaClientMsgSerializer::class)
sealed class MediaClientMsg {
    /** 过了门禁后的第一条，这条连接就接到电脑收图片和视频的地方。 */
    data class MediaHello(val version: UInt) : MediaClientMsg()

    /** 要发一个文件了，随后的 [FrameKind.Media] 帧的载荷加起来正好 [len] 字节。 */
    data class Media(val len: UInt) : MediaClientMsg()
}

internal object MediaClientMsgSerializer : EncodeOnlySerializer<MediaClientMsg>("MediaClientMsg") {
    override fun toJson(value: MediaClientMsg): JsonElement = when (value) {
        is MediaClientMsg.MediaHello -> typed("media_hello") { putUInt("version", value.version) }
        is MediaClientMsg.Media -> typed("media") { putUInt("len", value.len) }
    }
}

/** 电脑发给手机的图片、视频消息。 */
@Serializable(with = MediaHostMsgSerializer::class)
sealed class MediaHostMsg {
    /** 握手后的第一条。 */
    data object Ready : MediaHostMsg()

    /** 电脑的版本和这个 app 不同，随后断开。 */
    data class Incompatible(val version: UInt) : MediaHostMsg()

    /** 存好了，[path] 是文件在电脑上的绝对路径。 */
    data class Saved(val path: String) : MediaHostMsg()

    /** 存不了，[reason] 是给人看的英文。 */
    data class Failed(val reason: String) : MediaHostMsg()

    /** 这个 app 不认识的消息，忽略。 */
    data object Unknown : MediaHostMsg()
}

internal object MediaHostMsgSerializer : DecodeOnlySerializer<MediaHostMsg>("MediaHostMsg") {
    override fun fromJson(element: JsonElement): MediaHostMsg {
        val c = element.asObject("media message")
        return when (c.type()) {
            "ready" -> MediaHostMsg.Ready
            "incompatible" -> MediaHostMsg.Incompatible(c.uint("version"))
            "saved" -> MediaHostMsg.Saved(c.string("path"))
            "failed" -> MediaHostMsg.Failed(c.string("reason"))
            else -> MediaHostMsg.Unknown
        }
    }
}
