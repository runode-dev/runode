package dev.runode.mobile.connection

import android.content.Context
import android.content.SharedPreferences
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.security.keystore.StrongBoxUnavailableException
import java.math.BigInteger
import android.util.Log
import java.security.AlgorithmParameters
import java.security.InvalidAlgorithmParameterException
import java.security.KeyFactory
import java.security.KeyPair
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.PrivateKey
import java.security.SecureRandom
import java.security.Signature
import java.security.interfaces.ECPublicKey
import java.security.spec.ECGenParameterSpec
import java.security.spec.ECParameterSpec
import java.security.spec.ECPoint
import java.security.spec.ECPublicKeySpec
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import javax.crypto.AEADBadTagException
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey

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

    /** 存着的 [machine] 的 iroh 端点私钥（32 字节）。没存过、或者存着的坏了时为空，读不了时抛错。 */
    fun storedTunnelSecret(machine: UUID): ByteArray?

    /** 存下 [machine] 的 iroh 端点私钥，换掉原有的；存不进时抛错。 */
    fun saveTunnelSecret(machine: UUID, secret: ByteArray)

    /** 删掉设备私钥和 iroh 端点私钥。 */
    fun deleteKey(machine: UUID)
}

/** 存不进（或者存着的读不了）的 iroh 端点私钥，按电脑记在这个进程里。[tunnelSecret] 和 [deleteKeyAndTunnel] 都在它的锁里排队。 */
private val temporaryTunnelSecrets = HashMap<UUID, ByteArray>()

/**
 * 跨网络连 [machine] 用的 iroh 端点私钥：每台电脑一把，各台电脑看到的这部手机的公钥不同。没存过时新生成一把存下；存不进、
 * 或者存着的读不了时用进程里为这台电脑记着的临时私钥（没有就生成一把记下），下次照旧用它，存不进的再试着存，读不了的
 * 不覆盖。没有设备私钥时抛 [LinkFailure.MissingKey]。读写 Keystore，不在主线程上调。
 */
fun DeviceKeyStore.tunnelSecret(machine: UUID): ByteArray {
    synchronized(temporaryTunnelSecrets) {
        key(machine) ?: throw LinkFailure.MissingKey
        val readable = try {
            storedTunnelSecret(machine)?.let { stored ->
                temporaryTunnelSecrets.remove(machine)
                return stored
            }
            true
        } catch (_: Exception) {
            false
        }
        val secret = temporaryTunnelSecrets[machine] ?: newTunnelSecret()
        val saved = readable && try {
            saveTunnelSecret(machine, secret)
            true
        } catch (_: Exception) {
            false
        }
        if (saved) temporaryTunnelSecrets.remove(machine) else temporaryTunnelSecrets[machine] = secret
        return secret
    }
}

/**
 * 删掉 [machine] 的设备私钥和 iroh 端点私钥，连同进程里为它记着的临时私钥；返回跨网络连它用过的 iroh 端点私钥，它们的
 * 端点由调用方关掉。只在不再连这台电脑时调，所以删不掉时不报。读写 Keystore，还可能等着 [tunnelSecret] 生成密钥，
 * 不在主线程上调。
 */
fun DeviceKeyStore.deleteKeyAndTunnel(machine: UUID): List<ByteArray> = synchronized(temporaryTunnelSecrets) {
    val secrets = listOfNotNull(
        runCatching { storedTunnelSecret(machine) }.getOrNull(),
        temporaryTunnelSecrets.remove(machine),
    ).distinctBy { it.toList() }
    runCatching { deleteKey(machine) }
    secrets
}

/** iroh 端点私钥的长度：任意这么多字节都是合法的 Ed25519 种子。 */
private const val TUNNEL_SECRET_BYTES = 32

private fun newTunnelSecret(): ByteArray = ByteArray(TUNNEL_SECRET_BYTES).also(SecureRandom()::nextBytes)

/** 软件密钥，只记在内存里；测试和演示模式用。 */
class MemoryDeviceKeyStore : DeviceKeyStore {
    private val keys = ConcurrentHashMap<UUID, DeviceKey>()
    private val tunnels = ConcurrentHashMap<UUID, ByteArray>()

    override fun generate(machine: UUID): DeviceKey = SoftwareDeviceKey.generate().also { keys[machine] = it }

    override fun key(machine: UUID): DeviceKey? = keys[machine]

    override fun storedTunnelSecret(machine: UUID): ByteArray? = tunnels[machine]?.copyOf()

    override fun saveTunnelSecret(machine: UUID, secret: ByteArray) {
        tunnels[machine] = secret.copyOf()
    }

    override fun deleteKey(machine: UUID) {
        keys.remove(machine)
        tunnels.remove(machine)
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
 *
 * iroh 端点私钥要交给原生库，只能另存：用 Keystore 里的一把 AES-GCM 密钥（别名 [aliasPrefix] 加 `tunnel`）加密，密文
 * 按电脑存在 app 私有的 SharedPreferences 里。这把密钥不要求用户验证身份，后台重连时也能用。
 */
class AndroidKeyStoreDeviceKeys(
    context: Context,
    private val aliasPrefix: String = "dev.runode.mobile.device-key.",
) : DeviceKeyStore {
    private val tunnelSecrets: SharedPreferences =
        context.getSharedPreferences(aliasPrefix + "tunnel-secrets", Context.MODE_PRIVATE)
    private val tunnelAlias = aliasPrefix + "tunnel"

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

    /**
     * 密文格式不对或验签不过时当成没存过，记一条日志，之后存下的新私钥覆盖它；加密它的密钥没了时存着的都解不开，全部
     * 删掉。Keystore 出错读不了时记一条日志再抛。
     */
    override fun storedTunnelSecret(machine: UUID): ByteArray? = try {
        keystore {
            val text = tunnelSecrets.getString(machine.toString(), null) ?: return@keystore null
            val key = keyStore().getKey(tunnelAlias, null) as? SecretKey
            if (key == null) {
                Log.w(LOG_TAG, "the key sealing iroh secret keys is gone; dropping the stored ones")
                tunnelSecrets.edit().clear().apply()
                return@keystore null
            }
            openTunnelSecret(text, key).also {
                if (it == null) Log.w(LOG_TAG, "the stored iroh secret key for $machine is unreadable; replacing it")
            }
        }
    } catch (failure: LinkFailure.KeyStore) {
        Log.w(LOG_TAG, "could not read the iroh secret key for $machine", failure)
        throw failure
    }

    override fun saveTunnelSecret(machine: UUID, secret: ByteArray) = keystore {
        val sealed = Sealed.seal(secret, Sealed.encrypter(tunnelKey()))
        if (!tunnelSecrets.edit().putString(machine.toString(), sealed.encoded()).commit()) {
            throw LinkFailure.KeyStore("could not write the iroh secret key")
        }
    }

    override fun deleteKey(machine: UUID) = keystore {
        tunnelSecrets.edit().remove(machine.toString()).apply()
        keyStore().deleteEntry(alias(machine))
    }

    /** 加密 iroh 端点私钥的密钥，没有时生成一把，先删掉存着的：它们是旧密钥加密的，新的解不开。 */
    private fun tunnelKey(): SecretKey {
        (keyStore().getKey(tunnelAlias, null) as? SecretKey)?.let { return it }
        if (!tunnelSecrets.edit().clear().commit()) throw LinkFailure.KeyStore("could not drop the iroh secret keys")
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, PROVIDER)
        generator.init(
            KeyGenParameterSpec.Builder(tunnelAlias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build(),
        )
        return generator.generateKey()
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
        const val LOG_TAG = "RunodeDeviceKeys"
    }
}

private const val CURVE = "secp256r1"

/**
 * 用 [key] 解开 [Sealed.encoded] 存下的 iroh 端点私钥。格式不对、验签不过（密文被改过，或者是别的密钥加密的）、解出来
 * 不是 32 字节时为空；Keystore 自己出的错照样抛。
 */
internal fun openTunnelSecret(text: String, key: SecretKey): ByteArray? {
    val sealed = Sealed.decode(text) ?: return null
    if (sealed.iv.isEmpty() || sealed.ciphertext.isEmpty()) return null
    val secret = try {
        sealed.open(sealed.decrypter(key))
    } catch (_: AEADBadTagException) {
        return null
    } catch (_: InvalidAlgorithmParameterException) {
        return null
    } catch (_: IllegalArgumentException) {
        return null
    }
    return secret.takeIf { it.size == TUNNEL_SECRET_BYTES }
}

/** 跑 [block]，Keystore 和加解密的错误换成 [LinkFailure.KeyStore]。 */
internal inline fun <T> keystore(block: () -> T): T = try {
    block()
} catch (failure: LinkFailure) {
    throw failure
} catch (error: Exception) {
    throw LinkFailure.KeyStore(error.message ?: error.toString())
}

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
