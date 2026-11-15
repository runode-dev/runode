package dev.runode.mobile.speech

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioRecord
import android.media.MediaRecorder
import android.media.audiofx.AcousticEchoCanceler
import kotlin.concurrent.thread
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * 录麦克风，交出 [sampleRate] 的单声道样本（-1 到 1 的 float）。边录边把新的一段交给 [onChunk]（交给电脑识别时边录边发，
 * 在录音的线程上调），停下时交出整段。录音要 `RECORD_AUDIO` 权限，向用户要权限是界面层的事；没有权限时 [start] 报
 * [SpeechError.NoPermission]。
 */
class SpeechRecorder(
    private val context: Context,
    private val onChunk: ((FloatArray) -> Unit)? = null,
) {
    private var record: AudioRecord? = null
    private var reader: Thread? = null
    private var canceler: AcousticEchoCanceler? = null
    private val audio = context.getSystemService(AudioManager::class.java)

    /**
     * 录音期间独占的音频焦点：别的 App 在放的声音暂停，不然扬声器放出来的也会被麦克风收进去、一起转写；
     * 交还后它们接着放。
     */
    private val focus = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT_EXCLUSIVE)
        .setAudioAttributes(
            AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_ASSISTANT)
                .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                .build(),
        )
        .build()

    /** 要不到焦点也照样录：只是别的声音可能被一起转写。 */
    private fun requestFocus() {
        audio.requestAudioFocus(focus)
    }

    private fun abandonFocus() {
        audio.abandonAudioFocusRequest(focus)
    }
    private val lock = Any()
    private var samples = FloatArray(0)
    private var count = 0

    /** 开始录。 */
    suspend fun start() = withContext(queue) { startNow() }

    /** 停下，交出整段录音；样本全是 0 时报 [SpeechError.Silent]。 */
    suspend fun stop(): FloatArray = withContext(queue) { stopNow() }

    private fun startNow() {
        if (!permitted(context)) throw SpeechError.NoPermission
        val minimum = AudioRecord.getMinBufferSize(sampleRate, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_FLOAT)
        if (minimum <= 0) throw SpeechError.MicrophoneUnavailable
        val format = AudioFormat.Builder()
            .setSampleRate(sampleRate)
            .setChannelMask(AudioFormat.CHANNEL_IN_MONO)
            .setEncoding(AudioFormat.ENCODING_PCM_FLOAT)
            .build()
        val bufferSize = maxOf(minimum, chunkSamples * Float.SIZE_BYTES * 4)
        // 通话音源上系统做回声消除，去掉此刻设备自己放出来的声音；建不起来时退回给识别用的音源，不消回声。
        val record = open(MediaRecorder.AudioSource.VOICE_COMMUNICATION, format, bufferSize)
            ?: open(MediaRecorder.AudioSource.VOICE_RECOGNITION, format, bufferSize)
            ?: throw SpeechError.MicrophoneUnavailable
        // 有的机型通话音源本身不消回声，另挂一个；挂不上不影响录音。
        val canceler = if (AcousticEchoCanceler.isAvailable()) {
            runCatching { AcousticEchoCanceler.create(record.audioSessionId)?.also { it.enabled = true } }.getOrNull()
        } else {
            null
        }
        requestFocus()
        val started = try {
            record.startRecording()
            record.recordingState == AudioRecord.RECORDSTATE_RECORDING
        } catch (_: IllegalStateException) {
            false
        }
        if (!started) {
            canceler?.release()
            record.release()
            abandonFocus()
            throw SpeechError.MicrophoneUnavailable
        }
        this.record = record
        this.canceler = canceler
        reader = thread(name = "runode-speech-recorder") { read(record) }
    }

    /** 用 [source] 建一个初始化好的录音；这个音源建不起来时返回 null。 */
    private fun open(source: Int, format: AudioFormat, bufferSize: Int): AudioRecord? {
        val record = try {
            AudioRecord.Builder()
                .setAudioSource(source)
                .setAudioFormat(format)
                .setBufferSizeInBytes(bufferSize)
                .build()
        } catch (_: SecurityException) {
            throw SpeechError.NoPermission
        } catch (_: UnsupportedOperationException) {
            return null
        }
        if (record.state != AudioRecord.STATE_INITIALIZED) {
            record.release()
            return null
        }
        return record
    }

    /** 录音的线程：一块块读到 [AudioRecord.stop] 为止。 */
    private fun read(record: AudioRecord) {
        val buffer = FloatArray(chunkSamples)
        while (true) {
            val read = record.read(buffer, 0, buffer.size, AudioRecord.READ_BLOCKING)
            if (read <= 0) break
            val chunk = buffer.copyOf(read)
            synchronized(lock) {
                if (count + read > samples.size) samples = samples.copyOf(maxOf(count + read, samples.size * 2))
                chunk.copyInto(samples, count)
                count += read
            }
            onChunk?.invoke(chunk)
        }
    }

    private fun stopNow(): FloatArray {
        record?.let { record ->
            try {
                record.stop()
            } catch (_: IllegalStateException) {
            }
            reader?.join()
            canceler?.release()
            record.release()
            abandonFocus()
        }
        record = null
        canceler = null
        reader = null
        val samples = synchronized(lock) { samples.copyOf(count) }
        if (samples.isNotEmpty() && samples.all { it == 0f }) throw SpeechError.Silent
        return samples
    }

    companion object {
        /** 交出的采样率，识别模型和交给电脑的录音都是这个。 */
        const val sampleRate = 16_000

        /** 一次交给 [onChunk] 的样本数，0.1 秒。 */
        private const val chunkSamples = sampleRate / 10

        /** 所有录音器的启停按调用的先后排在这一条线上做：上一段的停下不会排到下一段的开始之后。 */
        private val queue = Dispatchers.IO.limitedParallelism(1)

        /** 有没有录音权限。 */
        fun permitted(context: Context): Boolean =
            context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED
    }
}
