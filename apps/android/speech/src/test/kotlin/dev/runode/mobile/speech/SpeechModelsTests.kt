package dev.runode.mobile.speech

import com.sun.net.httpserver.HttpServer
import java.io.File
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.URL
import java.nio.file.Files
import java.security.MessageDigest
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertTrue
import kotlinx.coroutines.test.runTest

class SpeechModelsTests {
    private val root: File = Files.createTempDirectory("speech").toFile()

    @AfterTest fun tearDown() {
        root.deleteRecursively()
    }

    @Test fun spacesOnlyBetweenLatinSentences() {
        val text = listOf("Hello.", "World.", "你好。", "好的", "OK.").fold("", ::appendSentence)
        assertEquals("Hello. World.你好。好的OK.", text)
    }

    /** 少一个文件就不算装好，删的时候整个目录一起删。 */
    @Test fun installedOnlyWithEveryFile() {
        for (asset in SpeechModels.assets.dropLast(1)) touch(asset.path(root))
        assertFalse(SpeechModels.isInstalled(root))
        touch(SpeechModels.vad.path(root))
        assertTrue(SpeechModels.isInstalled(root))
        SpeechModels.remove(root)
        assertTrue(root.list()!!.isEmpty())
    }

    @Test fun downloadSizeIsTheSumOfTheFiles() {
        assertEquals(239_233_841L + 315_894 + 1_807_522, SpeechModels.downloadSize)
    }

    /** 中文界面先试 ModelScope 上的镜像，别的先试 HuggingFace 上原作者的仓库。 */
    @Test fun chinaFirstTriesTheMirrorFirst() {
        val hf = URL("https://huggingface.co/csukuangfj/vad/resolve/main/silero_vad.onnx")
        val ms = URL("https://modelscope.cn/models/runode/speech-models/resolve/master/silero-vad/silero_vad.onnx")
        assertEquals(listOf(ms, hf), SpeechModels.vad.urls(chinaFirst = true))
        assertEquals(listOf(hf, ms), SpeechModels.vad.urls(chinaFirst = false))
    }

    /** 一个源给的文件核对不过、或者回了出错的状态码，就换下一个源；都不行时报最后一个源的错。 */
    @Test fun aBadSourceFallsBackToTheNext() = runTest {
        val body = "model".toByteArray()
        val asset = SpeechModels.Asset("dir", "model.onnx", "unused", body.size.toLong(), sha256(body))
        serve(mapOf("/good" to body, "/corrupt" to "modex".toByteArray())) { base ->
            var progress = 0L
            SpeechModels.download(root, listOf(asset), { listOf(URL("$base/corrupt"), URL("$base/good")) }) {
                progress = it
            }
            assertEquals("model", asset.path(root).readText())
            assertEquals(body.size.toLong(), progress)
            assertFalse(File(asset.path(root).parentFile, "model.onnx.part").exists())

            val other = SpeechModels.Asset("other", "model.onnx", "unused", body.size.toLong(), sha256(body))
            val error = assertFailsWith<SpeechError.HttpStatus> {
                SpeechModels.download(root, listOf(other), { listOf(URL("$base/corrupt"), URL("$base/missing")) }) {}
            }
            assertEquals(404, error.status)
            assertFalse(other.path(root).exists())
            assertFailsWith<SpeechError.CorruptDownload> {
                SpeechModels.download(root, listOf(other), { listOf(URL("$base/corrupt")) }) {}
            }
        }
    }

    /** 已经下好的文件不再下。 */
    @Test fun installedFilesAreSkipped() = runTest {
        val asset = SpeechModels.Asset("dir", "model.onnx", "unused", 5, "unused")
        touch(asset.path(root))
        var progress = 0L
        SpeechModels.download(root, listOf(asset), { error("should not fetch") }) { progress = it }
        assertEquals(0L, progress)
    }

    /** JVM 上加载不了 sherpa-onnx 的 JNI 库；模型不全时先报模型的问题。 */
    @Test fun recognitionIsUnsupportedOffDevice() = runTest {
        val engine = SpeechEngine(root)
        assertFailsWith<SpeechError.ModelUnavailable> { engine.transcribe(FloatArray(16)) }
        for (asset in SpeechModels.assets) touch(asset.path(root))
        assertFailsWith<SpeechError.Unsupported> { engine.transcribe(FloatArray(16)) }
    }

    private fun touch(file: File) {
        file.parentFile!!.mkdirs()
        file.writeBytes(ByteArray(0))
    }

    private fun sha256(bytes: ByteArray): String =
        MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }

    /** 在本机起一个 HTTP 服务，按路径回 [files] 里的内容，别的路径回 404。 */
    private suspend fun serve(files: Map<String, ByteArray>, body: suspend (String) -> Unit) {
        val server = HttpServer.create(InetSocketAddress(InetAddress.getLoopbackAddress(), 0), 0)
        server.createContext("/") { exchange ->
            val content = files[exchange.requestURI.path]
            if (content == null) {
                exchange.sendResponseHeaders(404, -1)
            } else {
                exchange.sendResponseHeaders(200, content.size.toLong())
                exchange.responseBody.use { it.write(content) }
            }
            exchange.close()
        }
        server.start()
        try {
            body("http://127.0.0.1:${server.address.port}")
        } finally {
            server.stop(0)
        }
    }
}
