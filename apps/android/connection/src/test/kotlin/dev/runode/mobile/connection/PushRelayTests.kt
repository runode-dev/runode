package dev.runode.mobile.connection

import java.net.URI
import kotlin.test.assertEquals
import org.junit.Test

class PushRelayTests {
    @Test
    fun baseComesFromTheBuildConfiguration() {
        assertEquals(URI("https://relay.test:8443"), HttpPushRelay.configured(" https://relay.test:8443 ").base)
        // 没配、占位没换掉、不是 https：用官方的。
        for (raw in listOf(null, "\${RUNODE_PUSH_RELAY}", "", "http://relay.test")) {
            assertEquals(HttpPushRelay.official.base, HttpPushRelay.configured(raw).base, "$raw")
        }
    }
}
