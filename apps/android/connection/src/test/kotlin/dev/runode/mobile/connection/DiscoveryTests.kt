package dev.runode.mobile.connection

import dev.runode.mobile.protocol.CertificateFingerprint
import kotlin.test.assertTrue
import kotlin.time.Duration
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.launch
import org.junit.Test

/** 等电脑在局域网里公布服务。 */
class DiscoveryTests {
    /** 每次只找一小段：找的时候拿着 Wi-Fi 的 MulticastLock，电脑一直不回来时不能一直拿着。 */
    @Test
    fun awaitingPresenceSearchesInShortSpans() = test { scope ->
        val firstTimeout = CompletableDeferred<Duration>()
        val discovery = object : HostDiscovery {
            override suspend fun locate(fingerprint: CertificateFingerprint, timeout: Duration): TransportTarget? {
                firstTimeout.complete(timeout)
                return null
            }
        }
        val waiting = scope.launch { discovery.awaitPresence(sampleMachine().fingerprint) }
        val timeout = firstTimeout.await()
        waiting.cancel()
        assertTrue(timeout <= presenceSearch, "searched for $timeout")
    }
}
