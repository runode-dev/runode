package dev.runode.mobile.speech

import java.io.File
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.security.MessageDigest
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withContext
import kotlin.coroutines.cancellation.CancellationException

/**
 * 手机上识别要的模型文件（SenseVoice 和切句用的 Silero VAD）和它们的下载。和电脑那边 `runode_speech` 的清单是同一批文件、
 * 同样的目录布局。每个文件按大小和 SHA-256 核对过才改成正式的名字，所以目录里有这个文件就说明它是完整的。
 */
object SpeechModels {
    internal class Asset(
        /** 在模型目录下的子目录。 */
        val dir: String,
        val name: String,
        /** HuggingFace 上原作者的仓库。 */
        val hfRepo: String,
        val size: Long,
        val sha256: String,
    ) {
        fun path(root: File): File = File(File(root, dir), name)

        fun urls(chinaFirst: Boolean): List<URL> {
            val hf = URL("https://huggingface.co/$hfRepo/resolve/main/$name")
            val ms = URL("$modelScope/$dir/$name")
            return if (chinaFirst) listOf(ms, hf) else listOf(hf, ms)
        }
    }

    /** ModelScope 上 runode 自己的镜像，按 `<dir>/<name>` 放和 HuggingFace 上一样的文件。 */
    internal const val modelScope = "https://modelscope.cn/models/runode/speech-models/resolve/master"

    internal val senseVoice = Asset(
        dir = "sense-voice-int8-2024-07-17",
        name = "model.int8.onnx",
        hfRepo = "csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17",
        size = 239_233_841,
        sha256 = "c71f0ce00bec95b07744e116345e33d8cbbe08cef896382cf907bf4b51a2cd51",
    )
    internal val tokens = Asset(
        dir = "sense-voice-int8-2024-07-17",
        name = "tokens.txt",
        hfRepo = "csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17",
        size = 315_894,
        sha256 = "f449eb28dc567533d7fa59be34e2abca8784f771850c78a47fb731a31429a1dc",
    )
    internal val vad = Asset(
        dir = "silero-vad",
        name = "silero_vad.onnx",
        hfRepo = "csukuangfj/vad",
        size = 1_807_522,
        sha256 = "a35ebf52fd3ce5f1469b2a36158dba761bc47b973ea3382b3186ca15b1f5af28",
    )
    internal val assets = listOf(senseVoice, tokens, vad)

    /** 全部文件的字节数。 */
    val downloadSize: Long = assets.sumOf { it.size }

    fun isInstalled(root: File): Boolean = assets.all { it.path(root).isFile }

    fun remove(root: File) {
        for (dir in assets.map { File(root, it.dir) }.toSet()) {
            if (dir.exists() && !dir.deleteRecursively()) throw IOException("could not delete $dir")
        }
    }

    /**
     * 把缺的文件下载到 [root] 下。[chinaFirst] 时先试 ModelScope 再试 HuggingFace，否则反过来；一个源失败（连不上、断了、
     * 核对不过）就换另一个。下载期间在下载的线程上不时调 `progress(已有的字节)`。协程取消时停下。
     *
     * shortcut: 断了从头下，下载途中 App 被系统杀掉也得重来；常见了改成前台服务或 WorkManager 加断点续传
     */
    suspend fun download(root: File, chinaFirst: Boolean, progress: (Long) -> Unit) =
        download(root, assets, { it.urls(chinaFirst) }, progress)

    internal suspend fun download(
        root: File,
        assets: List<Asset>,
        urls: (Asset) -> List<URL>,
        progress: (Long) -> Unit,
    ) = withContext(Dispatchers.IO) {
        var done = 0L
        for (asset in assets) {
            val target = asset.path(root)
            if (target.isFile) {
                done += asset.size
                continue
            }
            val dir = target.parentFile!!
            if (!dir.isDirectory && !dir.mkdirs()) throw IOException("could not create $dir")
            var failure: Exception? = null
            for (url in urls(asset)) {
                ensureActive()
                try {
                    val before = done
                    val file = fetch(url, File(dir, "${asset.name}.part")) { progress(before + it) }
                    verify(file, asset)
                    if (!file.renameTo(target)) throw IOException("could not move $file to $target")
                    failure = null
                    break
                } catch (e: CancellationException) {
                    throw e
                } catch (e: Exception) {
                    failure = e
                }
            }
            failure?.let { throw it }
            done += asset.size
            progress(done)
        }
    }

    /** 把 [url] 下载到 [file]（已有的覆盖掉），失败时删掉它。 */
    private suspend fun fetch(url: URL, file: File, progress: (Long) -> Unit): File {
        val connection = url.openConnection() as HttpURLConnection
        connection.connectTimeout = 30_000
        connection.readTimeout = 60_000
        try {
            val status = connection.responseCode
            if (status !in 200..299) throw SpeechError.HttpStatus(status)
            connection.inputStream.use { input ->
                file.outputStream().use { output ->
                    val buffer = ByteArray(1 shl 16)
                    var total = 0L
                    while (true) {
                        currentCoroutineContext().ensureActive()
                        val count = input.read(buffer)
                        if (count < 0) break
                        output.write(buffer, 0, count)
                        total += count
                        progress(total)
                    }
                }
            }
            return file
        } catch (e: Exception) {
            file.delete()
            throw e
        } finally {
            connection.disconnect()
        }
    }

    private fun verify(file: File, asset: Asset) {
        val digest = MessageDigest.getInstance("SHA-256")
        var size = 0L
        file.inputStream().use { input ->
            val buffer = ByteArray(1 shl 20)
            while (true) {
                val count = input.read(buffer)
                if (count < 0) break
                digest.update(buffer, 0, count)
                size += count
            }
        }
        val hex = digest.digest().joinToString("") { "%02x".format(it) }
        if (size != asset.size || hex != asset.sha256) {
            file.delete()
            throw SpeechError.CorruptDownload(asset.name)
        }
    }
}

/** 下载模型、录音、识别出的错。 */
sealed class SpeechError(message: String) : Exception(message, null, false, false) {
    /** 下载的文件大小或 SHA-256 对不上。 */
    data class CorruptDownload(val name: String) : SpeechError("$name does not match its expected size or SHA-256")

    /** 服务器回了出错的状态码。 */
    data class HttpStatus(val status: Int) : SpeechError("HTTP $status")

    /** 麦克风交来的样本全是 0：别的 App 占着麦克风、App 在后台录时系统不报错，只给静音。 */
    data object Silent : SpeechError("the microphone recorded only silence")

    /** 没有录音权限（`RECORD_AUDIO`）。 */
    data object NoPermission : SpeechError("no permission to record audio")

    /** 模型文件不全或者读不了。 */
    data object ModelUnavailable : SpeechError("the speech model is missing or unreadable")

    /** 没有能用的麦克风，或者它录不了识别要的格式。 */
    data object MicrophoneUnavailable : SpeechError("no usable microphone")

    /** 这里没有识别：JVM 上跑单元测试时加载不了 sherpa-onnx 的 JNI 库。 */
    data object Unsupported : SpeechError("speech recognition is not available here")
}
