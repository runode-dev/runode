package dev.runode.mobile.connection

import android.content.Context
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import dev.runode.mobile.protocol.CertificateFingerprint
import dev.runode.mobile.protocol.tunnelALPN
import java.io.IOException
import java.net.InetAddress
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread
import kotlin.coroutines.cancellation.CancellationException
import kotlin.coroutines.resumeWithException
import kotlin.time.Duration
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine

/**
 * iroh 原生库的几个调用，编号是原生库给的端点和流，关掉后再拿来调只会抛 [IOException]。都是阻塞调用，可以从任意线程同时调。
 * 真的是 [NativeIroh]；JVM 单测换成假的，不加载原生库。
 */
interface IrohNative {
    /**
     * 用私钥 [secret]（32 字节）建一个端点，返回它的编号。[secret] 长度不对时抛 [IllegalArgumentException]。
     */
    fun bind(secret: ByteArray): Long

    /**
     * 经 [endpoint] 拨公钥是 [node]（十六进制）的电脑，连上后开一条双向流，返回流的编号。[node] 不是公钥时抛
     * [IllegalArgumentException]，[timeoutMillis] 内没连上和别的失败抛 [IOException]。
     */
    fun connect(endpoint: Long, node: String, alpn: ByteArray, timeoutMillis: Long): Long

    /** 读进 [buffer]，返回读到的字节数；对面说完了返回 -1。 */
    fun read(stream: Long, buffer: ByteArray, offset: Int, length: Int): Int

    /** 写完才返回。 */
    fun write(stream: Long, buffer: ByteArray, offset: Int, length: Int)

    /** 关掉流和它所在的连接，阻塞着的 [read]、[write] 随之抛错。 */
    fun close(stream: Long)

    fun closeEndpoint(endpoint: Long)

    /** 本机网络可能变了：所有端点重新探测地址、换掉绑在旧网络上的路径。 */
    fun networkChanged()
}

/** librunode_tunnel.so，第一次调到时加载。 */
internal object IrohLibrary {
    init {
        System.loadLibrary("runode_tunnel")
    }

    @JvmStatic external fun bind(context: Any?, secret: ByteArray): Long

    @JvmStatic external fun connect(endpoint: Long, node: String, alpn: ByteArray, timeoutMillis: Long): Long

    @JvmStatic external fun read(stream: Long, buffer: ByteArray, offset: Int, length: Int): Int

    @JvmStatic external fun write(stream: Long, buffer: ByteArray, offset: Int, length: Int)

    @JvmStatic external fun close(stream: Long)

    @JvmStatic external fun closeEndpoint(endpoint: Long)

    @JvmStatic external fun networkChanged()
}

/** 真的原生库。 */
object NativeIroh : IrohNative {
    /** iroh 经它读系统的 DNS 设置。在第一次拨号前设好；没设时 iroh 用自带的公共 DNS。 */
    @Volatile
    var context: Context? = null

    override fun bind(secret: ByteArray): Long = IrohLibrary.bind(context?.applicationContext, secret)

    override fun connect(endpoint: Long, node: String, alpn: ByteArray, timeoutMillis: Long): Long =
        IrohLibrary.connect(endpoint, node, alpn, timeoutMillis)

    override fun read(stream: Long, buffer: ByteArray, offset: Int, length: Int): Int =
        IrohLibrary.read(stream, buffer, offset, length)

    override fun write(stream: Long, buffer: ByteArray, offset: Int, length: Int) =
        IrohLibrary.write(stream, buffer, offset, length)

    override fun close(stream: Long) = IrohLibrary.close(stream)

    override fun closeEndpoint(endpoint: Long) = IrohLibrary.closeEndpoint(endpoint)

    override fun networkChanged() = IrohLibrary.networkChanged()
}

/**
 * 跨网络连电脑：iroh 按电脑的公钥拨号，先打洞直连，打不通走中继。连接上开一条双向流，流上跑的和局域网的 TCP 一样是 TLS
 * 加门禁。[TlsChannel] 只能开在 socket 上，所以在本机回环地址上起一个只接一条连接的代理（[LoopbackProxy]）接到流上，
 * [open] 连它。每把私钥一个 iroh 端点，第一次用它拨号时建，同一把私钥同时拨号时共用同一次建；最多留 [tunnelEndpointLimit]
 * 个（[TunnelEndpointPool]），一条连接从拨号到关掉都借着它的端点。整个 app 用 [shared]。
 */
class IrohDialer(
    private val native: IrohNative,
    private val open: TransportOpener = { target, fingerprint -> TlsChannel.open(target, fingerprint) },
) {
    private val lock = Any()

    /** 正在建或者建好了的端点；建失败的从这里忘掉，下次拨号重建。 */
    private val pool = TunnelEndpointPool<TunnelSecret, Deferred<Long>>(tunnelEndpointLimit)

    /** 用私钥 [secret] 的端点拨公钥是 [node] 的电脑。 */
    suspend fun open(node: String, fingerprint: CertificateFingerprint, secret: ByteArray): FrameTransport {
        val (endpoint, lease) = bound(TunnelSecret(secret))
        val alpn = tunnelALPN.encodeToByteArray()
        val stream = try {
            abandoningOnCancel(
                work = { native.connect(endpoint, node, alpn, connectTimeout.inWholeMilliseconds) },
                discard = native::close,
            )
        } catch (error: Throwable) {
            giveBack(lease)
            throw when (error) {
                is IllegalArgumentException -> LinkFailure.ProtocolViolation("the computer reported a malformed iroh address")
                is IOException -> LinkFailure.ConnectionFailed(error.message ?: error.toString())
                else -> error
            }
        }
        val proxy = try {
            LoopbackProxy.start(native, stream, onClose = { giveBack(lease) })
        } catch (error: IOException) {
            native.close(stream)
            giveBack(lease)
            throw LinkFailure.ConnectionFailed(error.message ?: error.toString())
        }
        return try {
            TunnelledTransport(open(proxy.target, fingerprint), proxy)
        } catch (error: Throwable) {
            proxy.close()
            throw error
        }
    }

    /** 关掉私钥 [secret] 的端点，经它开着的连接一起断掉。没建过时什么也不做。 */
    fun forget(secret: ByteArray) {
        val removed = synchronized(lock) { pool.remove(TunnelSecret(secret)) }
        closeLater(listOfNotNull(removed))
    }

    /** 本机网络可能变了，告诉建着的端点；一个也没有时不碰原生库。 */
    fun networkChanged() {
        if (synchronized(lock) { pool.count } == 0) return
        detached.launch { runCatching { native.networkChanged() } }
    }

    /**
     * 跟着 [context] 里系统的默认网络，换了网络、链路属性变了时调 [networkChanged]。整个进程调一次，一直跟着。
     */
    fun followNetwork(context: Context) {
        val connectivity = context.getSystemService(ConnectivityManager::class.java) ?: return
        connectivity.registerDefaultNetworkCallback(
            object : ConnectivityManager.NetworkCallback() {
                override fun onAvailable(network: Network) = networkChanged()

                override fun onLost(network: Network) = networkChanged()

                override fun onLinkPropertiesChanged(network: Network, linkProperties: LinkProperties) = networkChanged()
            },
        )
    }

    /** [secret] 的端点和借它的凭据，用完要 [giveBack]；取消或者失败时已经还了。 */
    private suspend fun bound(secret: TunnelSecret): Pair<Long, TunnelEndpointPool.Lease<TunnelSecret>> {
        val (binding, lease) = synchronized(lock) {
            pool.borrow(secret) ?: run {
                val binding = detached.async { native.bind(secret.bytes) }
                val (lease, evicted) = pool.insert(binding, secret)
                closeLater(evicted)
                binding to lease
            }
        }
        return try {
            binding.await() to lease
        } catch (cancelled: CancellationException) {
            giveBack(lease)
            throw cancelled
        } catch (error: Exception) {
            throw bindFailed(lease, error)
        } catch (error: LinkageError) {
            // 这个 ABI 没打进原生库时加载不了，当成连不上，不让它冲出主循环。
            throw bindFailed(lease, error)
        }
    }

    /** 建端点失败：忘掉这次，下次拨号重建。 */
    private fun bindFailed(lease: TunnelEndpointPool.Lease<TunnelSecret>, error: Throwable): LinkFailure {
        synchronized(lock) { pool.remove(lease) }
        return LinkFailure.ConnectionFailed(error.message ?: error.toString())
    }

    private fun giveBack(lease: TunnelEndpointPool.Lease<TunnelSecret>) {
        closeLater(synchronized(lock) { pool.giveBack(lease) })
    }

    /** 在后台关掉 [endpoints]：关端点要等它把连接关好，不占调用方的线程。建失败的不用关。 */
    private fun closeLater(endpoints: List<Deferred<Long>>) {
        if (endpoints.isEmpty()) return
        detached.launch {
            for (endpoint in endpoints) runCatching { native.closeEndpoint(endpoint.await()) }
        }
    }

    /** 池里按内容比较的私钥；拷一份，调用方之后改了数组也不影响。 */
    private class TunnelSecret(bytes: ByteArray) {
        val bytes: ByteArray = bytes.copyOf()

        override fun equals(other: Any?): Boolean = other is TunnelSecret && bytes.contentEquals(other.bytes)

        override fun hashCode(): Int = bytes.contentHashCode()
    }

    companion object {
        /** 整个 app 共用的那个，用真的原生库。 */
        val shared: IrohDialer by lazy { IrohDialer(NativeIroh) }

        /** 打洞或者经中继连上、开出流的时限。 */
        private val connectTimeout: Duration = 15.seconds
    }
}

/** 同时留着的 iroh 端点最多几个。 */
internal const val tunnelEndpointLimit = 8

/** 拨号、建端点这类调用方取消了也要跑完的活；不随哪个调用方取消。 */
private val detached = CoroutineScope(SupervisorJob() + Dispatchers.IO)

/**
 * 在 [Dispatchers.IO] 上跑不理会取消的 [work]（阻塞在原生库里的拨号，取消了也要等它自己成功或超时）：取消时马上抛
 * [kotlin.coroutines.cancellation.CancellationException]，不等它；它晚到的结果交给 [discard] 收掉。
 */
internal suspend fun <T> abandoningOnCancel(work: suspend () -> T, discard: (T) -> Unit): T {
    currentCoroutineContext().ensureActive()
    return suspendCancellableCoroutine { continuation ->
        detached.launch {
            runCatching { work() }.fold(
                onSuccess = { value -> continuation.resume(value) { _, late, _ -> discard(late) } },
                onFailure = { continuation.resumeWithException(it) },
            )
        }
    }
}

/** 经 iroh 的 TLS 连接。 */
internal class TunnelledTransport(private val tls: FrameTransport, private val proxy: LoopbackProxy) : FrameTransport {
    override suspend fun receive(): ByteArray? = tls.receive()

    override fun send(data: ByteArray) = tls.send(data)

    override suspend fun drain() = tls.drain()

    override fun exporter(): ByteArray = tls.exporter()

    /** 不报：[tls] 连的是本机代理，不是电脑，不能记成下次先试的地址。 */
    override val remoteAddress: String? get() = null

    override fun close() {
        tls.close()
        proxy.close()
    }
}

/**
 * 本机回环地址上只接一条连接的 TCP 代理，两个方向搬到 iroh 的流 [stream] 上，各占一条线程阻塞着搬。流归它管：哪个方向
 * 出错都关掉整个代理和流，另一个方向跟着停；iroh 那边说完时关掉本机连接写的一半。
 *
 * shortcut: 认的是第一条连上来的，不核对是不是自己的 [TlsChannel]，同一台手机上别的 app 抢先连上时这次拨号失败（拿不到
 * 电脑的东西：流上还要过 TLS 和门禁）；要防时核对对端端口是 TLS 那条 socket 的本地端口。
 */
internal class LoopbackProxy private constructor(
    private val native: IrohNative,
    private val stream: Long,
    private val server: ServerSocket,
    private val onClose: () -> Unit,
) {
    val target: TransportTarget = TransportTarget(server.inetAddress.hostAddress!!, server.localPort.toUShort(), "iroh")

    @Volatile
    private var accepted: Socket? = null
    private val closed = AtomicBoolean(false)

    private fun serve() {
        val socket = try {
            server.accept()
        } catch (_: IOException) {
            return close()
        } finally {
            closeQuietly(server)
        }
        accepted = socket
        if (closed.get()) return closeQuietly(socket)
        thread(name = "runode-iroh-down", isDaemon = true) { down(socket) }
        up(socket)
    }

    /** 本机连接到 iroh。本机那边说完时不再搬，留着流给另一个方向。 */
    private fun up(socket: Socket) {
        val buffer = ByteArray(chunk)
        try {
            val input = socket.getInputStream()
            while (true) {
                val count = input.read(buffer)
                if (count < 0) return
                native.write(stream, buffer, 0, count)
            }
        } catch (_: Exception) {
            close()
        }
    }

    /** iroh 到本机连接。 */
    private fun down(socket: Socket) {
        val buffer = ByteArray(chunk)
        try {
            val output = socket.getOutputStream()
            while (true) {
                val count = native.read(stream, buffer, 0, buffer.size)
                if (count < 0) break
                output.write(buffer, 0, count)
            }
            socket.shutdownOutput()
        } catch (_: Exception) {
            close()
        }
    }

    fun close() {
        if (!closed.compareAndSet(false, true)) return
        closeQuietly(server)
        accepted?.let(::closeQuietly)
        native.close(stream)
        onClose()
    }

    companion object {
        private const val chunk = 64 shl 10

        /**
         * 开始监听、在后台等那一条连接；[stream] 从此归它管，监听开不出来时抛 [IOException]，流仍归调用方。代理关掉时调
         * 一次 [onClose]。
         */
        fun start(native: IrohNative, stream: Long, onClose: () -> Unit = {}): LoopbackProxy {
            val server = ServerSocket(0, 1, InetAddress.getLoopbackAddress())
            val proxy = LoopbackProxy(native, stream, server, onClose)
            thread(name = "runode-iroh-up", isDaemon = true) { proxy.serve() }
            return proxy
        }

        private fun closeQuietly(closeable: java.io.Closeable) {
            try {
                closeable.close()
            } catch (_: IOException) {
            }
        }
    }
}
