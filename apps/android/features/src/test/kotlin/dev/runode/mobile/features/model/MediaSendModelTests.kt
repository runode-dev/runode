package dev.runode.mobile.features.model

import dev.runode.mobile.connection.LinkFailure
import dev.runode.mobile.connection.LinkState
import dev.runode.mobile.connection.MachineRecord
import dev.runode.mobile.connection.MediaFailure
import dev.runode.mobile.features.R
import dev.runode.mobile.protocol.ClientMsg
import dev.runode.mobile.protocol.maxMediaBytes
import java.io.ByteArrayInputStream
import java.io.File
import java.io.RandomAccessFile
import java.nio.file.Files
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.After
import org.junit.Rule

/**
 * 发图片和视频：图片处理过、视频压过，经一条连接存到电脑上，再按先后各发一条 `PastePath`；还没发完时再来的一批排在后面；
 * 电脑太旧、存不了、文件太大时报错。
 */
@OptIn(ExperimentalCoroutinesApi::class)
class MediaSendModelTests {
    @get:Rule val main = MainDispatcherRule()

    /** 假的连接：记下收到的文件内容，回 `/tmp/<序号>`。 */
    private class FakeUploader {
        val received = mutableListOf<ByteArray>()

        /** 每次报进度后记下模型当时的阶段。 */
        val phases = mutableListOf<MediaSendModel.Phase>()
        var model: MediaSendModel? = null

        /** 不为空时下一个文件报这个错，报过就清掉。 */
        var failNext: Exception? = null
        var opened = 0
        var closed = 0

        /** 不为空时每个文件等它完成再收。 */
        var gate: CompletableDeferred<Unit>? = null

        fun connection(): MediaUploader {
            opened += 1
            return object : MediaUploader {
                override suspend fun send(data: ByteArray, progress: (sent: Long, total: Long) -> Unit): String = receive(data, progress)

                override suspend fun send(file: File, progress: (sent: Long, total: Long) -> Unit): String =
                    receive(file.readBytes(), progress)

                override fun close() {
                    closed += 1
                }
            }
        }

        private suspend fun receive(data: ByteArray, progress: (sent: Long, total: Long) -> Unit): String {
            gate?.await()
            failNext?.let {
                failNext = null
                throw it
            }
            // 进度只看比例：按两块报，先报一半、再报完。
            for (sent in 1L..2L) {
                progress(sent, 2L)
                model?.let { phases += it.phase }
            }
            received += data
            return "/tmp/${received.size}"
        }
    }

    /** 假的压视频：把 URI 的文字写进临时文件当压好的视频；[gate] 不为空时等它完成。 */
    private class FakeVideoPreparer : VideoPreparer {
        val outputs = mutableListOf<File>()
        var gate: CompletableDeferred<Unit>? = null
        var failure: Exception? = null

        /** 不为空时交出这么大（稀疏）的文件。 */
        var length: Long? = null

        override suspend fun prepare(uri: String): File {
            gate?.await()
            failure?.let { throw it }
            val file = Files.createTempFile("video", ".mp4").toFile()
            outputs += file
            val size = length
            if (size == null) file.writeText(uri) else RandomAccessFile(file, "rw").use { it.setLength(size) }
            return file
        }
    }

    private val uploader = FakeUploader()
    private val videos = FakeVideoPreparer()
    private val opened = mutableListOf<MachineRecord>()
    private val machine = machineRecord()
    private val link = FakeLink().apply {
        state.value = LinkState.Connected("Ethan 的 MacBook Pro", "192.168.1.20")
        pastesPaths = true
    }

    @After fun removeTemporaryFiles() {
        videos.outputs.forEach { it.delete() }
    }

    /** 假的图片处理：每个字节加一，看得出处理过。 */
    private val preparer = ImagePreparer { raw -> ByteArray(raw.size) { (raw[it] + 1).toByte() } }

    private fun TestScope.model(prepare: ImagePreparer = preparer): MediaSendModel = MediaSendModel(
        openMedia = { machine ->
            opened += machine
            uploader.connection()
        },
        imagePreparer = prepare,
        videoPreparer = videos,
        work = main.dispatcher,
    ).also { uploader.model = it }

    private fun image(byte: Byte): OutgoingMedia = OutgoingMedia.Image { byteArrayOf(byte) }

    private fun images(vararg bytes: Byte): List<OutgoingMedia> = bytes.map(::image)

    private fun pastes() = link.sent.filterIsInstance<ClientMsg.PastePath>()

    @Test fun eachImageIsPastedInOrder() = runTest {
        val model = model()
        model.send(images(1, 2, 3), machine, link, sessionA)
        assertEquals(MediaSendModel.Phase.Sending(1, 3, 0f, video = false), model.phase)
        advanceUntilIdle()
        assertEquals(MediaSendModel.Phase.Idle, model.phase)
        assertEquals(listOf(machine), opened)
        assertEquals(listOf(2, 3, 4), uploader.received.map { it.single().toInt() })
        assertEquals(listOf("/tmp/1", "/tmp/2", "/tmp/3"), pastes().map { it.path })
        assertTrue(pastes().all { it.id == sessionA })
        assertEquals(3, pastes().map { it.req }.toSet().size)
        assertEquals(1, uploader.closed)
        assertNull(model.errorMessage)
    }

    @Test fun showsWhichItemIsBeingSentAndQueuesTheNextBatch() = runTest {
        val model = model()
        val gate = CompletableDeferred<Unit>()
        uploader.gate = gate
        model.send(images(1, 2), machine, link, sessionA)
        runCurrent()
        assertEquals(MediaSendModel.Phase.Sending(1, 2, 0f, video = false), model.phase)
        assertTrue(pastes().isEmpty())
        // 发着的时候再挑的排在后面，这一批发完接着发。
        model.send(images(9), machine, link, sessionB)
        runCurrent()
        assertEquals(MediaSendModel.Phase.Sending(1, 2, 0f, video = false), model.phase)
        gate.complete(Unit)
        advanceUntilIdle()
        assertEquals(listOf(sessionA, sessionA, sessionB), pastes().map { it.id })
        assertEquals(listOf(2, 3, 10), uploader.received.map { it.single().toInt() })
        assertEquals(
            listOf<MediaSendModel.Phase>(
                MediaSendModel.Phase.Sending(1, 2, 0.5f, video = false),
                MediaSendModel.Phase.Sending(1, 2, 1f, video = false),
                MediaSendModel.Phase.Sending(2, 2, 0.5f, video = false),
                MediaSendModel.Phase.Sending(2, 2, 1f, video = false),
                MediaSendModel.Phase.Sending(1, 1, 0.5f, video = false),
                MediaSendModel.Phase.Sending(1, 1, 1f, video = false),
            ),
            uploader.phases,
        )
        // 每一批各开一条连接，发完关掉。
        assertEquals(2, uploader.opened)
        assertEquals(2, uploader.closed)
        assertEquals(MediaSendModel.Phase.Idle, model.phase)
    }

    @Test fun aFailedBatchDoesNotStopTheQueuedOne() = runTest {
        val model = model()
        val gate = CompletableDeferred<Unit>()
        uploader.gate = gate
        uploader.failNext = MediaFailure("disk full")
        model.send(images(1, 2), machine, link, sessionA)
        model.send(images(3), machine, link, sessionB)
        gate.complete(Unit)
        advanceUntilIdle()
        assertEquals(listOf(sessionB), pastes().map { it.id })
        assertEquals(UiText.res(R.string.media_failed_on_computer, "disk full"), model.errorMessage)
    }

    @Test fun aVideoIsCompressedThenStreamedAndTheTemporaryFileRemoved() = runTest {
        val model = model()
        val gate = CompletableDeferred<Unit>()
        videos.gate = gate
        model.send(listOf(OutgoingMedia.Video("content://media/video/7")), machine, link, sessionA)
        assertEquals(MediaSendModel.Phase.Preparing(1, 1), model.phase)
        runCurrent()
        assertEquals(MediaSendModel.Phase.Preparing(1, 1), model.phase)
        gate.complete(Unit)
        advanceUntilIdle()
        assertContentEquals("content://media/video/7".encodeToByteArray(), uploader.received.single())
        assertEquals(
            listOf<MediaSendModel.Phase>(
                MediaSendModel.Phase.Sending(1, 1, 0.5f, video = true),
                MediaSendModel.Phase.Sending(1, 1, 1f, video = true),
            ),
            uploader.phases,
        )
        assertEquals(listOf("/tmp/1"), pastes().map { it.path })
        assertFalse(videos.outputs.single().exists())
        assertEquals(MediaSendModel.Phase.Idle, model.phase)
        assertNull(model.errorMessage)
    }

    @Test fun imagesAndVideosAreSentInTheOrderPicked() = runTest {
        val model = model()
        val items = listOf(image(1), OutgoingMedia.Video("v1"), image(5), OutgoingMedia.Video("v2"))
        model.send(items, machine, link, sessionA)
        advanceUntilIdle()
        assertEquals(
            listOf(listOf<Byte>(2), "v1".encodeToByteArray().toList(), listOf<Byte>(6), "v2".encodeToByteArray().toList()),
            uploader.received.map { it.toList() },
        )
        assertEquals(listOf("/tmp/1", "/tmp/2", "/tmp/3", "/tmp/4"), pastes().map { it.path })
        assertEquals(1, uploader.opened)
        assertTrue(videos.outputs.none { it.exists() })
    }

    @Test fun aVideoThatStaysTooLargeIsNotSent() = runTest {
        videos.length = maxMediaBytes.toLong() + 1
        val model = model()
        model.send(listOf(OutgoingMedia.Video("big"), image(1)), machine, link, sessionA)
        advanceUntilIdle()
        assertTrue(uploader.received.isEmpty())
        assertTrue(pastes().isEmpty())
        assertEquals(UiText.res(R.string.media_video_too_large), model.errorMessage)
        assertFalse(videos.outputs.single().exists())
    }

    @Test fun aVideoThatCannotBeConvertedIsReported() = runTest {
        videos.failure = IllegalStateException("no decoder")
        val model = model()
        model.send(listOf(OutgoingMedia.Video("broken")), machine, link, sessionA)
        advanceUntilIdle()
        assertTrue(pastes().isEmpty())
        assertEquals(UiText.res(R.string.media_video_unreadable), model.errorMessage)
    }

    @Test fun atMostFiveItemsAreSent() = runTest {
        val model = model()
        model.send(images(1, 2, 3, 4, 5, 6, 7), machine, link, sessionA)
        assertEquals(MediaSendModel.Phase.Sending(1, MediaSendModel.maxItems, 0f, video = false), model.phase)
        advanceUntilIdle()
        assertEquals(MediaSendModel.maxItems, pastes().size)
    }

    @Test fun anOldComputerIsReported() = runTest {
        link.pastesPaths = false
        val model = model()
        assertFalse(model.checkReady(link))
        assertEquals(UiText.res(R.string.media_computer_outdated), model.errorMessage)
        assertEquals(MediaSendRemedy.UpdateComputer, model.remedy)
        assertEquals("电脑上的 Runode 版本太旧，更新后才能发送图片和视频", model.errorMessage.simplifiedChinese)
        model.errorMessage = null
        assertNull(model.remedy)

        model.send(images(1), machine, link, sessionA)
        advanceUntilIdle()
        assertEquals(MediaSendModel.Phase.Idle, model.phase)
        assertTrue(opened.isEmpty())
        assertTrue(pastes().isEmpty())
        assertEquals(MediaSendRemedy.UpdateComputer, model.remedy)
    }

    @Test fun nothingIsSentWhileDisconnected() = runTest {
        link.state.value = LinkState.Idle
        val model = model()
        model.send(images(1), machine, link, sessionA)
        advanceUntilIdle()
        assertTrue(opened.isEmpty())
        assertEquals(UiText.res(R.string.not_connected), model.errorMessage)
        assertNull(model.remedy)
    }

    /** 存好一项时终端的连接断着（App 退到后台），重新连上后照样打进会话，接着发后面的。 */
    @Test fun aPathWaitsForTheTerminalToReconnect() = runTest {
        val model = model()
        val gate = CompletableDeferred<Unit>()
        uploader.gate = gate
        model.send(images(1, 2), machine, link, sessionA)
        runCurrent()
        link.state.value = LinkState.Idle
        gate.complete(Unit)
        advanceTimeBy(MediaSendModel.reconnectTimeout - 1.seconds)
        assertEquals(1, uploader.received.size)
        assertTrue(pastes().isEmpty())
        link.state.value = LinkState.Connected("Ethan 的 MacBook Pro", "192.168.1.20")
        advanceUntilIdle()
        assertEquals(listOf("/tmp/1", "/tmp/2"), pastes().map { it.path })
        assertNull(model.errorMessage)
        assertEquals(MediaSendModel.Phase.Idle, model.phase)
    }

    /** 终端的连接一直没重新连上：存好的那项不打进去，报连接断了，后面的不发。 */
    @Test fun aTerminalThatStaysDisconnectedStopsTheBatch() = runTest {
        val model = model()
        val gate = CompletableDeferred<Unit>()
        uploader.gate = gate
        model.send(images(1, 2), machine, link, sessionA)
        runCurrent()
        link.state.value = LinkState.Idle
        gate.complete(Unit)
        advanceUntilIdle()
        assertEquals(1, uploader.received.size)
        assertTrue(pastes().isEmpty())
        assertEquals(MediaSendModel.message(LinkFailure.HostClosed), model.errorMessage)
        assertEquals(1, uploader.closed)
        assertEquals(MediaSendModel.Phase.Idle, model.phase)
    }

    @Test fun aFailureOnTheComputerStopsTheBatch() = runTest {
        uploader.failNext = MediaFailure("disk full")
        val model = model()
        model.send(images(1, 2), machine, link, sessionA)
        advanceUntilIdle()
        assertEquals(MediaSendModel.Phase.Idle, model.phase)
        assertTrue(pastes().isEmpty())
        assertEquals(UiText.res(R.string.media_failed_on_computer, "disk full"), model.errorMessage)
        assertEquals(1, uploader.closed)
    }

    @Test fun anUnreadableImageIsReported() = runTest {
        val model = model { throw IllegalArgumentException("not an image") }
        model.send(images(1), machine, link, sessionA)
        advanceUntilIdle()
        assertTrue(pastes().isEmpty())
        assertEquals(UiText.res(R.string.media_image_unreadable), model.errorMessage)
    }

    @Test fun anImageTooLargeToReadIsNotSent() = runTest {
        val model = model()
        val tooLarge = OutgoingMedia.Image { MediaSendModel.readImage(ByteArrayInputStream(ByteArray(11)), limit = 10) }
        model.send(listOf(tooLarge), machine, link, sessionA)
        advanceUntilIdle()
        assertTrue(uploader.received.isEmpty())
        assertEquals(UiText.res(R.string.media_image_too_large), model.errorMessage)
    }

    @Test fun readingAnImageStopsAtTheLimit() {
        assertContentEquals(ByteArray(10) { it.toByte() }, MediaSendModel.readImage(ByteArrayInputStream(ByteArray(10) { it.toByte() }), 10))
        assertFailsWith<MediaTooLarge> { MediaSendModel.readImage(ByteArrayInputStream(ByteArray(11)), 10) }
    }

    @Test fun withoutAnOpenerNothingHappens() = runTest {
        val model = MediaSendModel(openMedia = null, imagePreparer = preparer, videoPreparer = videos, work = main.dispatcher)
        assertFalse(model.available)
        model.send(images(1), machine, link, sessionA)
        advanceUntilIdle()
        assertEquals(MediaSendModel.Phase.Idle, model.phase)
        assertNull(model.errorMessage)
        assertTrue(pastes().isEmpty())
    }
}
