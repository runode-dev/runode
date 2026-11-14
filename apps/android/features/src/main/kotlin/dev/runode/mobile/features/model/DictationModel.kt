package dev.runode.mobile.features.model

import android.content.Context
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import dev.runode.mobile.connection.LinkFailure
import dev.runode.mobile.connection.MachineRecord
import dev.runode.mobile.connection.SpeechConnection
import dev.runode.mobile.features.R
import dev.runode.mobile.protocol.maxSpeechSeconds
import dev.runode.mobile.protocol.speechSampleRate
import dev.runode.mobile.speech.SpeechEngine
import dev.runode.mobile.speech.SpeechError
import dev.runode.mobile.speech.SpeechModels
import dev.runode.mobile.speech.SpeechRecorder
import dev.runode.mobile.terminal.DictationPress
import java.io.File
import java.util.Locale
import kotlin.time.ComparableTimeMark
import kotlin.time.Duration
import kotlin.time.Duration.Companion.milliseconds
import kotlin.time.Duration.Companion.seconds
import kotlin.time.TimeSource
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/** 连上一台电脑的识别。App 入口给真的（`SpeechConnection.open`），演示模式和测试不给。 */
typealias SpeechOpener = suspend (MachineRecord) -> SpeechConnection

/** 语音输入在哪里识别。 */
@Serializable
enum class DictationSource {
    /** 手机上识别；手机上还没下载模型时交给电脑。 */
    @SerialName("phone") Phone,

    /** 交给终端所在的电脑识别，用电脑设置里的模型。 */
    @SerialName("computer") Computer,
}

/** 语音输入出错后用户能去哪里补救。 */
enum class DictationRemedy {
    /** 到 app 的设置页下载识别模型。 */
    DownloadModel,

    /** 到系统设置里给 Runode 麦克风权限。 */
    MicrophonePermission,
}

/** 录音的一方，真的是 [SpeechRecorder]，测试换成假的。 */
internal interface DictationRecorder {
    suspend fun start()

    suspend fun stop(): FloatArray
}

/** 手机上没下模型，交给电脑时电脑上也没下。 */
internal class NoModelAnywhere : Exception("neither the phone nor the computer has a speech model")

/**
 * 语音输入：手机上识别模型的下载（设置页看它），和终端页上的一次次录音、识别。全 app 一份，同一时刻只有一段在录。辅助栏上的
 * 麦克风键按住超过 [hold] 再松开就结束，点一下就一直录到再按一次；录满 `maxSpeechSeconds` 自己结束。识别出的文字交给开始
 * 录音时给的 `insert`。在主线程上用。
 */
class DictationModel internal constructor(
    /** 手机上的语音模型下载到哪里。 */
    val root: File,
    /** 为空时只在手机上识别。 */
    private val openSpeech: SpeechOpener?,
    private val makeRecorder: (onChunk: (FloatArray) -> Unit) -> DictationRecorder,
    private val transcribe: suspend (FloatArray) -> String,
    /** 有没有麦克风权限；测试里不碰系统的授权。 */
    private val permitted: () -> Boolean,
    isInstalled: (File) -> Boolean = SpeechModels::isInstalled,
    private val downloadModels: suspend (root: File, chinaFirst: Boolean, progress: (Long) -> Unit) -> Unit =
        SpeechModels::download,
) {
    /** [context] 用来录麦克风、查麦克风权限；为空时（测试、演示模式）语音输入报没有能用的麦克风。 */
    constructor(root: File, openSpeech: SpeechOpener?, context: Context?) : this(
        root = root,
        openSpeech = openSpeech,
        makeRecorder = { onChunk -> if (context == null) NoMicrophone else SystemRecorder(SpeechRecorder(context, onChunk)) },
        transcribe = SpeechEngine(root)::transcribe,
        permitted = { context == null || SpeechRecorder.permitted(context) },
    )

    sealed class Download {
        data object Idle : Download()

        /** 已经下了这么多字节，总数是 `SpeechModels.downloadSize`。 */
        data class Running(val done: Long) : Download()

        data class Failed(val reason: UiText) : Download()
    }

    enum class Phase {
        Idle,
        Recording,
        Transcribing,
    }

    /** 正在录的一段。 */
    private class Take(
        val recorder: DictationRecorder,
        /** 交给电脑识别时连电脑的那次连接。 */
        val remote: Deferred<SpeechConnection>?,
        val insert: (String) -> Unit,
        val started: ComparableTimeMark,
    ) {
        var starting: Deferred<Unit>? = null

        /** 麦克风键还按着。 */
        var held = true
    }

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    var download by mutableStateOf<Download>(Download.Idle)
        private set

    /** 手机上的模型下好了。 */
    var installed by mutableStateOf(isInstalled(root))
        private set

    var phase by mutableStateOf(Phase.Idle)
        private set

    private var error by mutableStateOf<UiText?>(null)

    /** 录音、识别出的错，终端页弹出来说；清掉时 [remedy] 跟着清掉。 */
    var errorMessage: UiText?
        get() = error
        set(value) {
            error = value
            if (value == null) remedy = null
        }

    /** [errorMessage] 说的错用户能去哪里补救，终端页的弹框据此多给一个按钮。 */
    var remedy by mutableStateOf<DictationRemedy?>(null)
        private set

    /**
     * 弹系统的框向用户要麦克风权限，返回给没给；要 Activity，由界面层给。为空时没有权限就直接说去系统设置里打开。
     */
    var requestPermission: (suspend () -> Boolean)? = null

    /** 量麦克风键按了多久的时钟，测试换成虚拟时间的。 */
    internal var timeSource: TimeSource.WithComparableMarks = TimeSource.Monotonic

    private var downloading: Job? = null
    private var take: Take? = null

    /** 上一段录音器的停下：下一段的开始等它做完。 */
    private var stopping: Job? = null

    // 模型

    /** 在后台下载手机上的模型。中文界面先试 ModelScope，国内连 HuggingFace 常常不通。 */
    fun startDownload() {
        if (downloading != null || installed) return
        val chinaFirst = Locale.getDefault().language == "zh"
        download = Download.Running(0)
        val total = SpeechModels.downloadSize
        var shown = 0L
        downloading = scope.launch {
            try {
                downloadModels(root, chinaFirst) { done ->
                    // 按百分比刷新：每到一小块都刷的话一次下载要刷几千次界面。
                    val percent = done * 100 / total
                    if (percent != shown) {
                        shown = percent
                        scope.launch { progressed(done) }
                    }
                }
                installed = true
                download = Download.Idle
            } catch (e: CancellationException) {
                download = Download.Idle
                throw e
            } catch (e: Exception) {
                download = Download.Failed(message(e))
            } finally {
                downloading = null
            }
        }
    }

    fun cancelDownload() {
        downloading?.cancel()
    }

    fun remove() {
        try {
            SpeechModels.remove(root)
            installed = false
        } catch (e: Exception) {
            download = Download.Failed(message(e))
        }
    }

    private fun progressed(done: Long) {
        if (download is Download.Running) download = Download.Running(done)
    }

    // 录音

    /** 麦克风键的一次动作。[machine] 是终端所在的电脑，交给电脑识别时连它。 */
    fun press(press: DictationPress, machine: MachineRecord?, source: DictationSource, insert: (String) -> Unit) {
        val take = take
        when {
            press == DictationPress.Down && phase == Phase.Idle -> start(machine, source, insert)
            press == DictationPress.Down && phase == Phase.Recording && take?.held == false -> finish()
            press == DictationPress.Up && phase == Phase.Recording && take?.held == true -> {
                take.held = false
                if (take.started.elapsedNow() >= hold) finish()
            }
            press == DictationPress.Cancel && phase == Phase.Recording && take?.held == true -> cancel()
        }
    }

    /** 丢掉正在录的（离开终端页时）。 */
    fun cancel() {
        val take = take ?: return
        this.take = null
        phase = Phase.Idle
        stop(take)
        closeRemote(take)
    }

    private fun start(machine: MachineRecord?, source: DictationSource, insert: (String) -> Unit) {
        if (!permitted()) {
            val request = requestPermission
            if (request == null) {
                fail(UiText.res(R.string.dictation_no_permission), DictationRemedy.MicrophonePermission)
            } else {
                // 这一次只问权限：系统的框弹出来时手指早松开了，等用户再按。
                scope.launch {
                    if (!request()) fail(UiText.res(R.string.dictation_no_permission), DictationRemedy.MicrophonePermission)
                }
            }
            return
        }
        val local = source == DictationSource.Phone && installed
        val open = openSpeech
        if (!local && (machine == null || open == null)) {
            fail(UiText.res(R.string.dictation_download_model_first), DictationRemedy.DownloadModel)
            return
        }
        val feed = Feed()
        val recorder = makeRecorder { chunk -> if (!local) feed.push(chunk) }
        val remote = if (!local && machine != null && open != null) {
            scope.async { open(machine).also(feed::attach) }
        } else {
            null
        }
        val take = Take(recorder, remote, insert, timeSource.markNow())
        this.take = take
        phase = Phase.Recording
        // 电脑一连上就知道它有没有模型，没有时当场停下，不让用户说完一整段才知道白说了。
        if (remote != null) {
            scope.launch {
                val connection = try {
                    remote.await()
                } catch (_: Exception) {
                    return@launch
                }
                if (!connection.installed && this@DictationModel.take === take) {
                    cancel()
                    fail(NoModelAnywhere())
                }
            }
        }
        // 录音器开起来之前就可能松手或取消，所以开不起来时只在这段还是当前这段时才复位。
        val previous = stopping
        take.starting = scope.async {
            previous?.join()
            try {
                recorder.start()
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                closeRemote(take)
                fail(e)
                if (this@DictationModel.take === take) {
                    this@DictationModel.take = null
                    phase = Phase.Idle
                }
                throw e
            }
        }
        scope.launch {
            delay(maxSpeechSeconds.seconds)
            if (this@DictationModel.take === take) finish()
        }
    }

    private fun finish() {
        val take = take ?: return
        this.take = null
        phase = Phase.Transcribing
        val stopped = stop(take)
        scope.launch {
            try {
                val samples = stopped.await()
                val remote = take.remote
                val text = if (remote != null) {
                    val connection = remote.await()
                    if (!connection.installed) {
                        connection.close()
                        throw NoModelAnywhere()
                    }
                    connection.finish()
                } else {
                    transcribe(samples)
                }
                if (text.isNotEmpty()) take.insert(text)
            } catch (e: Exception) {
                closeRemote(take)
                if (e !is CancellationException) fail(e)
            }
            phase = Phase.Idle
        }
    }

    /** 等这段的录音器开起来再停；没开起来就不停，交出开始时的错误。 */
    private fun stop(take: Take): Deferred<FloatArray> {
        val starting = take.starting
        val recorder = take.recorder
        val stopped = scope.async {
            starting?.await()
            recorder.stop()
        }
        stopping = stopped
        return stopped
    }

    /** 放弃交给电脑的识别：还在连的不连了，连上了的关掉。 */
    private fun closeRemote(take: Take) {
        val remote = take.remote ?: return
        remote.cancel()
        scope.launch { runCatching { remote.await() }.getOrNull()?.close() }
    }

    private fun fail(message: UiText, remedy: DictationRemedy?) {
        errorMessage = message
        this.remedy = remedy
    }

    private fun fail(error: Exception) {
        val remedy = when (error) {
            is NoModelAnywhere -> DictationRemedy.DownloadModel
            SpeechError.Silent, SpeechError.NoPermission -> DictationRemedy.MicrophonePermission
            else -> null
        }
        fail(message(error), remedy)
    }

    companion object {
        internal val hold: Duration = 350.milliseconds

        internal fun message(error: Exception): UiText = when (error) {
            SpeechError.Silent -> UiText.res(R.string.dictation_silent)
            SpeechError.NoPermission -> UiText.res(R.string.dictation_no_permission)
            SpeechError.MicrophoneUnavailable -> UiText.res(R.string.dictation_no_microphone)
            SpeechError.ModelUnavailable -> UiText.res(R.string.dictation_model_incomplete)
            is SpeechError.CorruptDownload -> UiText.res(R.string.dictation_download_corrupt)
            is SpeechError.HttpStatus -> UiText.res(R.string.dictation_download_http, error.status)
            SpeechError.Unsupported -> UiText.res(R.string.dictation_unsupported)
            is NoModelAnywhere -> UiText.res(R.string.dictation_no_model_anywhere)
            is LinkFailure -> Presentation.linkFailure(error)
            else -> UiText.Raw(error.message ?: error.toString())
        }
    }
}

/** 录 [SpeechRecorder] 的录音。 */
private class SystemRecorder(private val recorder: SpeechRecorder) : DictationRecorder {
    override suspend fun start() = recorder.start()

    override suspend fun stop(): FloatArray = recorder.stop()
}

/** 没有 Context 时的录音器：一开始就报没有麦克风。 */
private object NoMicrophone : DictationRecorder {
    override suspend fun start() = throw SpeechError.MicrophoneUnavailable

    override suspend fun stop(): FloatArray = FloatArray(0)
}

/**
 * 交给电脑识别时的录音：连上电脑之前录到的先攒着，连上后一次补发，之后边录边发。超过 `maxSpeechSeconds` 的不发：定时结束
 * 总比录音晚到一点，多出的几块会让电脑拒掉整段。录音的线程和主线程都用它。
 */
private class Feed {
    private val lock = Any()
    private var connection: SpeechConnection? = null
    private val pending = mutableListOf<FloatArray>()

    /** 还能发的样本数。 */
    private var budget = maxSpeechSeconds * speechSampleRate

    fun push(chunk: FloatArray) {
        synchronized(lock) {
            val taken = if (chunk.size > budget) chunk.copyOf(budget) else chunk
            if (taken.isEmpty()) return
            budget -= taken.size
            val connection = connection
            if (connection != null) connection.send(taken) else pending += taken
        }
    }

    fun attach(connection: SpeechConnection) {
        synchronized(lock) {
            this.connection = connection
            pending.forEach(connection::send)
            pending.clear()
        }
    }
}
