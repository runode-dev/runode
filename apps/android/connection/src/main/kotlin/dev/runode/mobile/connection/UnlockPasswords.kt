package dev.runode.mobile.connection

import android.content.Context
import android.content.SharedPreferences
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.security.keystore.KeyProperties
import java.security.KeyStore
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey

/**
 * 远程桌面里替用户解开锁屏的电脑登录密码，每台电脑一个，只存在手机上。存和取都要用户当场验证身份：先拿
 * [cipherForSaving] 或 [cipherForReading] 给的 [Cipher]，交给界面层的 BiometricPrompt（`CryptoObject`）验证，验证过后
 * 再拿它调 [save] 或 [password]。
 */
interface UnlockPasswordStore {
    /** 有没有存着，不验证身份。 */
    fun hasPassword(machine: UUID): Boolean

    /** 存密码用的 [Cipher]，还没验证身份。 */
    fun cipherForSaving(machine: UUID): Cipher

    /** 用验证过身份的 [cipher]（[cipherForSaving] 给的）存下，换掉原有的。 */
    fun save(password: String, machine: UUID, cipher: Cipher)

    /** 取密码用的 [Cipher]，还没验证身份；没存着（或者手机的锁屏、生物识别变了，存的已经作废）时为空。 */
    fun cipherForReading(machine: UUID): Cipher?

    /** 用验证过身份的 [cipher]（[cipherForReading] 给的）取出。 */
    fun password(machine: UUID, cipher: Cipher): String

    fun deletePassword(machine: UUID)
}

/** 只记在内存里，测试用；[Cipher] 用软件的 AES-GCM 密钥，不验证身份。 */
class MemoryUnlockPasswords : UnlockPasswordStore {
    private val key: SecretKey = KeyGenerator.getInstance("AES").apply { init(256) }.generateKey()
    private val sealed = ConcurrentHashMap<UUID, Sealed>()

    override fun hasPassword(machine: UUID): Boolean = sealed.containsKey(machine)

    override fun cipherForSaving(machine: UUID): Cipher = Sealed.encrypter(key)

    override fun save(password: String, machine: UUID, cipher: Cipher) {
        sealed[machine] = Sealed.seal(password.encodeToByteArray(), cipher)
    }

    override fun cipherForReading(machine: UUID): Cipher? = sealed[machine]?.decrypter(key)

    override fun password(machine: UUID, cipher: Cipher): String =
        sealed[machine]?.open(cipher)?.decodeToString() ?: throw LinkFailure.KeyStore("no unlock password for $machine")

    override fun deletePassword(machine: UUID) {
        sealed.remove(machine)
    }
}

/**
 * 用 Android Keystore 里的一把 AES-GCM 密钥加密，密文存在 app 私有的 SharedPreferences 里。密钥每次用都要用户验证
 * （生物识别或手机密码），只在设了锁屏的本机上有，不随备份迁走；手机关掉锁屏或者生物识别变了时系统作废它，存的密码
 * 跟着全部删掉。
 */
class KeystoreUnlockPasswords(context: Context, private val alias: String = "dev.runode.mobile.unlock-password") :
    UnlockPasswordStore {
    private val preferences: SharedPreferences =
        context.getSharedPreferences("dev.runode.mobile.unlock-passwords", Context.MODE_PRIVATE)

    override fun hasPassword(machine: UUID): Boolean = preferences.contains(machine.toString())

    override fun cipherForSaving(machine: UUID): Cipher = keystore {
        try {
            Sealed.encrypter(key())
        } catch (_: KeyPermanentlyInvalidatedException) {
            forgetAll()
            Sealed.encrypter(key())
        }
    }

    override fun save(password: String, machine: UUID, cipher: Cipher) = keystore {
        val sealed = Sealed.seal(password.encodeToByteArray(), cipher)
        preferences.edit().putString(machine.toString(), sealed.encoded()).apply()
    }

    override fun cipherForReading(machine: UUID): Cipher? = keystore {
        val stored = preferences.getString(machine.toString(), null)?.let(Sealed::decode) ?: return@keystore null
        val key = keyStore().getKey(alias, null) as? SecretKey
        if (key == null) {
            forgetAll()
            return@keystore null
        }
        try {
            stored.decrypter(key)
        } catch (_: KeyPermanentlyInvalidatedException) {
            forgetAll()
            null
        }
    }

    override fun password(machine: UUID, cipher: Cipher): String = keystore {
        val stored = preferences.getString(machine.toString(), null)?.let(Sealed::decode)
            ?: throw LinkFailure.KeyStore("no unlock password for $machine")
        stored.open(cipher).decodeToString()
    }

    override fun deletePassword(machine: UUID) {
        preferences.edit().remove(machine.toString()).apply()
    }

    private fun keyStore(): KeyStore = KeyStore.getInstance(PROVIDER).apply { load(null) }

    /** 现有的密钥，没有时生成一把。 */
    private fun key(): SecretKey {
        (keyStore().getKey(alias, null) as? SecretKey)?.let { return it }
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, PROVIDER)
        generator.init(
            KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .setUserAuthenticationRequired(true)
                .setUserAuthenticationParameters(0, KeyProperties.AUTH_BIOMETRIC_STRONG or KeyProperties.AUTH_DEVICE_CREDENTIAL)
                .build(),
        )
        return generator.generateKey()
    }

    /** 密钥作废了：删掉它和用它加密的全部密码。 */
    private fun forgetAll() {
        keyStore().deleteEntry(alias)
        preferences.edit().clear().apply()
    }

    private companion object {
        const val PROVIDER = "AndroidKeyStore"
    }
}
