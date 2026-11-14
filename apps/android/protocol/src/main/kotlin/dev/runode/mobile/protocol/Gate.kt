package dev.runode.mobile.protocol

import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

// 远程访问的门禁：TLS 握手之后、`Hello` 之前在同一条连接上来回的几条消息。都是通道 0 的
// `FrameKind.Control` 帧，载荷是带 `type` 字段的 JSON，二进制字段一律 base64url 不带填充。
// 字节格式以宿主的 `protocol::remote` 为准。

/** 门禁的版本。宿主的 `remote_challenge` 里报的版本不是它时断开，提示升级。 */
const val gateVersion: UInt = 1u

/** 电脑在 Bonjour TXT 记录里写门禁版本的键。 */
const val bonjourVersionKey: String = "v"

/** 电脑在 Bonjour TXT 记录里写证书指纹（base64url）的键。 */
const val bonjourFingerprintKey: String = "fp"

/** 跨网络连接的 ALPN，宿主的 `remote::TUNNEL_ALPN`。 */
const val tunnelALPN: String = "runode/remote/1"

/** 宿主为什么拒绝。不认识的取值读成 [Unknown]，按通用失败处理。 */
@Serializable(with = GateRejectionSerializer::class)
sealed class GateRejection(val wireName: String) {
    /** 没配对，或者已经在电脑上撤销了。 */
    data object UnknownDevice : GateRejection("unknown_device")

    data object BadSignature : GateRejection("bad_signature")

    /** 配对口令不对、过期或已经用过。 */
    data object PairingInvalid : GateRejection("pairing_invalid")

    data object RateLimited : GateRejection("rate_limited")

    /** 电脑上关了远程访问，或者电脑那边一时办不了（读不了设备表、读写不了口令文件）。 */
    data object Disabled : GateRejection("disabled")

    data class Unknown(val kind: String) : GateRejection(kind)

    companion object {
        internal val known: List<GateRejection>
            get() = listOf(UnknownDevice, BadSignature, PairingInvalid, RateLimited, Disabled)
    }
}

internal object GateRejectionSerializer : JsonElementSerializer<GateRejection>("GateRejection") {
    override fun toJson(value: GateRejection): JsonElement = buildJsonObject { put("kind", value.wireName) }

    override fun fromJson(element: JsonElement): GateRejection {
        val kind = element.asObject("reason").string("kind")
        return GateRejection.known.firstOrNull { it.wireName == kind } ?: GateRejection.Unknown(kind)
    }
}

/** 门禁阶段的消息，两个方向的都在这里。 */
@Serializable(with = GateMessageSerializer::class)
sealed class GateMessage {
    /**
     * 宿主在 TLS 握手后立刻发：门禁版本、32 字节随机数、主机名，以及跨网络连接的地址（iroh 的公钥，
     * 电脑没开时为空）。
     */
    class Challenge(
        val version: UInt,
        val nonce: ByteArray,
        val hostName: String,
        val node: String? = null,
    ) : GateMessage() {
        override fun equals(other: Any?): Boolean =
            other is Challenge && version == other.version && nonce.contentEquals(other.nonce) &&
                hostName == other.hostName && node == other.node

        override fun hashCode(): Int = listOf(version, nonce.contentHashCode(), hostName, node).hashCode()

        override fun toString(): String =
            "Challenge(version=$version, nonce=${Base64URL.encode(nonce)}, hostName=$hostName, node=$node)"
    }

    /** 已配对的设备登录。 */
    class Auth(val deviceId: String, val signature: ByteArray) : GateMessage() {
        override fun equals(other: Any?): Boolean =
            other is Auth && deviceId == other.deviceId && signature.contentEquals(other.signature)

        override fun hashCode(): Int = deviceId.hashCode() * 31 + signature.contentHashCode()

        override fun toString(): String = "Auth(deviceId=$deviceId, signature=${Base64URL.encode(signature)})"
    }

    /** 第一次配对：二维码里的口令、设备名、P-256 公钥（X9.63 未压缩 65 字节）。 */
    class Pair(
        val secret: ByteArray,
        val deviceName: String,
        val publicKey: ByteArray,
        val signature: ByteArray,
    ) : GateMessage() {
        override fun equals(other: Any?): Boolean =
            other is Pair && secret.contentEquals(other.secret) && deviceName == other.deviceName &&
                publicKey.contentEquals(other.publicKey) && signature.contentEquals(other.signature)

        override fun hashCode(): Int = listOf(
            secret.contentHashCode(),
            deviceName,
            publicKey.contentHashCode(),
            signature.contentHashCode(),
        ).hashCode()

        override fun toString(): String = "Pair(deviceName=$deviceName, publicKey=${Base64URL.encode(publicKey)})"
    }

    data class Accepted(val deviceId: String) : GateMessage()

    data class Rejected(val reason: GateRejection) : GateMessage()

    /** 不认识的种类。 */
    data class Unknown(val type: String) : GateMessage()
}

internal object GateMessageSerializer : JsonElementSerializer<GateMessage>("GateMessage") {
    override fun toJson(value: GateMessage): JsonElement = when (value) {
        is GateMessage.Challenge -> typed("remote_challenge") {
            putUInt("version", value.version)
            put("nonce", Base64URL.encode(value.nonce))
            put("host_name", value.hostName)
            value.node?.let { put("node", it) }
        }
        is GateMessage.Auth -> typed("remote_auth") {
            put("device_id", value.deviceId)
            put("signature", Base64URL.encode(value.signature))
        }
        is GateMessage.Pair -> typed("remote_pair") {
            put("secret", Base64URL.encode(value.secret))
            put("device_name", value.deviceName)
            put("public_key", Base64URL.encode(value.publicKey))
            put("signature", Base64URL.encode(value.signature))
        }
        is GateMessage.Accepted -> typed("remote_accepted") { put("device_id", value.deviceId) }
        is GateMessage.Rejected -> typed("remote_rejected") {
            put("reason", GateRejectionSerializer.toJson(value.reason))
        }
        is GateMessage.Unknown -> typed(value.type)
    }

    override fun fromJson(element: JsonElement): GateMessage {
        val c = element.asObject("gate message")
        return when (val type = c.type()) {
            "remote_challenge" -> GateMessage.Challenge(
                version = c.uint("version"),
                nonce = c.binary("nonce"),
                hostName = c.optional("host_name", String.serializer()) ?: "",
                node = c.optional("node", String.serializer()),
            )
            "remote_auth" -> GateMessage.Auth(deviceId = c.string("device_id"), signature = c.binary("signature"))
            "remote_pair" -> GateMessage.Pair(
                secret = c.binary("secret"),
                deviceName = c.string("device_name"),
                publicKey = c.binary("public_key"),
                signature = c.binary("signature"),
            )
            "remote_accepted" -> GateMessage.Accepted(deviceId = c.string("device_id"))
            "remote_rejected" -> GateMessage.Rejected(c.required("reason", GateRejectionSerializer))
            else -> GateMessage.Unknown(type)
        }
    }

    private fun JsonObject.binary(key: String): ByteArray =
        Base64URL.decode(string(key)) ?: throw SerializationException("$key is not unpadded base64url")
}

/** 签名的用途，拼进被签的字节串末尾。 */
enum class GatePurpose(val rawValue: String) {
    Auth("auth"),
    Pair("pair"),
    ;

    companion object {
        fun fromRaw(rawValue: String): GatePurpose? = entries.firstOrNull { it.rawValue == rawValue }
    }
}

/**
 * 门禁里被签的字节串：[domain]（16 字节 ASCII）、`nonce`（32 字节）、TLS exporter（[exporterLength] 字节，
 * label [exporterLabel]、不带 context）、用途（ASCII）依次拼起来，中间没有分隔和长度。exporter 把签名绑在这条
 * TLS 连接上，中间人转发不了。签名是 ECDSA P-256 + SHA-256，DER 编码。
 */
object GateSignature {
    val domain: ByteArray get() = "runode-remote-v1".encodeToByteArray()

    /** TLS exporter 的 label 和长度。 */
    const val exporterLabel: String = "EXPORTER-runode-remote"
    const val exporterLength: Int = 32
    const val nonceLength: Int = 32

    fun payload(nonce: ByteArray, exporter: ByteArray, purpose: GatePurpose): ByteArray =
        domain + nonce + exporter + purpose.rawValue.encodeToByteArray()
}

/**
 * RFC 4648 §5 的 base64url，不带填充。读的时候只认规范的写法：有字母表以外的字符（包括填充的 `=`）、长度不可能，
 * 或者最后一个字符多出来的位不是 0 时读不了。
 */
object Base64URL {
    private const val alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"

    fun encode(data: ByteArray): String = buildString((data.size + 2) / 3 * 4) {
        for (chunk in data.indices step 3) {
            val count = minOf(3, data.size - chunk)
            var n = 0
            for (i in 0 until count) n = n or ((data[chunk + i].toInt() and 0xFF) shl (16 - 8 * i))
            for (i in 0..count) append(alphabet[(n shr (18 - 6 * i)) and 0x3F])
        }
    }

    fun decode(text: String): ByteArray? {
        if (text.length % 4 == 1) return null
        val out = java.io.ByteArrayOutputStream(text.length / 4 * 3 + 2)
        for (chunk in text.indices step 4) {
            val count = minOf(4, text.length - chunk)
            var n = 0
            for (i in 0 until count) {
                val value = alphabet.indexOf(text[chunk + i])
                if (value < 0) return null
                n = n or (value shl (18 - 6 * i))
            }
            val bytes = count - 1
            if (n and (0x00FF_FFFF ushr (8 * bytes)) != 0) return null
            for (i in 0 until bytes) out.write(n shr (16 - 8 * i))
        }
        return out.toByteArray()
    }
}
