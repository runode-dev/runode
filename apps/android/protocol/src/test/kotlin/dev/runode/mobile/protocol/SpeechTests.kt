package dev.runode.mobile.protocol

import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertTrue
import org.junit.Test

/** 宿主 `runode_protocol::speech` 的样例，两边的测试读同一批文件。 */
class SpeechTests {
    @Test
    fun clientMessagesMatchTheSamples() {
        val cases = listOf(
            "speech_hello.json" to SpeechClientMsg.SpeechHello(version = speechVersion),
            "finish.json" to SpeechClientMsg.Finish,
        )
        for ((name, message) in cases) {
            assertTrue(sameJson(encodeToJson(SpeechClientMsg.serializer(), message), Samples.json("speech", name)), name)
        }
    }

    @Test
    fun hostMessagesMatchTheSamples() {
        val cases = listOf(
            "ready.json" to SpeechHostMsg.Ready(installed = true),
            "incompatible.json" to SpeechHostMsg.Incompatible(version = 1u),
            "text.json" to SpeechHostMsg.Text("打开 README.md 看看"),
            "failed.json" to SpeechHostMsg.Failed(reason = "no speech model downloaded"),
        )
        for ((name, message) in cases) {
            assertEquals(message, decode(SpeechHostMsg.serializer(), Samples.json("speech", name)), name)
        }
    }

    @Test
    fun audioFramesHaveKindFive() {
        assertEquals(5.toUByte(), FrameKind.Audio.rawValue)
    }

    /** 录音编成 16 位小端 PCM，超出 -1 到 1 的截到两头。 */
    @Test
    fun audioIsLittleEndianPCM() {
        assertContentEquals(
            byteArrayOf(0, 0, 0xFF.toByte(), 0x7F, 0x01, 0x80.toByte(), 0xFF.toByte(), 0x7F),
            speechAudioPayload(floatArrayOf(0f, 1f, -1f, 2f)),
        )
    }
}
