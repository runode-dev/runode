package dev.runode.mobile.features.model

import android.graphics.ImageFormat
import android.media.Image
import android.media.ImageReader
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.media.MediaFormat
import android.os.Handler
import android.os.HandlerThread
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.runode.mobile.protocol.Codec
import dev.runode.mobile.protocol.VideoFrame
import java.io.ByteArrayOutputStream
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue
import org.junit.After
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith

/**
 * 真的 MediaCodec 解码、画到 [ImageReader] 的 surface 上。码流在测试里用 MediaCodec 的编码器现编，按电脑那边的格式拆成
 * [VideoFrame]：参数集单独放，帧的数据是长度前缀的 NAL。
 */
@RunWith(AndroidJUnit4::class)
class VideoDecoderTests {
    private val keyframeRequests = AtomicInteger()
    private val screens = mutableListOf<Screen>()
    private val decoders = mutableListOf<VideoDecoder>()

    @After fun tearDown() {
        decoders.forEach(VideoDecoder::release)
        screens.forEach(Screen::close)
    }

    /** 画面画到的地方，记下画上来的每一帧的尺寸。 */
    private class Screen(width: Int, height: Int) {
        private val thread = HandlerThread("test-image-reader").apply { start() }
        val reader: ImageReader = ImageReader.newInstance(width, height, ImageFormat.PRIVATE, 4)
        val sizes = LinkedBlockingQueue<Pair<Int, Int>>()

        init {
            reader.setOnImageAvailableListener(
                { reader -> reader.acquireLatestImage()?.use { sizes += it.width to it.height } },
                Handler(thread.looper),
            )
        }

        /** 等到画上来一帧 [size] 的画面，等不到时为假。 */
        fun awaitFrame(size: Pair<Int, Int>, seconds: Long = 5): Boolean {
            val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(seconds)
            while (true) {
                val left = deadline - System.nanoTime()
                if (left <= 0) return false
                if (sizes.poll(left, TimeUnit.NANOSECONDS) == size) return true
            }
        }

        fun close() {
            reader.close()
            thread.quitSafely()
        }
    }

    private fun screen(width: Int, height: Int) = Screen(width, height).also { screens += it }

    private fun decoder(screen: Screen): VideoDecoder =
        MediaCodecVideoDecoder(screen.reader.surface) { keyframeRequests.incrementAndGet() }.also { decoders += it }

    private fun hasDecoder(codec: Codec): Boolean = MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos
        .any { info -> !info.isEncoder && info.supportedTypes.any { it.equals(codec.mime, ignoreCase = true) } }

    /** 解码、画出关键帧和之后的帧，尺寸对。 */
    private fun decodesAndDraws(codec: Codec) {
        val frames = encode(codec, 320, 240, count = 3)
        assumeTrue("no ${codec.mime} encoder on this device", frames != null)
        assumeTrue("no ${codec.mime} decoder on this device", hasDecoder(codec))
        val screen = screen(320, 240)
        val decoder = decoder(screen)
        decoder.reset(320, 240)
        frames!!.forEach(decoder::decode)
        assertTrue(screen.awaitFrame(320 to 240), "no frame was drawn")
        assertEquals(0, keyframeRequests.get())
    }

    @Test fun decodesH264() = decodesAndDraws(Codec.H264)

    /** 模拟器镜像里有 HEVC 的软件编解码器；真机上没有 HEVC 编码器时跳过。 */
    @Test fun decodesHevc() = decodesAndDraws(Codec.Hevc)

    /** 关键帧之前的帧解不出来：要一次关键帧，等到之前不再要；关键帧到了接着画。 */
    @Test fun framesBeforeTheKeyframeAskForOneOnce() {
        val frames = assertNotNullFrames(encode(Codec.H264, 320, 240, count = 3))
        val screen = screen(320, 240)
        val decoder = decoder(screen)
        decoder.decode(frames[1])
        decoder.decode(frames[2])
        assertTrue(waitFor { keyframeRequests.get() == 1 })
        Thread.sleep(200)
        assertEquals(1, keyframeRequests.get())
        frames.forEach(decoder::decode)
        assertTrue(screen.awaitFrame(320 to 240), "no frame was drawn after the keyframe")
        assertEquals(1, keyframeRequests.get())
    }

    /** 关键帧带来新的参数集（电脑换了尺寸）时重配解码器，按新尺寸画。 */
    @Test fun newParameterSetsReconfigureTheDecoder() {
        val small = assertNotNullFrames(encode(Codec.H264, 320, 240, count = 2))
        val large = assertNotNullFrames(encode(Codec.H264, 640, 480, count = 2))
        val screen = screen(640, 480)
        val decoder = decoder(screen)
        decoder.reset(320, 240)
        small.forEach(decoder::decode)
        assertTrue(screen.awaitFrame(320 to 240), "no small frame was drawn")
        large.forEach(decoder::decode)
        assertTrue(screen.awaitFrame(640 to 480), "no large frame was drawn after the parameter sets changed")
        assertEquals(0, keyframeRequests.get())
    }

    /** 参数集坏了的关键帧解不出东西，下一个好的关键帧换上新的参数集，照样画。 */
    @Test fun aGoodKeyframeRecoversFromBrokenParameterSets() {
        val frames = assertNotNullFrames(encode(Codec.H264, 320, 240, count = 2))
        val screen = screen(320, 240)
        val decoder = decoder(screen)
        val broken = VideoFrame(
            data = byteArrayOf(0, 0, 0, 2, 0x65, 0x88.toByte()),
            keyframe = true,
            parameterSets = listOf(byteArrayOf(0x67, 0x64), byteArrayOf(0x68, 0xee.toByte())),
            ptsMicros = 7,
            codec = Codec.H264,
        )
        decoder.decode(broken)
        frames.forEach(decoder::decode)
        assertTrue(screen.awaitFrame(320 to 240), "no frame was drawn after the good keyframe")
    }

    /** 报给电脑的编码都有解码器，不重复；有硬件解码器的排前面。 */
    @Test fun reportsTheCodecsThisDeviceDecodes() {
        val codecs = MediaCodecSupport.decodable(1920, 1080)
        assertTrue(codecs.isNotEmpty())
        assertEquals(codecs.distinct(), codecs)
        assertTrue(codecs.all(::hasDecoder))
        assertEquals(codecs, MediaCodecSupport.decodable(1920, 1080))
    }

    /** 放开以后不再画，再交来的帧不理。 */
    @Test fun releasingStopsDrawing() {
        val frames = assertNotNullFrames(encode(Codec.H264, 320, 240, count = 3))
        val screen = screen(320, 240)
        val decoder = decoder(screen)
        frames.forEach(decoder::decode)
        assertTrue(screen.awaitFrame(320 to 240))
        decoder.release()
        screen.sizes.clear()
        frames.forEach(decoder::decode)
        decoder.reset(320, 240)
        assertNull(screen.sizes.poll(500, TimeUnit.MILLISECONDS))
        assertEquals(0, keyframeRequests.get())
    }

    private fun assertNotNullFrames(frames: List<VideoFrame>?): List<VideoFrame> {
        assumeTrue("no H.264 encoder on this device", frames != null)
        return frames!!
    }

    private fun waitFor(seconds: Long = 5, condition: () -> Boolean): Boolean {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(seconds)
        while (System.nanoTime() < deadline) {
            if (condition()) return true
            Thread.sleep(10)
        }
        return condition()
    }
}

/**
 * 用 MediaCodec 的编码器编 [count] 帧 [width]×[height] 的画面，第一帧是关键帧，之后都不是；没有这种编码器时为空。
 */
private fun encode(codec: Codec, width: Int, height: Int, count: Int): List<VideoFrame>? {
    val encoder = try {
        MediaCodec.createEncoderByType(codec.mime)
    } catch (_: Exception) {
        return null
    }
    try {
        val format = MediaFormat.createVideoFormat(codec.mime, width, height).apply {
            setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible)
            setInteger(MediaFormat.KEY_BIT_RATE, 1_000_000)
            setInteger(MediaFormat.KEY_FRAME_RATE, 30)
            setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 60)
        }
        encoder.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
        encoder.start()
        var parameterSets = emptyList<ByteArray>()
        val frames = mutableListOf<VideoFrame>()
        val info = MediaCodec.BufferInfo()
        var queued = 0
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(10)
        while (System.nanoTime() < deadline) {
            if (queued <= count) {
                val index = encoder.dequeueInputBuffer(10_000)
                if (index >= 0) {
                    val pts = queued * 33_333L
                    if (queued == count) {
                        encoder.queueInputBuffer(index, 0, 0, pts, MediaCodec.BUFFER_FLAG_END_OF_STREAM)
                    } else {
                        fill(encoder.getInputImage(index)!!, width, height, queued)
                        encoder.queueInputBuffer(index, 0, width * height * 3 / 2, pts, 0)
                    }
                    queued += 1
                }
            }
            val index = encoder.dequeueOutputBuffer(info, 10_000)
            if (index < 0) continue
            val buffer = encoder.getOutputBuffer(index)!!
            val bytes = ByteArray(info.size)
            buffer.position(info.offset)
            buffer.get(bytes)
            encoder.releaseOutputBuffer(index, false)
            val units = nalUnits(bytes)
            if (info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0) {
                parameterSets = units
            } else if (units.isNotEmpty()) {
                val keyframe = info.flags and MediaCodec.BUFFER_FLAG_KEY_FRAME != 0
                val slices = units.filterNot { isParameterSet(codec, it) }
                frames += VideoFrame(
                    data = lengthPrefixed(slices),
                    keyframe = keyframe,
                    parameterSets = if (keyframe) parameterSets else emptyList(),
                    ptsMicros = info.presentationTimeUs,
                    codec = codec,
                )
            }
            if (info.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0) break
        }
        check(frames.size == count && frames.first().keyframe && frames.first().parameterSets.isNotEmpty()) {
            "the encoder gave ${frames.size} frames"
        }
        return frames
    } finally {
        encoder.stop()
        encoder.release()
    }
}

/** 亮度是随帧挪动的斜纹，色度是灰的。 */
private fun fill(image: Image, width: Int, height: Int, index: Int) {
    for ((plane, data) in image.planes.withIndex()) {
        val w = if (plane == 0) width else width / 2
        val h = if (plane == 0) height else height / 2
        val buffer = data.buffer
        for (y in 0 until h) {
            for (x in 0 until w) {
                val value = if (plane == 0) (x + y + index * 8) and 0xff else 128
                buffer.put(y * data.rowStride + x * data.pixelStride, value.toByte())
            }
        }
    }
}

private fun isParameterSet(codec: Codec, unit: ByteArray): Boolean = when (codec) {
    Codec.H264 -> (unit[0].toInt() and 0x1f) in 7..8
    Codec.Hevc -> ((unit[0].toInt() shr 1) and 0x3f) in 32..34
}

/** 按起始码（`00 00 01`、`00 00 00 01`）切开的 NAL 单元，不带起始码。 */
private fun nalUnits(stream: ByteArray): List<ByteArray> {
    val starts = mutableListOf<Pair<Int, Int>>()
    var i = 0
    while (i + 3 <= stream.size) {
        if (stream[i] == 0.toByte() && stream[i + 1] == 0.toByte() && stream[i + 2] == 1.toByte()) {
            starts += i to i + 3
            i += 3
        } else {
            i += 1
        }
    }
    return starts.mapIndexedNotNull { n, (_, payload) ->
        var end = if (n + 1 < starts.size) starts[n + 1].first else stream.size
        // 四字节起始码多出的那个 0 不算前一个单元的。
        while (end > payload && n + 1 < starts.size && stream[end - 1] == 0.toByte()) end -= 1
        stream.copyOfRange(payload, end).takeIf { it.isNotEmpty() }
    }
}

/** 每个单元前面加 4 字节大端长度。 */
private fun lengthPrefixed(units: List<ByteArray>): ByteArray {
    val out = ByteArrayOutputStream()
    for (unit in units) {
        out.write(unit.size ushr 24)
        out.write(unit.size ushr 16)
        out.write(unit.size ushr 8)
        out.write(unit.size)
        out.write(unit)
    }
    return out.toByteArray()
}
