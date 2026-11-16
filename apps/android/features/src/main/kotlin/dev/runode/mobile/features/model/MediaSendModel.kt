package dev.runode.mobile.features.model

import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.ImageDecoder
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import dev.runode.mobile.connection.HostLink
import dev.runode.mobile.connection.LinkFailure
import dev.runode.mobile.connection.MachineRecord
import dev.runode.mobile.connection.MediaFailure
import dev.runode.mobile.features.R
import dev.runode.mobile.protocol.ClientMsg
import dev.runode.mobile.protocol.SessionId
import dev.runode.mobile.protocol.maxMediaBytes
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.InputStream
import java.nio.ByteBuffer
import kotlin.math.max
import kotlin.math.roundToInt
import kotlin.time.Duration
import kotlin.time.Duration.Companion.minutes
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull

/** 连上一台电脑收图片和视频的地方。App 入口给真的（`MediaConnection.open`），演示模式和测试不给。 */
typealias MediaOpener = suspend (MachineRecord) -> MediaUploader

/** 收图片和视频的一条连接，真的是 `MediaConnection`，测试换成假的。[send] 的 `progress` 报这个文件已发、一共的字节数。 */
interface MediaUploader {
    /** 发一张编好的图片，返回它在电脑上的绝对路径。 */
    suspend fun send(data: ByteArray, progress: (sent: Long, total: Long) -> Unit): String

    /** 边读边发 [file]，返回它在电脑上的绝对路径。 */
    suspend fun send(file: File, progress: (sent: Long, total: Long) -> Unit): String

    fun close()
}

/** 发之前处理图片，真的是 [SystemImagePreparer]，测试换成假的。 */
fun interface ImagePreparer {
    /** [raw] 是用户挑的图片文件原样的字节，返回要发给电脑的字节；读不出图片时抛错。 */
    suspend fun prepare(raw: ByteArray): ByteArray
}

/** 发之前处理视频，真的是 [SystemVideoPreparer]，测试换成假的。 */
fun interface VideoPreparer {
    /** 把 [uri] 指的视频转成要发给电脑的文件，返回的临时文件归调用方删；转不了时抛错，不留下临时文件。 */
    suspend fun prepare(uri: String): File
}

/** 要发给电脑的一项。 */
sealed class OutgoingMedia {
    /** 一张图片：[read] 读出图片文件原样的字节，在后台线程上调，读到超过 `maxMediaBytes` 时抛 [MediaTooLarge]。 */
    class Image(val read: () -> ByteArray) : OutgoingMedia()

    /** 一段视频，[uri] 是它的 content URI。 */
    data class Video(val uri: String) : OutgoingMedia()
}

/** 发图片、视频出错后用户该怎么补救。 */
enum class MediaSendRemedy {
    /** 电脑上的 Runode 不认得发图片和视频，要更新。 */
    UpdateComputer,
}

/** 读不出用户挑的图片、转不了用户挑的视频。 */
internal class MediaUnreadable(val video: Boolean, cause: Exception) :
    Exception(if (video) "the video could not be converted" else "the image could not be read", cause)

/** 读到的图片、处理过的图片或视频超过 `maxMediaBytes`。 */
internal class MediaTooLarge(val video: Boolean) : Exception(if (video) "the video is too large" else "the image is too large")

/**
 * 把手机上的截图、照片和视频发给电脑终端里的 AI agent：一项项处理（图片经 [ImagePreparer]，视频经 [VideoPreparer]），经一条
 * 收图片和视频的连接传到电脑，拿回它在电脑上的路径，再在终端的连接上发 [ClientMsg.PastePath] 打进会话。全 app 一份；一批
 * 还没发完时再来的一批排在后面，按先后发。在主线程上用。
 */
class MediaSendModel internal constructor(
    /** 为空时（演示模式、测试）发不了。 */
    private val openMedia: MediaOpener?,
    private val imagePreparer: ImagePreparer?,
    private val videoPreparer: VideoPreparer?,
    /** 读图片、处理图片、删临时文件在哪里跑；测试换成测试的调度器。 */
    private val work: CoroutineDispatcher,
) {
    constructor(openMedia: MediaOpener?, imagePreparer: ImagePreparer?, videoPreparer: VideoPreparer?) :
        this(openMedia, imagePreparer, videoPreparer, Dispatchers.IO)

    sealed class Phase {
        data object Idle : Phase()

        /** 正在压这一批第 [index] 项（从 1 数起）的视频，这一批一共 [count] 项。 */
        data class Preparing(val index: Int, val count: Int) : Phase()

        /** 正在发这一批第 [index] 项，一共 [count] 项；这一项发了 [fraction]（0 到 1），[video] 说它是不是视频。 */
        data class Sending(val index: Int, val count: Int, val fraction: Float, val video: Boolean) : Phase()
    }

    private class Batch(val items: List<OutgoingMedia>, val machine: MachineRecord, val link: HostLink, val sessionId: SessionId)

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    /** 排着还没开始发的批。 */
    private val queue = ArrayDeque<Batch>()
    private var draining = false

    var phase by mutableStateOf<Phase>(Phase.Idle)
        private set

    private var error by mutableStateOf<UiText?>(null)

    /** 发图片、视频出的错，终端页弹出来说；清掉时 [remedy] 跟着清掉。 */
    var errorMessage: UiText?
        get() = error
        set(value) {
            error = value
            if (value == null) remedy = null
        }

    /** [errorMessage] 说的错用户该怎么补救。 */
    var remedy by mutableStateOf<MediaSendRemedy?>(null)
        private set

    /** 有连电脑收图片和视频的办法，也能处理图片和视频。 */
    val available: Boolean get() = openMedia != null && imagePreparer != null && videoPreparer != null

    /** 现在能经 [link] 发：连着，电脑也认得 [ClientMsg.PastePath]。不能时设好 [errorMessage]，返回假。 */
    fun checkReady(link: HostLink): Boolean {
        if (!link.state.value.isConnected) {
            fail(UiText.res(R.string.not_connected), null)
            return false
        }
        if (!link.pastesPaths) {
            fail(UiText.res(R.string.media_computer_outdated), MediaSendRemedy.UpdateComputer)
            return false
        }
        return true
    }

    /**
     * 把 [items]（多于 [maxItems] 的不要）按先后发给 [machine]，每项存好后打进会话 [sessionId]。上一批还没发完时排在它后面。
     * 发不了或者 [machine] 为空时什么都不做。一批里出了错就停下这一批，已经打进去的不收回，排在后面的批照发；存好一项时终端的
     * 连接断着、[reconnectTimeout] 内没重新连上也算出错。
     */
    fun send(items: List<OutgoingMedia>, machine: MachineRecord?, link: HostLink, sessionId: SessionId) {
        if (items.isEmpty() || !available || machine == null) return
        if (!checkReady(link)) return
        queue.addLast(Batch(items.take(maxItems), machine, link, sessionId))
        if (draining) return
        draining = true
        phase = firstPhase(queue.first().items)
        scope.launch {
            try {
                while (true) deliver(queue.removeFirstOrNull() ?: break)
            } finally {
                queue.clear()
                draining = false
                phase = Phase.Idle
            }
        }
    }

    private suspend fun deliver(batch: Batch) {
        val open = openMedia ?: return
        val images = imagePreparer ?: return
        val videos = videoPreparer ?: return
        val count = batch.items.size
        var uploader: MediaUploader? = null
        suspend fun connection(): MediaUploader = uploader ?: open(batch.machine).also { uploader = it }
        try {
            for ((offset, item) in batch.items.withIndex()) {
                val index = offset + 1
                val path = when (item) {
                    is OutgoingMedia.Image -> {
                        phase = Phase.Sending(index, count, 0f, video = false)
                        val data = prepareImage(images, item)
                        connection().send(data) { sent, total -> phase = Phase.Sending(index, count, fraction(sent, total), false) }
                    }
                    is OutgoingMedia.Video -> {
                        phase = Phase.Preparing(index, count)
                        val file = prepareVideo(videos, item)
                        try {
                            if (withContext(work) { file.length() } > maxMediaBytes) throw MediaTooLarge(video = true)
                            phase = Phase.Sending(index, count, 0f, video = true)
                            connection().send(file) { sent, total -> phase = Phase.Sending(index, count, fraction(sent, total), true) }
                        } finally {
                            withContext(NonCancellable + work) { file.delete() }
                        }
                    }
                }
                // 断了的连接上发的消息会被丢掉，文件存好了也打不进去：App 退到后台时连接断开，等它回到前台重新连上。
                if (!awaitConnected(batch.link)) throw LinkFailure.HostClosed
                batch.link.send(ClientMsg.PastePath(batch.link.nextRequestId(), batch.sessionId, path))
            }
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            fail(message(e), null)
        } finally {
            uploader?.close()
        }
    }

    /** 等 [link] 连上，最多 [reconnectTimeout]；连着时为真。 */
    private suspend fun awaitConnected(link: HostLink): Boolean =
        withTimeoutOrNull(reconnectTimeout) { link.state.first { it.isConnected } } != null

    private suspend fun prepareImage(preparer: ImagePreparer, item: OutgoingMedia.Image): ByteArray {
        val data = try {
            withContext(work) { preparer.prepare(item.read()) }
        } catch (e: CancellationException) {
            throw e
        } catch (e: MediaTooLarge) {
            throw e
        } catch (e: Exception) {
            throw MediaUnreadable(video = false, e)
        }
        if (data.size > maxMediaBytes) throw MediaTooLarge(video = false)
        return data
    }

    private suspend fun prepareVideo(preparer: VideoPreparer, item: OutgoingMedia.Video): File = try {
        preparer.prepare(item.uri)
    } catch (e: CancellationException) {
        throw e
    } catch (e: Exception) {
        throw MediaUnreadable(video = true, e)
    }

    private fun fail(message: UiText, remedy: MediaSendRemedy?) {
        errorMessage = message
        this.remedy = remedy
    }

    companion object {
        /** 一次最多挑这么多项。 */
        const val maxItems: Int = 5

        /** 存好一项时终端的连接没连着，最多等它重新连上这么久再打进会话。 */
        internal val reconnectTimeout: Duration = 2.minutes

        private fun firstPhase(items: List<OutgoingMedia>): Phase = when (items.first()) {
            is OutgoingMedia.Image -> Phase.Sending(1, items.size, 0f, video = false)
            is OutgoingMedia.Video -> Phase.Preparing(1, items.size)
        }

        private fun fraction(sent: Long, total: Long): Float = if (total <= 0) 1f else (sent.toDouble() / total).toFloat()

        internal fun message(error: Exception): UiText = when (error) {
            is MediaFailure -> UiText.res(R.string.media_failed_on_computer, error.reason)
            is MediaUnreadable -> UiText.res(if (error.video) R.string.media_video_unreadable else R.string.media_image_unreadable)
            is MediaTooLarge -> UiText.res(if (error.video) R.string.media_video_too_large else R.string.media_image_too_large)
            is LinkFailure -> Presentation.linkFailure(error)
            else -> UiText.Raw(error.message ?: error.toString())
        }

        /** 读出 [input] 里的整张图片；读到超过 [limit] 字节时不再读，抛 [MediaTooLarge]。 */
        internal fun readImage(input: InputStream, limit: Int = maxMediaBytes): ByteArray {
            val out = ByteArrayOutputStream()
            val buffer = ByteArray(64 shl 10)
            var total = 0L
            while (true) {
                val read = input.read(buffer)
                if (read < 0) return out.toByteArray()
                total += read
                if (total > limit) throw MediaTooLarge(video = false)
                out.write(buffer, 0, read)
            }
        }
    }
}

/**
 * 用系统的 [ImageDecoder] 解图片（照 EXIF 转正），长边缩到 [maxSide] 以内，再编成不带元数据的 PNG 或 JPEG：原图是 PNG 的编成
 * PNG，编出来超过 [maxPngBytes] 时改编 JPEG；别的编成 [jpegQuality] 的 JPEG。GIF 不超过 `maxMediaBytes` 时原样发，动图才还动。
 */
object SystemImagePreparer : ImagePreparer {
    private const val maxSide = 2048
    private const val jpegQuality = 85
    private const val maxPngBytes = 5 shl 20

    override suspend fun prepare(raw: ByteArray): ByteArray = withContext(Dispatchers.Default) {
        if (raw.startsWith(gifMagic) && raw.size <= maxMediaBytes) return@withContext raw
        val bitmap = ImageDecoder.decodeBitmap(ImageDecoder.createSource(ByteBuffer.wrap(raw))) { decoder, info, _ ->
            // 透明图改编 JPEG 时要画到软件的 Canvas 上，硬件位图画不上去。
            decoder.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
            val width = info.size.width
            val height = info.size.height
            val longest = max(width, height)
            if (longest > maxSide) {
                val scale = maxSide.toDouble() / longest
                decoder.setTargetSize(max(1, (width * scale).roundToInt()), max(1, (height * scale).roundToInt()))
            }
        }
        try {
            if (raw.startsWith(pngMagic)) {
                val png = encode(bitmap, Bitmap.CompressFormat.PNG, 100)
                if (png.size <= maxPngBytes) return@withContext png
            }
            encodeJpeg(bitmap)
        } finally {
            bitmap.recycle()
        }
    }

    /** JPEG 没有透明：透明的地方铺白底，不然变成黑的。 */
    private fun encodeJpeg(bitmap: Bitmap): ByteArray {
        if (!bitmap.hasAlpha()) return encode(bitmap, Bitmap.CompressFormat.JPEG, jpegQuality)
        val opaque = Bitmap.createBitmap(bitmap.width, bitmap.height, Bitmap.Config.ARGB_8888)
        try {
            Canvas(opaque).apply {
                drawColor(Color.WHITE)
                drawBitmap(bitmap, 0f, 0f, null)
            }
            return encode(opaque, Bitmap.CompressFormat.JPEG, jpegQuality)
        } finally {
            opaque.recycle()
        }
    }

    private fun encode(bitmap: Bitmap, format: Bitmap.CompressFormat, quality: Int): ByteArray =
        ByteArrayOutputStream().also { bitmap.compress(format, quality, it) }.toByteArray()

    private val gifMagic = "GIF8".encodeToByteArray()
    private val pngMagic = byteArrayOf(0x89.toByte(), 'P'.code.toByte(), 'N'.code.toByte(), 'G'.code.toByte())

    private fun ByteArray.startsWith(prefix: ByteArray): Boolean =
        size >= prefix.size && prefix.indices.all { this[it] == prefix[it] }
}
