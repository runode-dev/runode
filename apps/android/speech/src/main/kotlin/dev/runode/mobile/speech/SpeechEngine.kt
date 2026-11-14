package dev.runode.mobile.speech

import com.k2fsa.sherpa.onnx.FeatureConfig
import com.k2fsa.sherpa.onnx.OfflineModelConfig
import com.k2fsa.sherpa.onnx.OfflineRecognizer
import com.k2fsa.sherpa.onnx.OfflineRecognizerConfig
import com.k2fsa.sherpa.onnx.OfflineSenseVoiceModelConfig
import com.k2fsa.sherpa.onnx.SileroVadModelConfig
import com.k2fsa.sherpa.onnx.Vad
import com.k2fsa.sherpa.onnx.VadModelConfig
import java.io.File
import kotlin.time.Duration
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

/**
 * 手机上的识别：第一次用时加载 [SpeechModels] 下好的模型，闲了 [idleUnload] 之后卸掉，模型加载后占几百 MB 内存。同一时刻
 * 只识别一段，加载和识别都不在调用的线程上做。
 */
class SpeechEngine(private val root: File) {
    private val lock = Mutex()
    private var recognizer: Recognizer? = null
    private var unload: Job? = null
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    /** 识别 [samples]（[SpeechRecorder.sampleRate] 的单声道）；没听出话来时是空的。 */
    suspend fun transcribe(samples: FloatArray): String = lock.withLock {
        unload?.cancel()
        val text = withContext(Dispatchers.Default) {
            val recognizer = recognizer ?: Recognizer.load(root)
            this@SpeechEngine.recognizer = recognizer
            recognizer.transcribe(samples)
        }
        unload = scope.launch {
            delay(idleUnload)
            lock.withLock {
                recognizer?.release()
                recognizer = null
            }
        }
        text
    }

    companion object {
        val idleUnload: Duration = 120.seconds
    }
}

/** 把一句接到前面的文字后面；两边都是西文时中间加空格，中文之间不加。 */
internal fun appendSentence(text: String, sentence: String): String {
    fun latin(codePoint: Int?): Boolean = codePoint != null && codePoint in 0x21..0x7E
    val last = if (text.isEmpty()) null else text.codePointBefore(text.length)
    val first = if (sentence.isEmpty()) null else sentence.codePointAt(0)
    return if (latin(last) && latin(first)) "$text $sentence" else text + sentence
}

/** SenseVoice 加 Silero VAD：先按停顿切成句，一句一句识别再接起来，长录音也不会整段塞进模型。不能同时在几条线程上用。 */
private class Recognizer private constructor(private val asr: OfflineRecognizer, private val vad: Vad) {
    fun transcribe(samples: FloatArray): String {
        vad.reset()
        var text = ""
        var at = 0
        while (at < samples.size) {
            val end = minOf(at + window, samples.size)
            vad.acceptWaveform(samples.copyOfRange(at, end))
            at = end
            text = drain(text)
        }
        vad.flush()
        return drain(text)
    }

    fun release() {
        vad.release()
        asr.release()
    }

    private fun drain(text: String): String {
        var result = text
        while (!vad.empty()) {
            val sentence = decode(vad.front().samples)
            if (sentence.isNotEmpty()) result = appendSentence(result, sentence)
            vad.pop()
        }
        return result
    }

    private fun decode(samples: FloatArray): String {
        val stream = asr.createStream()
        try {
            stream.acceptWaveform(samples, SpeechRecorder.sampleRate)
            asr.decode(stream)
            return asr.getResult(stream).text.trim()
        } finally {
            stream.release()
        }
    }

    companion object {
        private const val window = 512

        fun load(root: File): Recognizer {
            if (!SpeechModels.isInstalled(root)) throw SpeechError.ModelUnavailable
            val config = OfflineRecognizerConfig(
                featConfig = FeatureConfig(sampleRate = SpeechRecorder.sampleRate, featureDim = 80),
                modelConfig = OfflineModelConfig(
                    senseVoice = OfflineSenseVoiceModelConfig(
                        model = SpeechModels.senseVoice.path(root).path,
                        language = "auto",
                        // 加标点、数字写成阿拉伯数字。
                        useInverseTextNormalization = true,
                    ),
                    tokens = SpeechModels.tokens.path(root).path,
                    numThreads = minOf(4, Runtime.getRuntime().availableProcessors()),
                ),
                decodingMethod = "greedy_search",
            )
            // 加载不了 JNI 库（JVM 上的单元测试）时类的初始化就失败，报的是 LinkageError。
            val asr = try {
                OfflineRecognizer(config = config)
            } catch (_: LinkageError) {
                throw SpeechError.Unsupported
            } catch (_: IllegalArgumentException) {
                throw SpeechError.ModelUnavailable
            }
            val vadConfig = VadModelConfig(
                sileroVadModelConfig = SileroVadModelConfig(
                    model = SpeechModels.vad.path(root).path,
                    threshold = 0.5f,
                    minSilenceDuration = 0.5f,
                    minSpeechDuration = 0.25f,
                    windowSize = window,
                    maxSpeechDuration = 20f,
                ),
                sampleRate = SpeechRecorder.sampleRate,
                numThreads = 1,
            )
            val vad = try {
                Vad(config = vadConfig)
            } catch (_: IllegalArgumentException) {
                asr.release()
                throw SpeechError.ModelUnavailable
            }
            return Recognizer(asr, vad)
        }
    }
}
