package dev.runode.mobile.protocol

import java.time.Instant
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertTrue
import org.junit.Test

class PairingInvitationTests {
    private val fingerprint = ByteArray(32) { it.toByte() }
    private val secret = ByteArray(32) { 0xAB.toByte() }

    private fun link(
        v: String = "1",
        name: String = "Ethan%20%E7%9A%84%20MacBook%20Pro",
        fp: String = Base64URL.encode(fingerprint),
        secret: String = Base64URL.encode(this.secret),
        port: String = "7866",
        addr: String = "192.168.1.20,fd7a:115c:a1e0::1",
        exp: String = "1900000000",
    ): String = "runode://pair?v=$v&name=$name&fp=$fp&secret=$secret&port=$port&addr=$addr&exp=$exp"

    @Test
    fun parsesAFullLink() {
        val invitation = PairingInvitation.parse("  ${link()}\n")
        assertEquals("Ethan 的 MacBook Pro", invitation.hostName)
        assertContentEquals(fingerprint, invitation.fingerprint.bytes)
        assertContentEquals(secret, invitation.secret)
        assertEquals(7866.toUShort(), invitation.port)
        assertEquals(listOf("192.168.1.20", "fd7a:115c:a1e0::1"), invitation.addresses)
        assertEquals(Instant.ofEpochSecond(1_900_000_000), invitation.expiresAt)
        assertFalse(invitation.isExpired(at = Instant.ofEpochSecond(1_800_000_000)))
        assertTrue(invitation.isExpired(at = Instant.ofEpochSecond(1_900_000_000)))
    }

    @Test
    fun emptyAddressListIsAllowed() {
        assertTrue(PairingInvitation.parse(link(addr = "")).addresses.isEmpty())
    }

    /** 宿主的 `PairingUri` 读的时候参数的先后不限，不认识的参数忽略。 */
    @Test
    fun unknownParametersAreIgnored() {
        val invitation = PairingInvitation.parse(link() + "&future=1&node=abc")
        assertEquals("abc", invitation.node)
        assertEquals(7866.toUShort(), invitation.port)
    }

    @Test
    fun rejectsBadLinks() {
        fun error(text: String) = assertFailsWith<PairingInvitationError> { PairingInvitation.parse(text) }
        assertEquals(PairingInvitationError.NotAnInvitation, error("https://example.com"))
        assertEquals(PairingInvitationError.NotAnInvitation, error("runode://open?v=1"))
        assertEquals(PairingInvitationError.UnsupportedVersion(2), error(link(v = "2")))
        assertEquals(PairingInvitationError.Invalid("fp"), error(link(fp = "AAAA")))
        assertEquals(PairingInvitationError.Invalid("secret"), error(link(secret = "AQ")))
        assertEquals(PairingInvitationError.Invalid("port"), error(link(port = "70000")))
        assertEquals(PairingInvitationError.Missing("exp"), error(link().replace("&exp=1900000000", "")))
    }
}
