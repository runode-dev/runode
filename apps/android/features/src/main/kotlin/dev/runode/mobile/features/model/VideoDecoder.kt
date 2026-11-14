package dev.runode.mobile.features.model

import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.media.MediaFormat
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.view.Surface
import dev.runode.mobile.protocol.Codec
import dev.runode.mobile.protocol.VideoFrame
import java.nio.ByteBuffer
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

/** 把远程桌面的画面解出来、画到屏幕上。 */
interface VideoDecoder {
    /** 电脑重新开始采集了（`Started`），画面是 [width]×[height]：丢掉旧格式，新的第一帧就是关键帧，不是的话照常要一个。 */
    fun reset(width: Int, height: Int)

    /** 解一帧。还没收到关键帧、或者解码出错时丢掉，向电脑要一个关键帧，等到关键帧之前不再要。 */
    fun decode(frame: VideoFrame)

    /** 停掉、放开解码器；返回时已经不再往画面上画。之后的调用什么都不做。 */
    fun release()
}

/** 画面画到哪里。[ScreenModel] 每次（重新）连上时用它建一个解码器，[onNeedKeyframe] 是向电脑要关键帧，可以在任意线程上调。 */
fun interface VideoOutput {
    fun decoder(onNeedKeyframe: () -> Unit): VideoDecoder
}

/** 画到 [surface] 上（SurfaceView 的）；surface 销毁前要先 [ScreenModel.detachOutput]。 */
class SurfaceVideoOutput(private val surface: Surface) : VideoOutput {
    override fun decoder(onNeedKeyframe: () -> Unit): VideoDecoder = MediaCodecVideoDecoder(surface, onNeedKeyframe)
}

/** 这部手机解得了哪些编码，告诉电脑（`Start` 的 `codecs`）。 */
fun interface VideoCodecSupport {
    /** 解得了 [width]×[height] 的编码，想要的排前面；一个都没有时为空，电脑当成只有 HEVC。 */
    fun decodable(width: Int, height: Int): List<Codec>
}

/** 按 [MediaCodecList] 里实际有的解码器报：有硬件解码器的排前面，同样有时 HEVC 排前面；尺寸都太大时退回只看有没有。 */
object MediaCodecSupport : VideoCodecSupport {
    private val decoders: List<MediaCodecInfo> by lazy {
        MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos.filterNot { it.isEncoder }
    }

    override fun decodable(width: Int, height: Int): List<Codec> {
        fun rank(codec: Codec, checkSize: Boolean): Int? = decoders
            .filter { info -> info.supportedTypes.any { it.equals(codec.mime, ignoreCase = true) } }
            .filter { info ->
                !checkSize || runCatching {
                    info.getCapabilitiesForType(codec.mime).videoCapabilities?.isSizeSupported(width, height)
                }.getOrNull() ?: false
            }
            .minOfOrNull { if (it.isHardwareAccelerated) 0 else 1 }
        val preference = listOf(Codec.Hevc, Codec.H264)
        val sized = preference.mapNotNull { codec -> rank(codec, checkSize = true)?.let { codec to it } }
        val ranked = sized.ifEmpty { preference.mapNotNull { codec -> rank(codec, checkSize = false)?.let { codec to it } } }
        return ranked.sortedBy { it.second }.map { it.first }
    }
}

internal val Codec.mime: String
    get() = when (this) {
        Codec.Hevc -> MediaFormat.MIMETYPE_VIDEO_HEVC
        Codec.H264 -> MediaFormat.MIMETYPE_VIDEO_AVC
    }

/**
 * 用 MediaCodec 解码、直接画到 [surface] 上。异步模式，回调和对解码器的操作都在自己的线程上；关键帧带的参数集作为
 * csd 配进解码器，之后的帧沿用，参数集变了（换了尺寸、编码）时重建解码器。解码器支持时开低延迟。解出的帧马上画，不按时间戳排队。
 */
class MediaCodecVideoDecoder(
    private val surface: Surface,
    private val onNeedKeyframe: () -> Unit,
) : VideoDecoder {
    /** 配好一个解码器用的：编码、参数集、尺寸。 */
    private class Config(val codec: Codec, val parameterSets: List<ByteArray>, val width: Int, val height: Int) {
        fun sameStream(other: Config?): Boolean = other != null && codec == other.codec &&
            parameterSets.size == other.parameterSets.size &&
            parameterSets.zip(other.parameterSets).all { (a, b) -> a.contentEquals(b) }
    }

    private val thread = HandlerThread("runode-video").apply { start() }
    private val handler = Handler(thread.looper)

    // 以下只在 thread 上碰。
    private var codec: MediaCodec? = null
    private var config: Config? = null
    private var size: Pair<Int, Int>? = null

    /** 在等关键帧：这期间的别的帧解不出来，丢掉，也不重复要。 */
    private var awaitingKeyframe = false

    /** 等解码器给输入缓冲的帧，和解码器给了、还没用上的输入缓冲。 */
    private val frames = ArrayDeque<VideoFrame>()
    private val inputs = ArrayDeque<Int>()

    @Volatile private var released = false

    override fun reset(width: Int, height: Int) {
        post {
            size = width to height
            teardown()
            awaitingKeyframe = false
        }
    }

    override fun decode(frame: VideoFrame) {
        post { show(frame) }
    }

    override fun release() {
        if (released) return
        released = true
        if (Looper.myLooper() == thread.looper) {
            teardown()
        } else {
            val done = CountDownLatch(1)
            if (handler.post {
                    teardown()
                    done.countDown()
                }
            ) {
                done.await(releaseWait, TimeUnit.MILLISECONDS)
            }
        }
        thread.quitSafely()
    }

    private fun post(block: () -> Unit) {
        if (released) return
        handler.post { if (!released) block() }
    }

    private fun show(frame: VideoFrame) {
        if (awaitingKeyframe && !frame.keyframe) return
        if (frame.keyframe && frame.parameterSets.isNotEmpty()) {
            val (width, height) = size ?: fallbackSize
            val next = Config(frame.codec, frame.parameterSets, width, height)
            if (!next.sameStream(config)) {
                teardown()
                if (!start(next)) return needKeyframe(again = true)
            }
        }
        if (codec == null || frame.data.isEmpty()) return needKeyframe()
        awaitingKeyframe = false
        frames.addLast(frame)
        // 解码器跟不上：积着的帧越来越旧，丢掉从下一个关键帧接着画。
        if (frames.size > maxQueued) {
            frames.clear()
            return needKeyframe()
        }
        feed()
    }

    private fun start(config: Config): Boolean {
        val mime = config.codec.mime
        val format = MediaFormat.createVideoFormat(mime, config.width, config.height)
        when (config.codec) {
            Codec.Hevc -> format.setByteBuffer("csd-0", annexB(config.parameterSets))
            Codec.H264 -> {
                // H.264 的 SPS（NAL 类型 7）放 csd-0，PPS 放 csd-1。
                val (sps, pps) = config.parameterSets.partition { it.isNotEmpty() && it[0].toInt() and 0x1f == 7 }
                format.setByteBuffer("csd-0", annexB(sps))
                format.setByteBuffer("csd-1", annexB(pps))
            }
        }
        format.setInteger(MediaFormat.KEY_PRIORITY, 0)
        val created = try {
            MediaCodec.createDecoderByType(mime)
        } catch (_: Exception) {
            return false
        }
        return try {
            val capabilities = created.codecInfo.getCapabilitiesForType(mime)
            if (capabilities.isFeatureSupported(MediaCodecInfo.CodecCapabilities.FEATURE_LowLatency)) {
                format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
            }
            created.setCallback(Callback(created), handler)
            created.configure(format, surface, null, 0)
            created.start()
            codec = created
            this.config = config
            true
        } catch (_: Exception) {
            created.release()
            false
        }
    }

    /** 把等着的帧填进解码器给的输入缓冲。 */
    private fun feed() {
        val codec = codec ?: return
        try {
            while (frames.isNotEmpty() && inputs.isNotEmpty()) {
                val frame = frames.removeFirst()
                val index = inputs.removeFirst()
                val buffer = codec.getInputBuffer(index) ?: continue
                if (buffer.capacity() < frame.data.size) return fail()
                buffer.clear()
                putAnnexB(buffer, frame.data)
                val flags = if (frame.keyframe) MediaCodec.BUFFER_FLAG_KEY_FRAME else 0
                codec.queueInputBuffer(index, 0, frame.data.size, frame.ptsMicros, flags)
            }
        } catch (_: IllegalStateException) {
            fail()
        }
    }

    /** 解码出错：丢掉解码器，向电脑要关键帧，收到后重建。 */
    private fun fail() {
        teardown()
        needKeyframe(again = true)
    }

    /** [again] 时已经在等也再要一次：要来的关键帧解不了。 */
    private fun needKeyframe(again: Boolean = false) {
        if (awaitingKeyframe && !again) return
        awaitingKeyframe = true
        onNeedKeyframe()
    }

    private fun teardown() {
        frames.clear()
        inputs.clear()
        val old = codec ?: return
        codec = null
        config = null
        try {
            old.stop()
        } catch (_: IllegalStateException) {
        }
        old.release()
    }

    /** 只认当前解码器的回调，丢掉的解码器迟到的不算。 */
    private inner class Callback(private val owner: MediaCodec) : MediaCodec.Callback() {
        override fun onInputBufferAvailable(codec: MediaCodec, index: Int) {
            if (owner !== this@MediaCodecVideoDecoder.codec) return
            inputs.addLast(index)
            feed()
        }

        override fun onOutputBufferAvailable(codec: MediaCodec, index: Int, info: MediaCodec.BufferInfo) {
            if (owner !== this@MediaCodecVideoDecoder.codec) return
            try {
                codec.releaseOutputBuffer(index, info.size > 0)
            } catch (_: IllegalStateException) {
                fail()
            }
        }

        override fun onError(codec: MediaCodec, e: MediaCodec.CodecException) {
            if (owner !== this@MediaCodecVideoDecoder.codec) return
            fail()
        }

        override fun onOutputFormatChanged(codec: MediaCodec, format: MediaFormat) {}
    }

    private companion object {
        /** 还没收到 `Started` 时配解码器用的尺寸，解码器按参数集里的实际尺寸解。 */
        val fallbackSize = 1920 to 1080

        /** 最多积这么多帧等解码器。 */
        const val maxQueued = 30

        /** [release] 最多等解码器线程这么久（毫秒）。 */
        const val releaseWait = 1000L
    }
}

private val startCode = byteArrayOf(0, 0, 0, 1)

/** 参数集前面各加起始码接在一起，csd 要的写法。 */
private fun annexB(units: List<ByteArray>): ByteBuffer {
    val buffer = ByteBuffer.allocate(units.sumOf { startCode.size + it.size })
    for (unit in units) buffer.put(startCode).put(unit)
    buffer.flip()
    return buffer
}

/** 帧的数据是每个 NAL 前面 4 字节的长度，换成起始码写进 [buffer]，长度不变。格式不对的尾巴原样写。 */
private fun putAnnexB(buffer: ByteBuffer, data: ByteArray) {
    var at = 0
    while (at + 4 <= data.size) {
        val length = ((data[at].toInt() and 0xff) shl 24) or ((data[at + 1].toInt() and 0xff) shl 16) or
            ((data[at + 2].toInt() and 0xff) shl 8) or (data[at + 3].toInt() and 0xff)
        if (length < 0 || length > data.size - at - 4) break
        buffer.put(startCode).put(data, at + 4, length)
        at += 4 + length
    }
    if (at < data.size) buffer.put(data, at, data.size - at)
}
