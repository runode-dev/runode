package dev.runode.mobile.features.model

import dev.runode.mobile.protocol.SessionLink
import java.io.File
import java.util.UUID
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

private val sampleMachine = UUID.fromString("6F9619FF-8B86-D011-B42D-00C04FC964FF")
private const val sampleSession = "0123456789abcdef0011223344556677"

/** 宿主 `push::fcm_message` 照规格写出的样例里的 `data`，FCM 交给 App 的就是它。 */
private fun sampleData(name: String): Map<String, String> {
    val repository = File(System.getProperty("runode.repo") ?: error("runode.repo is not set; run the tests through Gradle"))
    val text = repository.resolve("crates/protocol/tests/fixtures/push").resolve(name).readText()
    val data = Json.parseToJsonElement(text).jsonObject.getValue("message").jsonObject.getValue("data").jsonObject
    return data.mapValues { it.value.jsonPrimitive.content }
}

/** 电脑经 FCM 推来的数据拼成通知：标题按种类和 agent 定，副标题是会话标题和电脑名。 */
class FcmDataTests {
    @Test fun blockedAgentsAskForAnAnswer() {
        val message = assertNotNull(PushMessage.parse(sampleData("fcm_message.json")))
        assertEquals(SessionLink(sampleMachine, sampleSession), message.link)
        assertEquals(PushMessage.Kind.Blocked, message.kind)
        assertEquals("Claude Code is waiting for your answer", message.title.english)
        assertEquals("修 bug · Ethan 的 MacBook Pro", message.subtitle.english)
        assertEquals("Bash command\ncargo test\nDo you want to proceed?", message.body)
        assertEquals(PushChannel.Blocked, message.channel)
        assertEquals(sampleSession, message.tag)
        assertEquals("6F9619FF-8B86-D011-B42D-00C04FC964FF", message.group)
    }

    @Test fun finishedTasksHaveTheirOwnTitleAndChannel() {
        val message = assertNotNull(PushMessage.parse(sampleData("fcm_message_todo_done.json")))
        assertEquals(PushMessage.Kind.TodoDone, message.kind)
        assertEquals("Task finished", message.title.english)
        assertEquals("给设置页加暗色模式 · Ethan 的 MacBook Pro", message.subtitle.english)
        assertEquals("", message.body)
        assertEquals(PushChannel.Tasks, message.channel)
        val failed = assertNotNull(PushMessage.parse(sampleData("fcm_message_todo_done.json") + ("kind" to "todo_failed")))
        assertEquals("Task failed", failed.title.english)
    }

    @Test fun unknownAgentsAndBrokenDataAreHandled() {
        val data = sampleData("fcm_message.json")
        val unnamed = assertNotNull(PushMessage.parse(data + ("agent" to "")))
        assertEquals("A terminal is waiting for your answer", unnamed.title.english)
        // 比自己新的电脑才有的种类、认不出是哪个会话的：不出通知。
        assertNull(PushMessage.parse(data + ("kind" to "deploy_done")))
        assertNull(PushMessage.parse(data - "kind"))
        assertNull(PushMessage.parse(data + ("machine" to "not-a-uuid")))
        assertNull(PushMessage.parse(data - "session"))
    }
}
