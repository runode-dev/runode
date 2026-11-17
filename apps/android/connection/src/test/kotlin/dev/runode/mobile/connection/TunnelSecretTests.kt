package dev.runode.mobile.connection

import dev.runode.mobile.protocol.Base64URL
import java.util.UUID
import java.util.concurrent.atomic.AtomicInteger
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.runBlocking
import org.junit.Test

/** 每台电脑一把 iroh 端点私钥：存着的照旧用，存不进或者读不了时用进程里记着的临时私钥。 */
class TunnelSecretTests {
    private val keys = FlakyKeys()
    private val machine = UUID.randomUUID().also { keys.generate(it) }

    @Test
    fun keepsOneSecretPerComputer() {
        val secret = keys.tunnelSecret(machine)
        assertEquals(32, secret.size)
        assertContentEquals(secret, keys.tunnelSecret(machine))
        assertContentEquals(secret, keys.storedTunnelSecret(machine))
        val other = UUID.randomUUID().also { keys.generate(it) }
        assertFalse(secret.contentEquals(keys.tunnelSecret(other)), "each computer sees a different phone key")
    }

    @Test
    fun aComputerWithoutADeviceKeyHasNoSecret() {
        assertFailsWith<LinkFailure.MissingKey> { keys.tunnelSecret(UUID.randomUUID()) }
    }

    /** 锁定存不进时每次给同一把临时私钥，存得进了就存下这一把。 */
    @Test
    fun anUnsavedSecretStaysTheSameUntilItIsSaved() {
        keys.saveFails = true
        val secret = keys.tunnelSecret(machine)
        assertContentEquals(secret, keys.tunnelSecret(machine))
        assertNull(keys.storedTunnelSecret(machine))
        keys.saveFails = false
        assertContentEquals(secret, keys.tunnelSecret(machine))
        assertContentEquals(secret, keys.storedTunnelSecret(machine))
    }

    /** 锁定存着的读不了时用临时私钥，不拿新的覆盖存着的。 */
    @Test
    fun anUnreadableSecretIsNotOverwritten() {
        val stored = keys.tunnelSecret(machine)
        val saves = keys.saves.get()
        keys.readFails = true
        val temporary = keys.tunnelSecret(machine)
        assertContentEquals(temporary, keys.tunnelSecret(machine))
        assertEquals(saves, keys.saves.get())
        keys.readFails = false
        assertContentEquals(stored, keys.tunnelSecret(machine))
    }

    /** 锁定同时取的几次拿到同一把，只存一次。 */
    @Test
    fun concurrentCallersShareOneSecret() = runBlocking {
        val secrets = List(16) { async(Dispatchers.Default) { keys.tunnelSecret(machine).toList() } }.awaitAll()
        assertEquals(1, secrets.toSet().size)
        assertEquals(1, keys.saves.get())
    }

    /** 锁定删掉时交出存着的和临时的私钥，之后都没了。 */
    @Test
    fun deletingReturnsEverySecretItUsed() {
        val stored = keys.tunnelSecret(machine)
        keys.readFails = true
        val temporary = keys.tunnelSecret(machine)
        keys.readFails = false
        val deleted = keys.deleteKeyAndTunnel(machine).map { it.toList() }
        assertEquals(listOf(stored.toList(), temporary.toList()), deleted)
        assertNull(keys.key(machine))
        assertNull(keys.storedTunnelSecret(machine))
        assertTrue(keys.deleteKeyAndTunnel(machine).isEmpty())
    }

    /** 可以叫它存不进、读不了的内存密钥库。 */
    private class FlakyKeys(private val inner: MemoryDeviceKeyStore = MemoryDeviceKeyStore()) : DeviceKeyStore by inner {
        @Volatile
        var saveFails = false

        @Volatile
        var readFails = false

        val saves = AtomicInteger()

        override fun storedTunnelSecret(machine: UUID): ByteArray? {
            if (readFails) throw LinkFailure.KeyStore("locked")
            return inner.storedTunnelSecret(machine)
        }

        override fun saveTunnelSecret(machine: UUID, secret: ByteArray) {
            if (saveFails) throw LinkFailure.KeyStore("full")
            saves.incrementAndGet()
            inner.saveTunnelSecret(machine, secret)
        }
    }
}

/** 存着的 iroh 端点私钥密文：格式不对、验签不过的当成没存过。 */
class StoredTunnelSecretTests {
    private val key: SecretKey = KeyGenerator.getInstance("AES").apply { init(256) }.generateKey()
    private val secret = ByteArray(32) { it.toByte() }

    private fun seal(plaintext: ByteArray, with: SecretKey = key) = Sealed.seal(plaintext, Sealed.encrypter(with))

    @Test
    fun opensWhatWasSealed() {
        assertContentEquals(secret, openTunnelSecret(seal(secret).encoded(), key))
    }

    @Test
    fun malformedTextIsAbsent() {
        val sealed = seal(secret)
        for (text in listOf("", ".", "nodot", "." + Base64URL.encode(sealed.ciphertext), Base64URL.encode(sealed.iv) + ".")) {
            assertNull(openTunnelSecret(text, key), "\"$text\"")
        }
    }

    @Test
    fun tamperedCiphertextIsAbsent() {
        val sealed = seal(secret)
        val tampered = sealed.ciphertext.copyOf().also { it[0] = (it[0].toInt() xor 1).toByte() }
        assertNull(openTunnelSecret(Sealed(sealed.iv, tampered).encoded(), key))
    }

    /** 加密它的密钥换过了：旧密文验签不过。 */
    @Test
    fun aSecretSealedWithAnotherKeyIsAbsent() {
        val other = KeyGenerator.getInstance("AES").apply { init(256) }.generateKey()
        assertNull(openTunnelSecret(seal(secret, other).encoded(), key))
    }

    @Test
    fun aSecretOfTheWrongLengthIsAbsent() {
        assertNull(openTunnelSecret(seal(ByteArray(16)).encoded(), key))
    }
}
