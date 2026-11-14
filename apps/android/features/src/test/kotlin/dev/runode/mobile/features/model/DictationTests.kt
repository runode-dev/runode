package dev.runode.mobile.features.model

import dev.runode.mobile.features.R
import dev.runode.mobile.speech.SpeechError
import dev.runode.mobile.speech.SpeechModels
import dev.runode.mobile.terminal.DictationPress
import java.io.File
import java.nio.file.Files
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlin.test.fail
import kotlin.time.Duration.Companion.milliseconds
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.delay
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.testTimeSource
import org.junit.Rule

/** 麦克风键：按住说话、点一下开始再点一下结束、滑出去取消；识别出的文字交给开始时给的 `insert`。 */
@OptIn(ExperimentalCoroutinesApi::class)
class DictationTests {
    @get:Rule val main = MainDispatcherRule()

    private class FakeRecorder : DictationRecorder {
        var started = false
        var stopped = false

        override suspend fun start() {
            started = true
        }

        override suspend fun stop(): FloatArray {
            stopped = true
            return floatArrayOf(0.1f, 0.2f)
        }
    }

    private val recorder = FakeRecorder()
    private val root: File = Files.createTempDirectory("dictation").toFile()

    @AfterTest fun tearDown() {
        root.deleteRecursively()
    }

    private fun TestScope.model(
        installed: Boolean = true,
        permitted: Boolean = true,
        download: suspend (File, Boolean, (Long) -> Unit) -> Unit = { _, _, _ -> },
    ): DictationModel = DictationModel(
        root = root,
        openSpeech = null,
        makeRecorder = { recorder },
        transcribe = { "第${it.size}段" },
        permitted = { permitted },
        isInstalled = { installed },
        downloadModels = download,
    ).also { it.timeSource = testTimeSource }

    @Test fun holdingThenReleasingTranscribes() = runTest {
        val model = model()
        val typed = mutableListOf<String>()
        model.press(DictationPress.Down, null, DictationSource.Phone) { typed += it }
        assertEquals(DictationModel.Phase.Recording, model.phase)
        runCurrent()
        assertTrue(recorder.started)
        delay(DictationModel.hold + 50.milliseconds)
        model.press(DictationPress.Up, null, DictationSource.Phone) {}
        assertEquals(DictationModel.Phase.Transcribing, model.phase)
        runCurrent()
        assertEquals(DictationModel.Phase.Idle, model.phase)
        assertEquals(listOf("第2段"), typed)
    }

    /** 点得快就一直录，再按一次才结束，松开那一下不算。 */
    @Test fun aTapKeepsRecordingUntilTheNextPress() = runTest {
        val model = model()
        val typed = mutableListOf<String>()
        model.press(DictationPress.Down, null, DictationSource.Phone) { typed += it }
        model.press(DictationPress.Up, null, DictationSource.Phone) {}
        assertEquals(DictationModel.Phase.Recording, model.phase)
        model.press(DictationPress.Down, null, DictationSource.Phone) {}
        assertEquals(DictationModel.Phase.Transcribing, model.phase)
        model.press(DictationPress.Up, null, DictationSource.Phone) {}
        runCurrent()
        assertEquals(DictationModel.Phase.Idle, model.phase)
        assertEquals(listOf("第2段"), typed)
    }

    @Test fun slidingOffCancels() = runTest {
        val model = model()
        model.press(DictationPress.Down, null, DictationSource.Phone) { fail("nothing should be typed") }
        model.press(DictationPress.Cancel, null, DictationSource.Phone) {}
        assertEquals(DictationModel.Phase.Idle, model.phase)
        advanceUntilIdle()
        assertTrue(recorder.stopped)
    }

    /** 录满上限自己结束。 */
    @Test fun recordingStopsAtTheLimit() = runTest {
        val model = model()
        val typed = mutableListOf<String>()
        model.press(DictationPress.Down, null, DictationSource.Phone) { typed += it }
        model.press(DictationPress.Up, null, DictationSource.Phone) {}
        advanceUntilIdle()
        assertEquals(DictationModel.Phase.Idle, model.phase)
        assertEquals(listOf("第2段"), typed)
    }

    /** 手机上没模型、又没有电脑可交时不录，说清楚要去下载；错误清掉时补救跟着清掉。 */
    @Test fun withoutAnyModelItAsksToDownload() = runTest {
        val model = model(installed = false)
        model.press(DictationPress.Down, null, DictationSource.Phone) {}
        runCurrent()
        assertEquals(DictationModel.Phase.Idle, model.phase)
        assertFalse(recorder.started)
        assertEquals(UiText.res(R.string.dictation_download_model_first), model.errorMessage)
        assertEquals(DictationRemedy.DownloadModel, model.remedy)
        model.errorMessage = null
        assertNull(model.remedy)
    }

    /** 没有麦克风权限时这一下只问权限；用户不给时说去系统设置里打开。 */
    @Test fun withoutPermissionItOnlyAsks() = runTest {
        val model = model(permitted = false)
        val answer = CompletableDeferred<Boolean>()
        model.requestPermission = { answer.await() }
        model.press(DictationPress.Down, null, DictationSource.Phone) {}
        runCurrent()
        assertEquals(DictationModel.Phase.Idle, model.phase)
        assertFalse(recorder.started)
        assertNull(model.errorMessage)
        answer.complete(false)
        runCurrent()
        assertEquals(UiText.res(R.string.dictation_no_permission), model.errorMessage)
        assertEquals(DictationRemedy.MicrophonePermission, model.remedy)
    }

    /** 录音器开不起来时回到空闲，说出原因。 */
    @Test fun aRecorderThatFailsToStartResets() = runTest {
        val model = DictationModel(
            root = root,
            openSpeech = null,
            makeRecorder = {
                object : DictationRecorder {
                    override suspend fun start() = throw SpeechError.MicrophoneUnavailable

                    override suspend fun stop(): FloatArray = FloatArray(0)
                }
            },
            transcribe = { fail("nothing to transcribe") },
            permitted = { true },
            isInstalled = { true },
        )
        model.press(DictationPress.Down, null, DictationSource.Phone) {}
        runCurrent()
        assertEquals(DictationModel.Phase.Idle, model.phase)
        assertEquals(UiText.res(R.string.dictation_no_microphone), model.errorMessage)
        assertNull(model.remedy)
    }

    /** 下载按百分比报进度，下完算装好；核对不过时说出来。 */
    @Test fun downloadingInstallsTheModel() = runTest {
        val gate = CompletableDeferred<Unit>()
        val model = model(installed = false, download = { _, _, progress ->
            progress(SpeechModels.downloadSize / 3)
            // 还是同一个百分比，不刷新。
            progress(SpeechModels.downloadSize / 3 + 1000)
            gate.await()
        })
        model.startDownload()
        runCurrent()
        assertEquals(DictationModel.Download.Running(SpeechModels.downloadSize / 3), model.download)
        gate.complete(Unit)
        runCurrent()
        assertEquals(DictationModel.Download.Idle, model.download)
        assertTrue(model.installed)

        val failing = model(installed = false, download = { _, _, _ -> throw SpeechError.CorruptDownload("tokens.txt") })
        failing.startDownload()
        runCurrent()
        assertEquals(DictationModel.Download.Failed(UiText.res(R.string.dictation_download_corrupt)), failing.download)
        assertFalse(failing.installed)
        assertEquals(
            "A downloaded model file failed verification. Try again.",
            English.render((failing.download as DictationModel.Download.Failed).reason),
        )
    }

    @Test fun cancellingTheDownloadGoesBackToIdle() = runTest {
        val model = model(installed = false, download = { _, _, _ -> CompletableDeferred<Unit>().await() })
        model.startDownload()
        runCurrent()
        model.cancelDownload()
        runCurrent()
        assertEquals(DictationModel.Download.Idle, model.download)
        assertFalse(model.installed)
    }
}
