package dev.runode.mobile.speech

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
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
        val record = try {
            AudioRecord.Builder()
                // 给识别用的音源：系统不做降噪、自动增益这类为通话做的处理。
                .setAudioSource(MediaRecorder.AudioSource.VOICE_RECOGNITION)
                .setAudioFormat(format)
                .setBufferSizeInBytes(maxOf(minimum, chunkSamples * Float.SIZE_BYTES * 4))
                .build()
        } catch (_: SecurityException) {
            throw SpeechError.NoPermission
        } catch (_: UnsupportedOperationException) {
            throw SpeechError.MicrophoneUnavailable
        }
        if (record.state != AudioRecord.STATE_INITIALIZED) {
            record.release()
            throw SpeechError.MicrophoneUnavailable
        }
        try {
            record.startRecording()
        } catch (_: IllegalStateException) {
            record.release()
            throw SpeechError.MicrophoneUnavailable
        }
        if (record.recordingState != AudioRecord.RECORDSTATE_RECORDING) {
            record.release()
            throw SpeechError.MicrophoneUnavailable
        }
        this.record = record
        reader = thread(name = "runode-speech-recorder") { read(record) }
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
            record.release()
        }
        record = null
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
