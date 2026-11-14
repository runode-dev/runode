package dev.runode.mobile.connection

import dev.runode.mobile.protocol.Base64URL
import dev.runode.mobile.protocol.GateMessage
import dev.runode.mobile.protocol.GateRejection
import dev.runode.mobile.protocol.PairingInvitation
import java.time.Instant
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlinx.coroutines.async
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Test

/** 配对：配好了却不要的、没配成的，为它存的私钥删掉。 */
class PairingTests {
    private val keys = RecordingKeys()

    /** 记下为哪些电脑生成过私钥，好查还剩哪些。 */
    private class RecordingKeys(private val inner: MemoryDeviceKeyStore = MemoryDeviceKeyStore()) : DeviceKeyStore by inner {
        private val generated = mutableListOf<java.util.UUID>()

        override fun generate(machine: java.util.UUID): DeviceKey = inner.generate(machine).also {
            synchronized(generated) { generated += machine }
        }

        fun isEmpty(): Boolean = synchronized(generated) { generated.none { inner.key(it) != null } }
    }
    private val supply = TransportSupply()

    private fun invitation(expiresAt: Instant = Instant.now().plusSeconds(300)) = PairingInvitation(
        hostName = "Ethan 的 MacBook Pro",
        fingerprint = sampleMachine().fingerprint,
        secret = ByteArray(32) { 7 },
        port = 7866u,
        addresses = listOf("192.168.1.20"),
        expiresAt = expiresAt,
    )

    @Test
    fun discardingAPairingDeletesItsKey() {
        val machine = sampleMachine()
        val other = sampleMachine()
        keys.generate(machine.id)
        keys.generate(other.id)
        RemotePairing(keys, NoDiscovery).discard(machine)
        assertNull(keys.key(machine.id))
        assertNotNull(keys.key(other.id), "other machines keep their keys")
    }

    @Test
    fun pairingSendsTheSecretAndKeepsTheKey() = test { scope ->
        val transport = FakeTransport()
        supply.add(transport)
        val host = FakeHost(transport)
        val pairing = RemotePairing(keys, NoDiscovery, open = supply.opener)
        val paired = scope.async { pairing.pair(invitation(), "测试 Pixel") }
        host.challenge(node = "abc")
        val pair = assertNotNull(host.readControl())
        assertEquals("remote_pair", pair["type"]?.jsonPrimitive?.content)
        assertEquals(Base64URL.encode(ByteArray(32) { 7 }), pair["secret"]?.jsonPrimitive?.content)
        host.send(GateMessage.Accepted("ffeeddccbbaa99887766554433221100"))
        val record = paired.await()
        assertEquals("ffeeddccbbaa99887766554433221100", record.deviceId)
        assertEquals("192.168.1.20", record.lastAddress)
        assertEquals("abc", record.node)
        val key = assertNotNull(keys.key(record.id))
        assertEquals(Base64URL.encode(key.publicKeyX963), pair["public_key"]?.jsonPrimitive?.content)
    }

    @Test
    fun aRefusedPairingLeavesNoKey() = test { scope ->
        val transport = FakeTransport()
        supply.add(transport)
        val host = FakeHost(transport)
        val pairing = RemotePairing(keys, NoDiscovery, open = supply.opener)
        val paired = scope.async { runCatching { pairing.pair(invitation(), "测试 Pixel") } }
        host.challenge()
        host.readControl()
        host.send(GateMessage.Rejected(GateRejection.PairingInvalid))
        assertEquals(LinkFailure.Rejected(GateRejection.PairingInvalid), paired.await().exceptionOrNull())
        assertTrue(keys.isEmpty())
    }

    @Test
    fun anExpiredInvitationIsNotTried() = test {
        val pairing = RemotePairing(keys, NoDiscovery, open = supply.opener)
        val result = runCatching { pairing.pair(invitation(expiresAt = Instant.now().minusSeconds(1)), "测试 Pixel") }
        assertEquals(LinkFailure.InvitationExpired, result.exceptionOrNull())
        assertTrue(supply.requestedTargets.isEmpty())
        assertTrue(keys.isEmpty())
    }
}
