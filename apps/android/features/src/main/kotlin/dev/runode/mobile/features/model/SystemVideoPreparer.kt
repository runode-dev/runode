package dev.runode.mobile.features.model

import android.content.Context
import android.media.MediaMetadataRetriever
import android.net.Uri
import android.os.Handler
import android.os.Looper
import androidx.annotation.OptIn
import androidx.media3.common.Format
import androidx.media3.common.MediaItem
import androidx.media3.common.Metadata
import androidx.media3.common.MimeTypes
import androidx.media3.common.util.UnstableApi
import androidx.media3.container.Mp4LocationData
import androidx.media3.container.XmpData
import androidx.media3.effect.Presentation
import androidx.media3.muxer.BufferInfo
import androidx.media3.muxer.Muxer
import androidx.media3.transformer.Composition
import androidx.media3.transformer.DefaultMuxer
import androidx.media3.transformer.EditedMediaItem
import androidx.media3.transformer.EditedMediaItemSequence
import androidx.media3.transformer.Effects
import androidx.media3.transformer.ExportException
import androidx.media3.transformer.ExportResult
import androidx.media3.transformer.Transformer
import com.google.common.collect.ImmutableList
import java.io.File
import java.nio.ByteBuffer
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext

/**
 * 用 media3 的 Transformer 把视频转成 H.264 视频、AAC 音频的 MP4，输出到 cacheDir 里的临时文件：短边超过 [maxShortSide] 时
 * 缩到 [maxShortSide]（横竖屏都按短边，读不出尺寸时也缩，不放大更小的视频），HDR 转成 SDR。
 *
 * Transformer 把原视频带的元数据照抄进输出，其中有拍摄地点（`Mp4LocationData`）和可能带 GPS 的 XMP；写文件的 [Muxer] 套上
 * [LocationFreeMuxer]，这两样不写。
 */
@OptIn(markerClass = [UnstableApi::class])
class SystemVideoPreparer(private val context: Context) : VideoPreparer {
    override suspend fun prepare(uri: String): File {
        val (probe, file) = withContext(Dispatchers.IO) { probe(uri) to File.createTempFile("video", ".mp4", context.cacheDir) }
        try {
            withContext(Dispatchers.Main) { export(uri, probe, file) }
            return file
        } catch (e: Throwable) {
            file.delete()
            throw e
        }
    }

    /** 转之前从原视频读到的：画面的短边（读不出来时为空）、有没有声音。 */
    private class Probe(val shortSide: Int?, val hasAudio: Boolean)

    /** 读不出来时当作尺寸不明、有声音。 */
    private fun probe(uri: String): Probe = try {
        MediaMetadataRetriever().use { retriever ->
            retriever.setDataSource(context, Uri.parse(uri))
            val width = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_WIDTH)?.toIntOrNull()
            val height = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_HEIGHT)?.toIntOrNull()
            val hasAudio = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_HAS_AUDIO) != null
            Probe(if (width == null || height == null) null else minOf(width, height), hasAudio)
        }
    } catch (_: Exception) {
        Probe(null, hasAudio = true)
    }

    /** 在主线程上跑：Transformer 在建它的线程的 Looper 上回调。 */
    private suspend fun export(uri: String, probe: Probe, output: File) = suspendCancellableCoroutine { continuation ->
        val scale = probe.shortSide == null || probe.shortSide > maxShortSide
        val effects = if (scale) Effects(emptyList(), listOf(Presentation.createForShortSide(maxShortSide))) else Effects.EMPTY
        val item = EditedMediaItem.Builder(MediaItem.fromUri(uri)).setEffects(effects).build()
        // 序列要声明出哪几种轨道，原视频没有的那种不声明。
        val sequence =
            if (probe.hasAudio) EditedMediaItemSequence.withAudioAndVideoFrom(listOf(item)) else EditedMediaItemSequence.withVideoFrom(listOf(item))
        val composition = Composition.Builder(sequence)
            .setHdrMode(Composition.HDR_MODE_TONE_MAP_HDR_TO_SDR_USING_OPEN_GL)
            .build()
        val transformer = Transformer.Builder(context)
            .setVideoMimeType(MimeTypes.VIDEO_H264)
            .setAudioMimeType(MimeTypes.AUDIO_AAC)
            .setMuxerFactory(LocationFreeMuxer.Factory(DefaultMuxer.Factory()))
            .addListener(
                object : Transformer.Listener {
                    override fun onCompleted(composition: Composition, exportResult: ExportResult) = continuation.resume(Unit)

                    override fun onError(composition: Composition, exportResult: ExportResult, exportException: ExportException) =
                        continuation.resumeWithException(exportException)
                },
            )
            .build()
        transformer.start(composition, output.path)
        val main = Handler(Looper.getMainLooper())
        continuation.invokeOnCancellation { main.post { transformer.cancel() } }
    }

    companion object {
        /** 视频短边的上限（像素）。 */
        const val maxShortSide: Int = 720
    }
}

/** 不写拍摄地点和 XMP 的 [Muxer]：轨道格式里带的和单独加的元数据都滤掉。 */
@OptIn(markerClass = [UnstableApi::class])
private class LocationFreeMuxer(private val muxer: Muxer) : Muxer {
    override fun addTrack(format: Format): Int = muxer.addTrack(format.buildUpon().setMetadata(format.metadata?.withoutLocation()).build())

    override fun writeSampleData(trackId: Int, data: ByteBuffer, bufferInfo: BufferInfo) = muxer.writeSampleData(trackId, data, bufferInfo)

    override fun addMetadataEntry(metadataEntry: Metadata.Entry) {
        if (!metadataEntry.revealsLocation()) muxer.addMetadataEntry(metadataEntry)
    }

    override fun close() = muxer.close()

    class Factory(private val factory: Muxer.Factory) : Muxer.Factory {
        override fun create(path: String): Muxer = LocationFreeMuxer(factory.create(path))

        override fun getSupportedSampleMimeTypes(trackType: Int): ImmutableList<String> = factory.getSupportedSampleMimeTypes(trackType)

        override fun supportsWritingNegativeTimestampsInEditList(): Boolean = factory.supportsWritingNegativeTimestampsInEditList()
    }

    private companion object {
        fun Metadata.Entry.revealsLocation(): Boolean = this is Mp4LocationData || this is XmpData

        fun Metadata.withoutLocation(): Metadata? {
            val kept = (0 until length()).map(::get).filterNot { it.revealsLocation() }
            return if (kept.isEmpty()) null else Metadata(kept)
        }
    }
}
