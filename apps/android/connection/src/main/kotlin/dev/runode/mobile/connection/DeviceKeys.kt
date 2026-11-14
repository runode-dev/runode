package dev.runode.mobile.connection

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.security.keystore.StrongBoxUnavailableException
import java.math.BigInteger
import java.security.AlgorithmParameters
import java.security.KeyFactory
import java.security.KeyPair
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.PrivateKey
import java.security.Signature
import java.security.interfaces.ECPublicKey
import java.security.spec.ECGenParameterSpec
import java.security.spec.ECParameterSpec
import java.security.spec.ECPoint
import java.security.spec.ECPublicKeySpec
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap

/** 一台电脑对应的设备签名私钥（P-256）。私钥本身拿不出来，只能签名。 */
interface DeviceKey {
    /** 公钥，X9.63 未压缩格式（65 字节），配对时发给电脑。 */
    val publicKeyX963: ByteArray

    /** ECDSA P-256 + SHA-256 签名，DER 编码。 */
    fun sign(payload: ByteArray): ByteArray
}

/** 设备私钥放在哪里，按电脑记录的 `id` 存。真的实现是 [AndroidKeyStoreDeviceKeys]；测试用 [MemoryDeviceKeyStore]。 */
interface DeviceKeyStore {
    /** 给 [machine] 新生成一把并存好，换掉原有的。 */
    fun generate(machine: UUID): DeviceKey

    /** 没有时为空。 */
    fun key(machine: UUID): DeviceKey?

    fun deleteKey(machine: UUID)
}

/** 软件密钥，只记在内存里；测试和演示模式用。 */
class MemoryDeviceKeyStore : DeviceKeyStore {
    private val keys = ConcurrentHashMap<UUID, DeviceKey>()

    override fun generate(machine: UUID): DeviceKey = SoftwareDeviceKey.generate().also { keys[machine] = it }

    override fun key(machine: UUID): DeviceKey? = keys[machine]

    override fun deleteKey(machine: UUID) {
        keys.remove(machine)
    }
}

/** `java.security` 生成、放在进程内存里的 P-256 密钥。 */
class SoftwareDeviceKey(private val pair: KeyPair) : DeviceKey {
    override val publicKeyX963: ByteArray get() = (pair.public as ECPublicKey).x963()

    override fun sign(payload: ByteArray): ByteArray = signP256(pair.private, payload)

    companion object {
        fun generate(): SoftwareDeviceKey {
            val generator = KeyPairGenerator.getInstance("EC")
            generator.initialize(ECGenParameterSpec(CURVE))
            return SoftwareDeviceKey(generator.generateKeyPair())
        }
    }
}

/**
 * 把设备私钥生成在 Android Keystore 里，别名是 [aliasPrefix] 加这台电脑记录的编号：私钥出不了 Keystore，有 StrongBox
 * 时放在 StrongBox 里，不随备份迁到别的设备。
 */
class AndroidKeyStoreDeviceKeys(private val aliasPrefix: String = "dev.runode.mobile.device-key.") : DeviceKeyStore {
    private fun alias(machine: UUID) = aliasPrefix + machine

    private fun keyStore(): KeyStore = KeyStore.getInstance(PROVIDER).apply { load(null) }

    override fun generate(machine: UUID): DeviceKey {
        val alias = alias(machine)
        fun spec(strongBox: Boolean) = KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_SIGN)
            .setAlgorithmParameterSpec(ECGenParameterSpec(CURVE))
            .setDigests(KeyProperties.DIGEST_SHA256)
            .setIsStrongBoxBacked(strongBox)
            .build()
        try {
            val generator = KeyPairGenerator.getInstance(KeyProperties.KEY_ALGORITHM_EC, PROVIDER)
            try {
                generator.initialize(spec(strongBox = true))
                generator.generateKeyPair()
            } catch (_: StrongBoxUnavailableException) {
                generator.initialize(spec(strongBox = false))
                generator.generateKeyPair()
            }
        } catch (error: Exception) {
            throw LinkFailure.KeyStore(error.message ?: error.toString())
        }
        return key(machine) ?: throw LinkFailure.KeyStore("the generated key is missing")
    }

    override fun key(machine: UUID): DeviceKey? {
        val alias = alias(machine)
        val entry = try {
            keyStore().getEntry(alias, null) as? KeyStore.PrivateKeyEntry
        } catch (error: Exception) {
            throw LinkFailure.KeyStore(error.message ?: error.toString())
        } ?: return null
        return KeystoreKey(entry)
    }

    override fun deleteKey(machine: UUID) {
        try {
            keyStore().deleteEntry(alias(machine))
        } catch (error: Exception) {
            throw LinkFailure.KeyStore(error.message ?: error.toString())
        }
    }

    private class KeystoreKey(private val entry: KeyStore.PrivateKeyEntry) : DeviceKey {
        override val publicKeyX963: ByteArray get() = (entry.certificate.publicKey as ECPublicKey).x963()

        override fun sign(payload: ByteArray): ByteArray = try {
            signP256(entry.privateKey, payload)
        } catch (error: Exception) {
            throw LinkFailure.KeyStore(error.message ?: error.toString())
        }
    }

    private companion object {
        const val PROVIDER = "AndroidKeyStore"
    }
}

private const val CURVE = "secp256r1"

private fun signP256(key: PrivateKey, payload: ByteArray): ByteArray =
    Signature.getInstance("SHA256withECDSA").run {
        initSign(key)
        update(payload)
        sign()
    }

/** X9.63 未压缩格式：`0x04 | X | Y`，坐标各左补零到 32 字节。 */
internal fun ECPublicKey.x963(): ByteArray = byteArrayOf(4) + w.affineX.fixed32() + w.affineY.fixed32()

private fun BigInteger.fixed32(): ByteArray {
    val bytes = toByteArray().let { if (it.size > 32 && it[0] == 0.toByte()) it.copyOfRange(it.size - 32, it.size) else it }
    require(bytes.size <= 32) { "coordinate is longer than 32 bytes" }
    return ByteArray(32 - bytes.size) + bytes
}

/** 读 X9.63 未压缩格式的 P-256 公钥；格式不对时抛 [IllegalArgumentException]。 */
internal fun p256PublicKey(x963: ByteArray): ECPublicKey {
    require(x963.size == 65 && x963[0] == 4.toByte()) { "not an uncompressed P-256 point" }
    val parameters = AlgorithmParameters.getInstance("EC").apply { init(ECGenParameterSpec(CURVE)) }
        .getParameterSpec(ECParameterSpec::class.java)
    val point = ECPoint(BigInteger(1, x963.copyOfRange(1, 33)), BigInteger(1, x963.copyOfRange(33, 65)))
    return KeyFactory.getInstance("EC").generatePublic(ECPublicKeySpec(point, parameters)) as ECPublicKey
}

/** 用 X9.63 公钥验 DER 编码的 ECDSA P-256 + SHA-256 签名。 */
internal fun verifyP256(publicKeyX963: ByteArray, payload: ByteArray, signature: ByteArray): Boolean =
    Signature.getInstance("SHA256withECDSA").run {
        initVerify(p256PublicKey(publicKeyX963))
        update(payload)
        verify(signature)
    }
