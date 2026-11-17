package dev.runode.mobile.connection

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.util.UUID
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/** Android Keystore 里的设备私钥：生成、签名，再用发给电脑的 X9.63 公钥验签。 */
@RunWith(AndroidJUnit4::class)
class AndroidKeyStoreDeviceKeysTests {
    private val keys = AndroidKeyStoreDeviceKeys(
        InstrumentationRegistry.getInstrumentation().targetContext,
        aliasPrefix = "dev.runode.mobile.test-device-key.",
    )

    /** iroh 端点私钥加密存下，再取是同一把；删设备私钥时一起删掉。 */
    @Test
    fun tunnelSecretsSurviveARoundTrip() {
        val machine = UUID.randomUUID()
        try {
            keys.generate(machine)
            assertNull(keys.storedTunnelSecret(machine))
            val secret = keys.tunnelSecret(machine)
            assertEquals(32, secret.size)
            assertContentEquals(secret, keys.storedTunnelSecret(machine))
            assertContentEquals(secret, keys.tunnelSecret(machine))
        } finally {
            keys.deleteKey(machine)
        }
        assertNull(keys.storedTunnelSecret(machine))
    }

    @Test
    fun generatedKeysSignVerifiably() {
        val machine = UUID.randomUUID()
        try {
            val key = keys.generate(machine)
            val publicKey = key.publicKeyX963
            assertEquals(65, publicKey.size)
            assertEquals(4, publicKey[0].toInt())
            val payload = "runode".encodeToByteArray()
            val signature = key.sign(payload)
            assertTrue(verifyP256(publicKey, payload, signature))
            assertFalse(verifyP256(publicKey, "other".encodeToByteArray(), signature))
            // 再取出来的是同一把。
            assertContentEquals(publicKey, assertNotNull(keys.key(machine)).publicKeyX963)
        } finally {
            keys.deleteKey(machine)
        }
        assertNull(keys.key(machine))
    }
}
