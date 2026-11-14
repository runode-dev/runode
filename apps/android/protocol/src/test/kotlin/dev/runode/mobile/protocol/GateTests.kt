package dev.runode.mobile.protocol

import java.time.Instant
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlin.test.fail
import org.junit.Test

class GateTests {
    private fun remoteFixtures(): List<java.io.File> =
        Samples.fixtures.resolve("remote").listFiles { file -> file.extension == "json" }
            .orEmpty().sortedBy { it.name }

    @Test
    fun base64urlHasNoPadding() {
        val fbff = byteArrayOf(0xFB.toByte(), 0xFF.toByte())
        assertEquals("-_8", Base64URL.encode(fbff))
        assertContentEquals(fbff, Base64URL.decode("-_8"))
        // 宿主的 `remote::decode_base64url` 只认不带填充的规范写法。
        assertNull(Base64URL.decode("-_8="))
        assertNull(Base64URL.decode("-_9"))
        assertNull(Base64URL.decode("+/8"))
        assertNull(Base64URL.decode("a"))
        val bytes = ByteArray(32) { it.toByte() }
        assertContentEquals(bytes, Base64URL.decode(Base64URL.encode(bytes)))
        assertEquals(43, Base64URL.encode(bytes).length)
    }

    @Test
    fun signedBytesAreConcatenatedInOrder() {
        val nonce = ByteArray(32) { 0x11 }
        val exporter = ByteArray(32) { 0x22 }
        val auth = GateSignature.payload(nonce, exporter, GatePurpose.Auth)
        assertEquals(16 + 32 + 32 + 4, auth.size)
        assertContentEquals("runode-remote-v1".encodeToByteArray(), auth.copyOfRange(0, 16))
        assertContentEquals(nonce, auth.copyOfRange(16, 48))
        assertContentEquals(exporter, auth.copyOfRange(48, 80))
        assertContentEquals("auth".encodeToByteArray(), auth.copyOfRange(80, 84))
        val pair = GateSignature.payload(nonce, exporter, GatePurpose.Pair)
        assertContentEquals("pair".encodeToByteArray(), pair.copyOfRange(80, 84))
        assertEquals("EXPORTER-runode-remote", GateSignature.exporterLabel)
    }

    @Test
    fun challengeAndVerdictsDecode() {
        val nonce = ByteArray(32) { 7 }
        val challenge =
            """{"type":"remote_challenge","version":1,"nonce":"${Base64URL.encode(nonce)}","host_name":"Ethan 的 MacBook Pro"}"""
        assertEquals(
            GateMessage.Challenge(version = 1u, nonce = nonce, hostName = "Ethan 的 MacBook Pro"),
            decode(GateMessage.serializer(), challenge),
        )
        val accepted = """{"type":"remote_accepted","device_id":"00112233445566778899aabbccddeeff"}"""
        assertEquals(
            GateMessage.Accepted(deviceId = "00112233445566778899aabbccddeeff"),
            decode(GateMessage.serializer(), accepted),
        )
        for ((kind, reason) in listOf(
            "unknown_device" to GateRejection.UnknownDevice,
            "bad_signature" to GateRejection.BadSignature,
            "pairing_invalid" to GateRejection.PairingInvalid,
            "rate_limited" to GateRejection.RateLimited,
            "disabled" to GateRejection.Disabled,
            "solar_flare" to GateRejection.Unknown("solar_flare"),
        )) {
            val rejected = """{"type":"remote_rejected","reason":{"kind":"$kind"}}"""
            assertEquals(GateMessage.Rejected(reason), decode(GateMessage.serializer(), rejected))
        }
    }

    @Test
    fun repliesEncodeWithBase64url() {
        val auth = GateMessage.Auth(
            deviceId = "00112233445566778899aabbccddeeff",
            signature = byteArrayOf(0xFB.toByte(), 0xFF.toByte()),
        )
        val expected = """{"type":"remote_auth","device_id":"00112233445566778899aabbccddeeff","signature":"-_8"}"""
        assertTrue(sameJson(encodeToJson(GateMessage.serializer(), auth), Json.parseToJsonElement(expected)))
        val pair = GateMessage.Pair(
            secret = byteArrayOf(1),
            deviceName = "Ethan 的 iPhone",
            publicKey = byteArrayOf(2),
            signature = byteArrayOf(3),
        )
        val pairExpected =
            """{"type":"remote_pair","secret":"AQ","device_name":"Ethan 的 iPhone","public_key":"Ag","signature":"Aw"}"""
        assertTrue(sameJson(encodeToJson(GateMessage.serializer(), pair), Json.parseToJsonElement(pairExpected)))
    }

    /** 宿主的门禁消息样例（`remote_*.json`）：每个都能读；认识的读出来再写回去和原文一样，以后才有的拒绝原因读成 `Unknown`。 */
    @Test
    fun macGateMessagesRoundTrip() {
        val files = remoteFixtures().filter { it.name.startsWith("remote_") }
        assertTrue(files.isNotEmpty())
        for (file in files) {
            val json = Json.parseToJsonElement(file.readText())
            val message = decode(GateMessage.serializer(), json)
            when {
                message is GateMessage.Unknown -> fail("${file.name}: unknown gate message ${message.type}")
                message is GateMessage.Rejected && message.reason is GateRejection.Unknown ->
                    assertTrue(file.name.contains("future"), "${file.name}: ${message.reason}")
                else -> assertTrue(sameJson(encodeToJson(GateMessage.serializer(), message), json), file.name)
            }
        }
    }

    /** 宿主被签字节串的样例（`signed_bytes_*.json`）和这里拼出来的一样。 */
    @Test
    fun macSignedBytesMatch() {
        val files = remoteFixtures().filter { it.name.startsWith("signed_bytes_") }
        assertTrue(files.isNotEmpty())
        for (file in files) {
            val sample = Json.parseToJsonElement(file.readText()).jsonObject
            fun field(name: String) = sample.getValue(name).jsonPrimitive.content
            val purpose = assertNotNull(GatePurpose.fromRaw(field("purpose")), file.name)
            val payload = GateSignature.payload(
                nonce = assertNotNull(Base64URL.decode(field("nonce"))),
                exporter = assertNotNull(Base64URL.decode(field("exporter"))),
                purpose = purpose,
            )
            assertEquals(field("signed"), Base64URL.encode(payload), file.name)
        }
    }

    /** 宿主 Bonjour TXT 记录的样例（`bonjour_txt.json`）用的键和这里认的一样。 */
    @Test
    fun bonjourTxtKeysMatchTheMac() {
        val sample = Samples.json("remote", "bonjour_txt.json").jsonObject
        val txt = sample.getValue("txt").jsonObject
        assertEquals(gateVersion.toString(), txt[bonjourVersionKey]?.jsonPrimitive?.content)
        val fingerprint = assertNotNull(
            txt[bonjourFingerprintKey]?.jsonPrimitive?.content?.let { CertificateFingerprint(text = it) },
        )
        assertEquals(sample.getValue("fingerprint").jsonPrimitive.content, fingerprint.toString())
    }

    /** 宿主配对链接的样例（`pair_uri.json`）解出来的各项和它列的一样。 */
    @Test
    fun macPairingLinkParses() {
        val sample = Samples.json("remote", "pair_uri.json").jsonObject
        fun field(name: String) = sample.getValue(name).jsonPrimitive
        val invitation = PairingInvitation.parse(field("uri").content)
        assertEquals(field("host_name").content, invitation.hostName)
        assertEquals(field("fingerprint").content, invitation.fingerprint.toString())
        assertEquals(field("secret").content, Base64URL.encode(invitation.secret))
        assertEquals(field("port").int.toUShort(), invitation.port)
        assertEquals(sample.getValue("addrs").jsonArray.map { it.jsonPrimitive.content }, invitation.addresses)
        assertEquals(Instant.ofEpochSecond(field("expires_at").long), invitation.expiresAt)
        assertNotNull(invitation.node)
        assertEquals(field("node").content, invitation.node)
    }
}
