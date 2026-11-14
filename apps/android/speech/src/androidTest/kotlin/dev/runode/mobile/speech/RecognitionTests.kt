package dev.runode.mobile.speech

import androidx.test.ext.junit.runners.AndroidJUnit4
import com.k2fsa.sherpa.onnx.OfflineRecognizer
import com.k2fsa.sherpa.onnx.OfflineRecognizerConfig
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue
import kotlinx.coroutines.runBlocking
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith

/**
 * 设备上真的加载 sherpa-onnx 识别。模型有几百 MB，不进仓库、测试里也不下载：先把 [SpeechModels] 的那几个文件按同样的目录
 * 布局、连同模型作者给的测试录音 `zh.wav`（16 kHz、16 位单声道）推到设备的 [root] 下，没推时跳过识别的那条。
 */
@RunWith(AndroidJUnit4::class)
class RecognitionTests {
    private val root = File("/data/local/tmp/runode-speech-models")

    /** 模型不全时报模型的问题，不碰 JNI 库。 */
    @Test fun missingModelIsReported() = runBlocking<Unit> {
        val empty = File.createTempFile("speech", null).also { it.delete() }
        assertFailsWith<SpeechError.ModelUnavailable> { SpeechEngine(empty).transcribe(FloatArray(16)) }
    }

    /** 打进 APK 的 JNI 库加载得了：没给模型的配置由原生那边拒掉，不是加载不了库。 */
    @Test fun nativeLibraryLoads() {
        assertFailsWith<IllegalArgumentException> { OfflineRecognizer(config = OfflineRecognizerConfig()) }
    }

    @Test fun senseVoiceTranscribesMandarin() = runBlocking {
        val wav = File(root, "zh.wav")
        assumeTrue("没有推模型和录音到 $root", SpeechModels.isInstalled(root) && wav.canRead())
        val text = SpeechEngine(root).transcribe(readWav(wav))
        // 锁住标点和阿拉伯数字：只有原版 SenseVoice 在 `use_itn` 时带这两样，换模型时会掉。
        assertTrue(text.contains("早上9点至下午5点。"), text)
    }

    /** 读 16 kHz、16 位单声道的 WAV：跳过 44 字节的文件头。 */
    private fun readWav(file: File): FloatArray {
        val pcm = ByteBuffer.wrap(file.readBytes(), 44, file.length().toInt() - 44)
            .order(ByteOrder.LITTLE_ENDIAN).asShortBuffer()
        return FloatArray(pcm.remaining()) { pcm.get(it) / 32768f }
    }
}
