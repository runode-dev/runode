package dev.runode.mobile.connection

import dev.runode.mobile.protocol.Base64URL
import dev.runode.mobile.protocol.Frame
import dev.runode.mobile.protocol.FrameError
import dev.runode.mobile.protocol.FrameKind
import dev.runode.mobile.protocol.GateMessage
import dev.runode.mobile.protocol.GatePurpose
import dev.runode.mobile.protocol.GateRejection
import dev.runode.mobile.protocol.GateSignature
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNotNull
import kotlin.test.assertTrue
import kotlin.time.Duration.Companion.milliseconds
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Test

class GateClientTests {
    private val key = SoftwareDeviceKey.generate()

    /** 验 `remote_auth`/`remote_pair` 里的签名：签的是 nonce、exporter 和用途拼起来的字节串。 */
    private fun verify(signature: String, purpose: GatePurpose, transport: FakeTransport): Boolean {
        val der = assertNotNull(Base64URL.decode(signature))
        val payload = GateSignature.payload(FakeHost.nonce, transport.exporterBytes, purpose)
        return verifyP256(key.publicKeyX963, payload, der)
    }

    @Test
    fun logsInWithASignedAuth() = runBlocking {
        val transport = FakeTransport()
        val host = FakeHost(transport)
        host.challenge()
        val outcome = async { GateClient.run(transport, GateCredential.Auth("00112233445566778899aabbccddeeff", key)) }
        val auth = assertNotNull(host.readControl())
        assertEquals("remote_auth", auth["type"]?.jsonPrimitive?.content)
        assertEquals("00112233445566778899aabbccddeeff", auth["device_id"]?.jsonPrimitive?.content)
        assertTrue(verify(auth.getValue("signature").jsonPrimitive.content, GatePurpose.Auth, transport))
        host.send(GateMessage.Accepted("00112233445566778899aabbccddeeff"))
        val result = outcome.await()
        assertEquals("00112233445566778899aabbccddeeff", result.deviceId)
        assertEquals("Ethan 的 MacBook Pro", result.hostName)
        // 门禁过后放开单帧上限，之后照常读。
        assertEquals(Frame.maxPayload, result.decoder.maxPayload)
    }

    @Test
    fun pairsWithAPublicKey() = runBlocking {
        val transport = FakeTransport()
        val host = FakeHost(transport)
        host.challenge()
        val secret = ByteArray(32) { 0xAB.toByte() }
        val outcome = async { GateClient.run(transport, GateCredential.Pair(secret, "Ethan 的 Pixel", key)) }
        val pair = assertNotNull(host.readControl())
        assertEquals("remote_pair", pair["type"]?.jsonPrimitive?.content)
        assertEquals("Ethan 的 Pixel", pair["device_name"]?.jsonPrimitive?.content)
        assertContentEquals(secret, Base64URL.decode(pair.getValue("secret").jsonPrimitive.content))
        val publicKey = assertNotNull(Base64URL.decode(pair.getValue("public_key").jsonPrimitive.content))
        assertEquals(65, publicKey.size)
        assertEquals(4, publicKey.first().toInt())
        assertTrue(verify(pair.getValue("signature").jsonPrimitive.content, GatePurpose.Pair, transport))
        host.send(GateMessage.Accepted("ffeeddccbbaa99887766554433221100"))
        assertEquals("ffeeddccbbaa99887766554433221100", outcome.await().deviceId)
    }

    @Test
    fun rejectionIsReported() = runBlocking {
        val transport = FakeTransport()
        val host = FakeHost(transport)
        host.challenge()
        val outcome = async {
            runCatching { GateClient.run(transport, GateCredential.Pair(ByteArray(32), "x", key)) }
        }
        host.readControl()
        host.send(GateMessage.Rejected(GateRejection.PairingInvalid))
        assertEquals(LinkFailure.Rejected(GateRejection.PairingInvalid), outcome.await().exceptionOrNull())
    }

    @Test
    fun aNewerGateVersionAsksForAnUpgrade() = runBlocking {
        val transport = FakeTransport()
        FakeHost(transport).challenge(version = 2u)
        val failure = assertFailsWith<LinkFailure> { GateClient.run(transport, GateCredential.Auth("x", key)) }
        assertEquals(LinkFailure.GateVersion(2u), failure)
    }

    @Test
    fun aSilentHostTimesOut() = runBlocking {
        val transport = FakeTransport()
        val failure = assertFailsWith<LinkFailure> {
            GateClient.run(transport, GateCredential.Auth("x", key), timeout = 100.milliseconds)
        }
        assertEquals(LinkFailure.Timeout, failure)
    }

    @Test
    fun oversizedGateFramesAreRefused() = runBlocking {
        val transport = FakeTransport()
        FakeHost(transport).send(Frame(FrameKind.Control, 0u, ByteArray(17 * 1024)))
        val failure = assertFailsWith<FrameError> { GateClient.run(transport, GateCredential.Auth("x", key)) }
        assertEquals(FrameError.TooLong((17 * 1024).toULong()), failure)
    }
}

class DeviceKeyTests {
    @Test
    fun softwareKeysSignVerifiably() {
        val key = SoftwareDeviceKey.generate()
        val payload = "runode".encodeToByteArray()
        assertTrue(verifyP256(key.publicKeyX963, payload, key.sign(payload)))
        assertEquals(65, key.publicKeyX963.size)
    }

    /** 公钥的两个坐标不论多短，都左补零到 32 字节。 */
    @Test
    fun publicKeysAlwaysHaveFixedWidthCoordinates() {
        repeat(64) {
            val key = SoftwareDeviceKey.generate()
            val x963 = key.publicKeyX963
            assertEquals(65, x963.size)
            assertContentEquals(x963, p256PublicKey(x963).x963())
        }
    }

    /**
     * Rust 那边用 OpenSSL 签的样例（`signature_p256.json`），`java.security` 验得过：两边对 DER 签名和 X9.63 公钥的
     * 理解一样。
     */
    @Test
    fun macSignatureFixtureVerifies() {
        val sample = Json.parseToJsonElement(remoteFixture("signature_p256.json").readText()).jsonObject
        fun field(name: String) = assertNotNull(Base64URL.decode(sample.getValue(name).jsonPrimitive.content))
        assertTrue(verifyP256(field("public_key"), field("signed"), field("signature")))
    }

    @Test
    fun memoryStoreKeepsKeysPerMachine() {
        val keys = MemoryDeviceKeyStore()
        val machine = sampleMachine()
        val key = keys.generate(machine.id)
        assertContentEquals(key.publicKeyX963, keys.key(machine.id)?.publicKeyX963)
        keys.deleteKey(machine.id)
        assertEquals(null, keys.key(machine.id))
    }
}
