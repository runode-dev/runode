package dev.runode.mobile.connection

import android.annotation.SuppressLint
import android.net.ssl.SSLSockets
import android.os.Looper
import android.os.ParcelFileDescriptor
import android.system.Os
import android.system.OsConstants
import dev.runode.mobile.protocol.CertificateFingerprint
import dev.runode.mobile.protocol.GateSignature
import java.io.IOException
import java.net.InetSocketAddress
import java.net.Socket
import java.net.SocketTimeoutException
import java.security.MessageDigest
import java.security.cert.CertificateException
import java.security.cert.X509Certificate
import java.util.concurrent.atomic.AtomicBoolean
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLEngine
import javax.net.ssl.SSLException
import javax.net.ssl.SSLSocket
import javax.net.ssl.X509ExtendedTrustManager
import kotlin.time.Duration
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.asExecutor
import kotlinx.coroutines.suspendCancellableCoroutine

/** 一条已经握好手的字节流。[TlsChannel] 是真的；测试用假的，不碰网络。 */
interface FrameTransport {
    /** 收下一块字节；对面正常关闭时返回空，出错时抛错。被取消时可以关掉连接。 */
    suspend fun receive(): ByteArray?

    /** 把字节排进发送队列，按调用的先后写出去；写失败时下一次 [receive] 会报错。 */
    fun send(data: ByteArray)

    /** TLS exporter（label [GateSignature.exporterLabel]、不带 context、[GateSignature.exporterLength] 字节），门禁签名用。 */
    fun exporter(): ByteArray

    /** 实际连上的对方地址（不含端口），记下来下次先试。 */
    val remoteAddress: String?

    /** 断开；之后的 [receive] 报错。 */
    fun close()
}

/** 要试着连的一个地方：Bonjour 找到的服务，或者一个地址加端口。[host] 是 IP 地址或主机名。 */
data class TransportTarget(
    val host: String,
    val port: UShort,
    /** 给人看、也用来去重。 */
    val label: String,
) {
    companion object {
        /** 一个 IPv4/IPv6 地址或主机名加端口；地址是空的时返回空。 */
        fun address(address: String, port: UShort): TransportTarget? =
            if (address.isEmpty()) null else TransportTarget(address, port, address)
    }
}

/** 打开一条到 target、指纹对得上的连接。真的实现是 [TlsChannel.open]，测试换成假的。 */
typealias TransportOpener = suspend (target: TransportTarget, fingerprint: CertificateFingerprint) -> FrameTransport

/** 从握好手的 TLS 连接导出 keying material（RFC 8446 §7.5）。JDK 没有这个接口，由平台给。 */
fun interface KeyingMaterialExporter {
    fun export(socket: SSLSocket, label: String, length: Int): ByteArray
}

/** Android 自带的 Conscrypt 导出：不带 context。 */
object ConscryptExporter : KeyingMaterialExporter {
    override fun export(socket: SSLSocket, label: String, length: Int): ByteArray {
        if (!SSLSockets.isSupportedSocket(socket)) throw LinkFailure.ProtocolViolation("the TLS socket cannot export keying material")
        return SSLSockets.exportKeyingMaterial(socket, label, null, length)
            ?: throw LinkFailure.ProtocolViolation("the TLS exporter returned nothing")
    }
}

/**
 * TLS 1.3 的连接。不走 CA 校验也不校验主机名：只比对叶子证书 DER 的 SHA-256 和配对时记下的指纹。读在调用方的
 * 协程里经 [Dispatchers.IO] 阻塞地读，取消时关掉连接让它返回；写排进一条只有一个线程的队列，按调用的先后写。
 */
class TlsChannel private constructor(
    private val socket: SSLSocket,
    private val keyingMaterial: KeyingMaterialExporter,
) : FrameTransport {
    private val writer = Dispatchers.IO.limitedParallelism(1).asExecutor()
    private val output = socket.outputStream
    private val input = socket.inputStream
    private val buffer = ByteArray(256 shl 10)

    override suspend fun receive(): ByteArray? = blocking(::close) {
        val count = try {
            input.read(buffer)
        } catch (error: IOException) {
            throw LinkFailure.Closed(error.message ?: error.toString())
        }
        if (count < 0) null else buffer.copyOf(count)
    }

    override fun send(data: ByteArray) {
        writer.execute {
            try {
                output.write(data)
                output.flush()
            } catch (_: IOException) {
                // 写不出去时关掉连接，卡着的读随之报错，主循环据此重连。
                close()
            }
        }
    }

    override fun exporter(): ByteArray {
        val bytes = try {
            keyingMaterial.export(socket, GateSignature.exporterLabel, GateSignature.exporterLength)
        } catch (error: SSLException) {
            throw LinkFailure.ProtocolViolation("cannot export TLS keying material: ${error.message}")
        }
        if (bytes.size != GateSignature.exporterLength) throw LinkFailure.ProtocolViolation("the TLS exporter has the wrong length")
        return bytes
    }

    override val remoteAddress: String? get() = socket.inetAddress?.hostAddress

    /**
     * 哪个线程上都能调。Conscrypt 关连接时要发 close_notify，主线程上做网络操作会抛 NetworkOnMainThreadException，
     * 所以主线程上交给 IO 线程关；不排在 [writer] 后面，卡住的写也挡不住关。
     */
    override fun close() {
        if (Looper.getMainLooper().isCurrentThread) Dispatchers.IO.asExecutor().execute(::closeSocket) else closeSocket()
    }

    private fun closeSocket() {
        try {
            socket.close()
        } catch (_: IOException) {
        }
    }

    companion object {
        /**
         * 连上 [target] 并做完 TLS 握手。[timeout] 内没握完手、证书指纹不对、对方不说 TLS 1.3 都报错。
         *
         * shortcut: 网络断了、换了网时连接不会马上关掉，要等 TCP keepalive 或重传超时才发现，要更快时按
         * ConnectivityManager 的回调关掉绑在旧网络上的连接。
         */
        suspend fun open(
            target: TransportTarget,
            fingerprint: CertificateFingerprint,
            timeout: Duration = 6.seconds,
            keyingMaterial: KeyingMaterialExporter = ConscryptExporter,
        ): TlsChannel {
            val mismatch = AtomicBoolean(false)
            val context = SSLContext.getInstance("TLSv1.3")
            context.init(null, arrayOf(PinnedTrustManager(fingerprint.bytes, mismatch)), null)
            val raw = Socket()
            return try {
                withLinkTimeout(timeout, onTimeout = { raw.close() }) {
                    blocking({ raw.close() }) {
                        raw.tcpNoDelay = true
                        raw.keepAlive = true
                        tuneKeepalive(raw)
                        raw.connect(InetSocketAddress(target.host, target.port.toInt()), timeout.inWholeMilliseconds.toInt())
                        val tls = context.socketFactory.createSocket(raw, target.host, target.port.toInt(), true) as SSLSocket
                        tls.enabledProtocols = arrayOf("TLSv1.3")
                        tls.soTimeout = timeout.inWholeMilliseconds.toInt()
                        tls.startHandshake()
                        tls.soTimeout = 0
                        TlsChannel(tls, keyingMaterial)
                    }
                }
            } catch (error: IOException) {
                raw.close()
                throw when {
                    mismatch.get() -> LinkFailure.FingerprintMismatch
                    error is SocketTimeoutException -> LinkFailure.Timeout
                    else -> LinkFailure.ConnectionFailed(error.message ?: error.toString())
                }
            } catch (error: Throwable) {
                raw.close()
                throw error
            }
        }

        /** 叶子证书 DER 的 SHA-256。 */
        internal fun leafFingerprint(chain: Array<out X509Certificate>?): ByteArray? =
            chain?.firstOrNull()?.let { MessageDigest.getInstance("SHA-256").digest(it.encoded) }

        /**
         * 手机静默消失（锁屏后被挂起、切网）时要能及时发现，和电脑那边的 keepalive 相当：空闲的连接靠 keepalive，
         * 有数据没送到的靠重传超时。设不上时照常连，只是发现得慢。
         */
        private fun tuneKeepalive(socket: Socket) {
            try {
                ParcelFileDescriptor.fromSocket(socket).use { descriptor ->
                    val fd = descriptor.fileDescriptor
                    Os.setsockoptInt(fd, OsConstants.IPPROTO_TCP, TCP_KEEPIDLE, 15)
                    Os.setsockoptInt(fd, OsConstants.IPPROTO_TCP, TCP_KEEPINTVL, 10)
                    Os.setsockoptInt(fd, OsConstants.IPPROTO_TCP, TCP_KEEPCNT, 4)
                    Os.setsockoptInt(fd, OsConstants.IPPROTO_TCP, TCP_USER_TIMEOUT, 60_000)
                }
            } catch (_: Exception) {
            }
        }

        // Linux 的 `<netinet/tcp.h>`，OsConstants 没有公开这几个。
        private const val TCP_KEEPIDLE = 4
        private const val TCP_KEEPINTVL = 5
        private const val TCP_KEEPCNT = 6
        private const val TCP_USER_TIMEOUT = 18
    }
}

/**
 * 只认指纹对得上的叶子证书；对不上时记下，好把握手失败报成 [LinkFailure.FingerprintMismatch]。电脑的证书是自签的，
 * 配对时经二维码记下指纹，系统的 CA 校验用不上。
 */
@SuppressLint("CustomX509TrustManager")
private class PinnedTrustManager(
    private val expected: ByteArray,
    private val mismatch: AtomicBoolean,
) : X509ExtendedTrustManager() {
    private fun check(chain: Array<out X509Certificate>?) {
        val actual = TlsChannel.leafFingerprint(chain)
        if (actual == null || !MessageDigest.isEqual(actual, expected)) {
            mismatch.set(true)
            throw CertificateException("certificate fingerprint mismatch")
        }
    }

    override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?) = check(chain)

    override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?, socket: Socket?) = check(chain)

    override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?, engine: SSLEngine?) = check(chain)

    override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?) =
        throw CertificateException("client certificates are not used")

    override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?, socket: Socket?) =
        throw CertificateException("client certificates are not used")

    override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?, engine: SSLEngine?) =
        throw CertificateException("client certificates are not used")

    override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
}

/**
 * 在 [Dispatchers.IO] 上跑阻塞的 [block]；协程被取消时调 [onCancel]（一般是关掉 socket）让它返回，取消后的
 * 结果丢掉。
 */
internal suspend fun <T> blocking(onCancel: () -> Unit, block: () -> T): T =
    suspendCancellableCoroutine { continuation ->
        continuation.invokeOnCancellation { onCancel() }
        Dispatchers.IO.asExecutor().execute {
            continuation.resumeWith(runCatching(block))
        }
    }
