package dev.runode.mobile.protocol

import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull
import org.junit.Test

class FrameTests {
    @Test
    fun encodesTheFrozenHeader() {
        val frame = Frame(FrameKind.Input, 0x0102_0304u, byteArrayOf(0xAA.toByte(), 0xBB.toByte()))
        // u32 LE 长度 | u8 类型 | u32 LE 通道 | 载荷
        assertContentEquals(
            byteArrayOf(2, 0, 0, 0, 1, 4, 3, 2, 1, 0xAA.toByte(), 0xBB.toByte()),
            frame.encoded(),
        )
    }

    @Test
    fun roundTripsSeveralFramesInOneChunk() {
        val frames = listOf(
            Frame(FrameKind.Control, 0u, """{"type":"list_sessions"}""".encodeToByteArray()),
            Frame(FrameKind.Output, 7u, ByteArray(1000) { 0x41 }),
            Frame(FrameKind.Snapshot, 7u, ByteArray(0)),
        )
        val decoder = FrameDecoder()
        decoder.append(frames.fold(ByteArray(0)) { acc, frame -> acc + frame.encoded() })
        val decoded = generateSequence { decoder.next() }.toList()
        assertEquals(frames, decoded)
        decoder.finish()
    }

    @Test
    fun waitsForTheRestOfAHalfFrame() {
        val bytes = Frame(FrameKind.Output, 3u, "hello".encodeToByteArray()).encoded()
        val decoder = FrameDecoder()
        // 帧头只到了一半。
        decoder.append(bytes.copyOfRange(0, 4))
        assertNull(decoder.next())
        // 帧头齐了，载荷还差。
        decoder.append(bytes.copyOfRange(4, 11))
        assertNull(decoder.next())
        decoder.append(bytes.copyOfRange(11, bytes.size))
        assertContentEquals("hello".encodeToByteArray(), decoder.next()?.payload)
        assertNull(decoder.next())
    }

    @Test
    fun aConnectionClosedMidFrameIsTruncated() {
        val bytes = Frame(FrameKind.Output, 3u, "hello".encodeToByteArray()).encoded()
        val decoder = FrameDecoder()
        decoder.append(bytes.copyOfRange(0, 12))
        assertNull(decoder.next())
        assertEquals(FrameError.Truncated, assertFailsWith<FrameError> { decoder.finish() })
    }

    @Test
    fun rejectsOverlongFramesBeforeTheirPayloadArrives() {
        val length = Frame.maxPayload + 1u
        val header = ByteArray(9)
        for (i in 0 until 4) header[i] = (length shr (8 * i)).toByte()
        val decoder = FrameDecoder()
        decoder.append(header)
        assertEquals(FrameError.TooLong(length.toULong()), assertFailsWith<FrameError> { decoder.next() })
    }

    @Test
    fun gatePhaseHasA16KiBLimit() {
        val frame = Frame(FrameKind.Control, 0u, ByteArray(16 * 1024 + 1) { 0x20 })
        val decoder = FrameDecoder(maxPayload = Frame.gateMaxPayload)
        decoder.append(frame.encoded())
        assertEquals(FrameError.TooLong(16uL * 1024uL + 1uL), assertFailsWith<FrameError> { decoder.next() })
    }

    @Test
    fun rejectsUnknownKinds() {
        val decoder = FrameDecoder()
        decoder.append(byteArrayOf(0, 0, 0, 0, 9, 0, 0, 0, 0))
        assertEquals(FrameError.UnknownKind(9u), assertFailsWith<FrameError> { decoder.next() })
    }

    @Test
    fun refusesToEncodeOverlongPayloads() {
        val frame = Frame(FrameKind.Output, 1u, ByteArray(Frame.maxPayload.toInt() + 1))
        assertEquals(
            FrameError.TooLong(Frame.maxPayload.toULong() + 1uL),
            assertFailsWith<FrameError> { frame.encoded() },
        )
    }
}
