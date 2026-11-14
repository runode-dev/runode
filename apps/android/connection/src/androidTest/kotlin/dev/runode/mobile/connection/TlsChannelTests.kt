package dev.runode.mobile.connection

import android.net.ssl.SSLSockets
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import dev.runode.mobile.protocol.CertificateFingerprint
import dev.runode.mobile.protocol.Frame
import dev.runode.mobile.protocol.FrameDecoder
import dev.runode.mobile.protocol.GateMessage
import dev.runode.mobile.protocol.GatePurpose
import dev.runode.mobile.protocol.GateRejection
import dev.runode.mobile.protocol.GateSignature
import java.net.InetAddress
import java.security.KeyStore
import java.security.MessageDigest
import java.security.cert.X509Certificate
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit
import javax.net.ssl.KeyManagerFactory
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLServerSocket
import javax.net.ssl.SSLSocket
import kotlin.concurrent.thread
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Test
import org.junit.runner.RunWith

/**
 * 证书固定：[TlsChannel] 只认配对时记下的叶子证书指纹，这是挡住冒充电脑的唯一一道防线。对着本机起一个用自签证书的
 * TLS 1.3 服务端真连一次，顺带核对两端导出的 exporter 一样。
 */
@RunWith(AndroidJUnit4::class)
class TlsChannelTests {
    private val server = Server()

    @After
    fun stop() = server.stop()

    private companion object {
        /** 已配对的设备私钥，服务端认它的公钥。 */
        val key = SoftwareDeviceKey.generate()
        val nonce = ByteArray(GateSignature.nonceLength) { it.toByte() }
    }

    @Test
    fun connectsWhenTheFingerprintMatches() = runBlocking {
        val channel = TlsChannel.open(server.target, server.fingerprint)
        try {
            assertEquals("127.0.0.1", channel.remoteAddress)
            val exporter = channel.exporter()
            assertEquals(GateSignature.exporterLength, exporter.size)
            assertContentEquals(server.exporter.get(5, TimeUnit.SECONDS), exporter)
        } finally {
            channel.close()
        }
    }

    /** 整个门禁跑在真的 TLS 上：服务端用自己那头导出的 exporter 验签名，对得上才放行。 */
    @Test
    fun passesTheGateOverRealTls() = runBlocking {
        val channel = TlsChannel.open(server.target, server.fingerprint)
        try {
            val outcome = GateClient.run(channel, GateCredential.Auth("00112233445566778899aabbccddeeff", key))
            assertEquals("00112233445566778899aabbccddeeff", outcome.deviceId)
            assertEquals("runode-test", outcome.hostName)
        } finally {
            channel.close()
        }
    }

    /** 主线程上关：不抛 NetworkOnMainThreadException，服务端读到连接结束。 */
    @Test
    fun closesOnTheMainThread() = runBlocking {
        val channel = TlsChannel.open(server.target, server.fingerprint)
        // 先收下服务端的 remote_challenge，它写完了才关，关的时候服务端已经在等着读。
        channel.receive()
        InstrumentationRegistry.getInstrumentation().runOnMainSync { channel.close() }
        assertEquals(true, server.ended.get(5, TimeUnit.SECONDS))
    }

    @Test
    fun refusesAnotherCertificate() = runBlocking {
        val other = server.fingerprint.bytes.also { it[0] = (it[0].toInt() xor 0xFF).toByte() }
        val wrong = CertificateFingerprint(other)!!
        val failure = assertFailsWith<LinkFailure> { TlsChannel.open(server.target, wrong) }
        assertEquals(LinkFailure.FingerprintMismatch, failure)
    }

    /** 本机上的 TLS 1.3 服务端：接下一条连接、握完手、导出 exporter，然后收着不回话，测试结束时关掉。 */
    private class Server {
        /** 测试用的自签证书和私钥（P-256，CN=runode-test，口令 `runode`），只在这里用。 */
        private val identity: KeyStore = KeyStore.getInstance("PKCS12").apply {
            val assets = InstrumentationRegistry.getInstrumentation().context.assets
            assets.open("tls-test.p12").use { load(it, PASSWORD) }
        }

        val fingerprint: CertificateFingerprint = run {
            val alias = identity.aliases().toList().single()
            val der = (identity.getCertificate(alias) as X509Certificate).encoded
            CertificateFingerprint(MessageDigest.getInstance("SHA-256").digest(der))!!
        }

        private val socket: SSLServerSocket = run {
            val keys = KeyManagerFactory.getInstance(KeyManagerFactory.getDefaultAlgorithm()).apply { init(identity, PASSWORD) }
            val context = SSLContext.getInstance("TLSv1.3").apply { init(keys.keyManagers, null, null) }
            (context.serverSocketFactory.createServerSocket(0, 1, InetAddress.getByName("127.0.0.1")) as SSLServerSocket)
                .apply { enabledProtocols = arrayOf("TLSv1.3") }
        }

        val target = TransportTarget.address("127.0.0.1", socket.localPort.toUShort())!!

        /** 服务端这头导出的 exporter；握手失败时以错误结束。 */
        val exporter = CompletableFuture<ByteArray>()

        /** 门禁时读到对面关了连接时为真。 */
        val ended = CompletableFuture<Boolean>()
        private val accepted = mutableListOf<SSLSocket>()

        init {
            thread(isDaemon = true) {
                try {
                    val connection = socket.accept() as SSLSocket
                    synchronized(accepted) { accepted += connection }
                    connection.startHandshake()
                    val exported = SSLSockets.exportKeyingMaterial(
                        connection,
                        GateSignature.exporterLabel,
                        null,
                        GateSignature.exporterLength,
                    ) ?: error("the server cannot export keying material")
                    exporter.complete(exported)
                    gate(connection, exported)
                } catch (error: Exception) {
                    exporter.completeExceptionally(error)
                }
            }
        }

        /** 照宿主的门禁发 `remote_challenge`，按这头的 exporter 验 `remote_auth` 的签名后放行或拒绝。 */
        private fun gate(connection: SSLSocket, exported: ByteArray) {
            val output = connection.outputStream
            output.write(Frame.control(GateMessage.Challenge(1u, nonce, "runode-test")).encoded())
            output.flush()
            val decoder = FrameDecoder(maxPayload = Frame.gateMaxPayload)
            val buffer = ByteArray(4096)
            var frame: Frame? = null
            while (frame == null) {
                val count = try {
                    connection.inputStream.read(buffer)
                } catch (_: java.io.IOException) {
                    -1
                }
                if (count < 0) {
                    ended.complete(true)
                    return
                }
                decoder.append(buffer, 0, count)
                frame = decoder.next()
            }
            val auth = frame.message(GateMessage.serializer()) as GateMessage.Auth
            val payload = GateSignature.payload(nonce, exported, GatePurpose.Auth)
            val reply = if (verifyP256(key.publicKeyX963, payload, auth.signature)) {
                GateMessage.Accepted(auth.deviceId)
            } else {
                GateMessage.Rejected(GateRejection.BadSignature)
            }
            output.write(Frame.control(reply).encoded())
            output.flush()
        }

        fun stop() {
            socket.close()
            synchronized(accepted) { accepted.forEach { it.close() } }
        }

        companion object {
            val PASSWORD = "runode".toCharArray()
        }
    }
}
