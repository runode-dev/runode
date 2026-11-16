package dev.runode.mobile.protocol

import kotlin.test.assertEquals
import kotlin.test.assertTrue
import org.junit.Test

/** 宿主 `runode_protocol::media` 的样例，两边的测试读同一批文件。 */
class MediaTests {
    @Test
    fun clientMessagesMatchTheSamples() {
        val cases = listOf(
            "media_hello.json" to MediaClientMsg.MediaHello(version = mediaVersion),
            "media.json" to MediaClientMsg.Media(len = 245_760u),
        )
        for ((name, message) in cases) {
            assertTrue(sameJson(encodeToJson(MediaClientMsg.serializer(), message), Samples.json("media", name)), name)
        }
    }

    @Test
    fun hostMessagesMatchTheSamples() {
        val cases = listOf(
            "ready.json" to MediaHostMsg.Ready,
            "incompatible.json" to MediaHostMsg.Incompatible(version = 1u),
            "saved.json" to MediaHostMsg.Saved(path = "/Users/ethan/Library/Caches/runode/images/截图 1.png"),
            "failed.json" to MediaHostMsg.Failed(reason = "the file is larger than 200 MiB"),
        )
        for ((name, message) in cases) {
            assertEquals(message, decode(MediaHostMsg.serializer(), Samples.json("media", name)), name)
        }
    }

    @Test
    fun unknownHostMessagesAreIgnored() {
        assertEquals(MediaHostMsg.Unknown, decode(MediaHostMsg.serializer(), """{"type":"progress","done":3}"""))
    }

    @Test
    fun mediaFramesHaveKindSix() {
        assertEquals(6.toUByte(), FrameKind.Media.rawValue)
        assertEquals(FrameKind.Media, FrameKind.fromRaw(6u))
    }

    @Test
    fun theLimitsMatchTheHost() {
        assertEquals(200 * 1024 * 1024, maxMediaBytes)
        assertEquals(1024 * 1024, maxMediaFrame)
    }
}
