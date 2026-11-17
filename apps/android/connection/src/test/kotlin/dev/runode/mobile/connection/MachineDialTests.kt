package dev.runode.mobile.connection

import dev.runode.mobile.protocol.CertificateFingerprint
import java.util.Collections
import kotlin.test.assertEquals
import kotlin.test.assertTrue
import kotlin.time.Duration
import kotlin.time.Duration.Companion.milliseconds
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.junit.Test

/** 几条路同时连：选出一条后其余的连上了也关掉，调用方取消时连已经选出的那条也关掉。 */
class MachineDialTests {
    private val machine = sampleMachine()
    private val keys = MemoryDeviceKeyStore().also { it.generate(machine.id) }

    /** 已经有一条连上、还在等别的几条收尾时被取消：连上的那条交给 `close`，不留着没人读。 */
    @Test
    fun cancellingAfterAWinnerClosesIt() = test { scope ->
        val bonjourStarted = CompletableDeferred<Unit>()
        val bonjourRelease = CompletableDeferred<Unit>()
        // Bonjour 那条不理会取消，拖住收尾。
        val slow = object : HostDiscovery {
            override suspend fun locate(fingerprint: CertificateFingerprint, timeout: Duration): TransportTarget? {
                bonjourStarted.complete(Unit)
                withContext(NonCancellable) { bonjourRelease.await() }
                return null
            }
        }
        val opened = Collections.synchronizedList(mutableListOf<FakeTransport>())
        val closed = Collections.synchronizedList(mutableListOf<FrameTransport>())
        val dialing = scope.launch {
            dial<FrameTransport>(
                machine,
                keys,
                slow,
                open = { _, _ -> FakeTransport().also { opened += it } },
                tunnel = null,
                handshake = { transport, _ -> transport },
                close = { closed += it },
            )
        }
        bonjourStarted.await()
        while (opened.isEmpty()) delay(5.milliseconds)
        // 记着的地址那条握完了手，选它当赢家。
        delay(100.milliseconds)
        dialing.cancel()
        bonjourRelease.complete(Unit)
        dialing.join()
        assertEquals(1, opened.size)
        assertTrue(closed.contains(opened.single()), "closed $closed")
    }
}
