package dev.runode.mobile.protocol

import java.time.Instant
import kotlinx.serialization.KSerializer
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.descriptors.PrimitiveKind
import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder

/** 电脑证书的指纹：叶子证书 DER 的 SHA-256，32 字节。[toString] 是它的 base64url。 */
@Serializable(with = CertificateFingerprintSerializer::class)
class CertificateFingerprint private constructor(private val raw: ByteArray) {
    val bytes: ByteArray get() = raw.copyOf()

    override fun equals(other: Any?): Boolean = other is CertificateFingerprint && raw.contentEquals(other.raw)

    override fun hashCode(): Int = raw.contentHashCode()

    override fun toString(): String = Base64URL.encode(raw)

    companion object {
        /** 不是 32 字节时为空。 */
        operator fun invoke(bytes: ByteArray): CertificateFingerprint? =
            if (bytes.size == 32) CertificateFingerprint(bytes.copyOf()) else null

        /** 读二维码和 Bonjour TXT 里的 `fp`，base64url；读不了时为空。 */
        operator fun invoke(text: String): CertificateFingerprint? = Base64URL.decode(text)?.let { invoke(it) }
    }
}

internal object CertificateFingerprintSerializer : KSerializer<CertificateFingerprint> {
    override val descriptor = PrimitiveSerialDescriptor("CertificateFingerprint", PrimitiveKind.STRING)

    override fun serialize(encoder: Encoder, value: CertificateFingerprint) = encoder.encodeString(value.toString())

    override fun deserialize(decoder: Decoder): CertificateFingerprint =
        CertificateFingerprint(decoder.decodeString()) ?: throw SerializationException("bad fingerprint")
}

/**
 * 二维码里的配对邀请：
 * `runode://pair?v=1&name=<主机名>&fp=<指纹>&secret=<口令>&port=<端口>&addr=<地址1>,<地址2>&exp=<Unix 秒>&node=<跨网络的地址>`。
 * 参数的先后不限，不认识的参数忽略。
 */
class PairingInvitation(
    val hostName: String,
    val fingerprint: CertificateFingerprint,
    /** 32 字节的一次性配对口令。 */
    val secret: ByteArray,
    val port: UShort,
    /** 生成二维码时电脑的非回环地址，挨个试。 */
    val addresses: List<String>,
    val expiresAt: Instant,
    /** 跨网络连接的地址（iroh 的公钥），电脑没开时为空：不在同一个网络时按它拨过去。 */
    val node: String? = null,
) {
    fun isExpired(at: Instant = Instant.now()): Boolean = !at.isBefore(expiresAt)

    override fun equals(other: Any?): Boolean =
        other is PairingInvitation && hostName == other.hostName && fingerprint == other.fingerprint &&
            secret.contentEquals(other.secret) && port == other.port && addresses == other.addresses &&
            expiresAt == other.expiresAt && node == other.node

    override fun hashCode(): Int =
        listOf(hostName, fingerprint, secret.contentHashCode(), port, addresses, expiresAt, node).hashCode()

    override fun toString(): String =
        "PairingInvitation(hostName=$hostName, fingerprint=$fingerprint, port=$port, addresses=$addresses, " +
            "expiresAt=$expiresAt, node=$node)"

    companion object {
        /** 现在认得的邀请版本。 */
        const val version: Int = 1

        /** 解析邀请链接，前后的空白不算；读不了时抛 [PairingInvitationError]。 */
        fun parse(text: String): PairingInvitation {
            val items = queryItems(text.trim(), scheme = "runode", host = "pair")
                ?: throw PairingInvitationError.NotAnInvitation
            val fields = items.toMap()
            val version = fields["v"]?.toIntOrNull() ?: throw PairingInvitationError.Missing("v")
            if (version != PairingInvitation.version) throw PairingInvitationError.UnsupportedVersion(version)
            val name = fields["name"]?.takeIf { it.isNotEmpty() } ?: throw PairingInvitationError.Missing("name")
            val fpText = fields["fp"] ?: throw PairingInvitationError.Missing("fp")
            val fingerprint = CertificateFingerprint(fpText) ?: throw PairingInvitationError.Invalid("fp")
            val secretText = fields["secret"] ?: throw PairingInvitationError.Missing("secret")
            val secret = Base64URL.decode(secretText)?.takeIf { it.size == 32 }
                ?: throw PairingInvitationError.Invalid("secret")
            val portText = fields["port"] ?: throw PairingInvitationError.Missing("port")
            val port = portText.toUShortOrNull()?.takeIf { it != 0.toUShort() }
                ?: throw PairingInvitationError.Invalid("port")
            val addresses = (fields["addr"] ?: "").split(',').map { it.trim() }.filter { it.isNotEmpty() }
            val expText = fields["exp"] ?: throw PairingInvitationError.Missing("exp")
            val exp = expText.toDoubleOrNull()?.takeIf { it.isFinite() } ?: throw PairingInvitationError.Invalid("exp")
            val seconds = Math.floor(exp)
            return PairingInvitation(
                hostName = name,
                fingerprint = fingerprint,
                secret = secret,
                port = port,
                addresses = addresses,
                expiresAt = Instant.ofEpochSecond(seconds.toLong(), ((exp - seconds) * 1e9).toLong()),
                node = fields["node"]?.takeIf { it.isNotEmpty() },
            )
        }
    }
}

/** 邀请链接读不了的原因。给人看的说法由界面按种类给出。 */
sealed class PairingInvitationError(message: String) : Exception(message) {
    data object NotAnInvitation : PairingInvitationError("not a runode pairing link")

    data class UnsupportedVersion(val version: Int) : PairingInvitationError("unsupported pairing link version $version")

    data class Missing(val field: String) : PairingInvitationError("the pairing link has no $field")

    data class Invalid(val field: String) : PairingInvitationError("the pairing link has a bad $field")
}
