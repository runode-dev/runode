package dev.runode.mobile.protocol

import java.net.URI
import java.util.UUID
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import org.junit.Test

private val sampleMachine = UUID.fromString("6F9619FF-8B86-D011-B42D-00C04FC964FF")
private const val sampleSession = "0123456789abcdef0011223344556677"

/** 推送数据交给 App 时的样子：对象是 Map，字符串是 String。 */
private fun JsonElement.toUserInfo(): Any? = when (this) {
    is JsonObject -> mapValues { it.value.toUserInfo() }
    is JsonArray -> map { it.toUserInfo() }
    JsonNull -> null
    is JsonPrimitive -> content
}

/** 宿主 `runode_protocol::push` 照规格写出的推送样例：电脑推来的等回答、任务做完的通知。 */
class PushTests {
    private val notifications =
        listOf("apns_notify.json", "apns_notify_unnamed.json", "apns_todo_done.json", "apns_todo_failed.json")

    /** 点开通知时按 `aps` 外面的 `machine`、`session` 打开那个会话；同一台电脑的通知按 `thread-id` 归在一组。 */
    @Test
    fun notificationsCarryTheSession() {
        for (name in notifications) {
            val payload = Samples.json("push", name).toUserInfo() as Map<*, *>
            assertEquals(SessionLink(sampleMachine, sampleSession), SessionLink(payload), name)
            val aps = payload["aps"] as Map<*, *>
            assertEquals(sampleMachine.toString().uppercase(), aps["thread-id"], name)
        }
    }

    /** 官方 App 向中转服务换句柄的请求体和回话。 */
    @Test
    fun relayRegistrationMatchesTheSamples() {
        val register = RelayRegister(
            env = ApnsEnv.Production,
            token = "80f0c8b3a4e2d1c0ffeeddccbbaa99887766554433221100aabbccddeeff0011",
            machine = sampleMachine.toString().uppercase(),
        )
        assertTrue(sameJson(encodeToJson(RelayRegister.serializer(), register), Samples.json("push", "relay_register.json")))
        assertEquals(Samples.text("push", "relay_register.json").trimEnd('\n'), protocolJson.encodeToString(RelayRegister.serializer(), register))
        assertEquals(register, decode(RelayRegister.serializer(), Samples.text("push", "relay_register.json")))
        val handle = decode(RelayHandle.serializer(), Samples.json("push", "relay_handle.json"))
        assertEquals("q2b9Xk0Lr7mVt3pZw8sYc1nH4dF6gJ-aE5uR_oTiBlM", handle.handle)
        val error = decode(PushError.serializer(), Samples.json("push", "push_error.json"))
        assertEquals("BadDeviceToken", error.reason)
    }

    /** Android 换句柄时多带 `service`，键的先后和宿主写的一样，逐字节对得上。 */
    @Test
    fun fcmRelayRegistrationMatchesTheSample() {
        val register = RelayRegister(
            env = ApnsEnv.Production,
            token = "fGx3Q9kVTm2b7WcLr0pYnA:APA91bE7tq2Kc0VhS8xJm4RzL1wPdN6uYf3Hg5oBk9aTe2Ci7Xv0Mn4Qs8Dj1Ur6Wy3Pl5Zh2Ko9Ib7Ga4Fc0Ex_-",
            machine = sampleMachine.toString().uppercase(),
            service = PushService.Fcm,
        )
        val text = Samples.text("push", "relay_register_fcm.json").trimEnd('\n')
        assertEquals(text, protocolJson.encodeToString(RelayRegister.serializer(), register))
        assertEquals(register, decode(RelayRegister.serializer(), text))
    }

    @Test
    fun otherNotificationsHaveNoSession() {
        assertNull(SessionLink(emptyMap<String, Any>()))
        assertNull(SessionLink(mapOf("machine" to "not-a-uuid", "session" to sampleSession)))
        assertNull(SessionLink(mapOf("machine" to sampleMachine.toString(), "session" to "")))
        assertNull(SessionLink(mapOf("machine" to sampleMachine.toString())))
    }
}

class SessionLinkTests {
    @Test
    fun deepLinkRoundTrips() {
        val link = SessionLink(sampleMachine, sampleSession)
        val url = assertNotNull(link.url)
        assertEquals(
            "runode://session?machine=6F9619FF-8B86-D011-B42D-00C04FC964FF&session=0123456789abcdef0011223344556677",
            url.toString(),
        )
        assertEquals(link, SessionLink(url))
    }

    @Test
    fun otherLinksAreNotSessionLinks() {
        for (text in listOf(
            "runode://pair?v=1&name=MacBook",
            "runode://session?session=0123456789abcdef0011223344556677",
            "runode://session?machine=not-a-uuid&session=0123456789abcdef0011223344556677",
            "runode://session?machine=6F9619FF-8B86-D011-B42D-00C04FC964FF&session=",
            "https://session?machine=6F9619FF-8B86-D011-B42D-00C04FC964FF&session=00",
        )) {
            assertNull(SessionLink(URI(text)), text)
        }
    }

    @Test
    fun hostAndSchemeIgnoreCase() {
        val link = assertNotNull(
            SessionLink(URI("RUNODE://Session?machine=6f9619ff-8b86-d011-b42d-00c04fc964ff&session=abc")),
        )
        assertEquals(sampleMachine, link.machine)
        assertEquals("abc", link.session)
    }
}
