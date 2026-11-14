package dev.runode.mobile.connection

import java.net.URI
import kotlin.test.assertEquals
import kotlin.test.assertNull
import org.junit.Test

class PushRelayTests {
    @Test
    fun baseComesFromTheBuildConfiguration() {
        assertEquals(URI("https://relay.test:8443"), HttpPushRelay.configured(" https://relay.test:8443 ")?.base)
        // 没配、不是带主机的 https：没有中转。
        for (raw in listOf(null, "", "http://relay.test", "https://:8443")) {
            assertNull(HttpPushRelay.configured(raw), "$raw")
        }
    }
}
