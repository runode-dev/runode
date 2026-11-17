package dev.runode.mobile.connection

import dev.runode.mobile.protocol.Base64URL
import javax.crypto.Cipher
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/** AES-GCM 加密后的字节和它的 IV；存成文本时是 `IV.密文`，两段都是 Base64URL。 */
internal class Sealed(val iv: ByteArray, val ciphertext: ByteArray) {
    /** 用 [key] 解开它的 [Cipher]，还没解。 */
    fun decrypter(key: SecretKey): Cipher {
        val spec = GCMParameterSpec(TAG_BITS, iv)
        return Cipher.getInstance(TRANSFORMATION).apply { init(Cipher.DECRYPT_MODE, key, spec) }
    }

    /** 用 [decrypter] 给的（或者验证过身份的）[cipher] 解开。 */
    fun open(cipher: Cipher): ByteArray = cipher.doFinal(ciphertext)

    fun encoded(): String = Base64URL.encode(iv) + "." + Base64URL.encode(ciphertext)

    companion object {
        /** 用 [key] 加密的 [Cipher]，IV 由它随机生成。 */
        fun encrypter(key: SecretKey): Cipher = Cipher.getInstance(TRANSFORMATION).apply { init(Cipher.ENCRYPT_MODE, key) }

        /** 用 [encrypter] 给的（或者验证过身份的）[cipher] 加密 [plaintext]。 */
        fun seal(plaintext: ByteArray, cipher: Cipher): Sealed {
            val ciphertext = cipher.doFinal(plaintext)
            return Sealed(cipher.iv, ciphertext)
        }

        /** 读 [encoded] 写出的文本；格式不对时为空。 */
        fun decode(text: String): Sealed? {
            val iv = Base64URL.decode(text.substringBefore('.', "")) ?: return null
            val ciphertext = Base64URL.decode(text.substringAfter('.', "")) ?: return null
            return Sealed(iv, ciphertext)
        }

        private const val TRANSFORMATION = "AES/GCM/NoPadding"
        private const val TAG_BITS = 128
    }
}
