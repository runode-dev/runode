package dev.runode.mobile.connection

import dev.runode.mobile.protocol.Frame
import dev.runode.mobile.protocol.FrameKind
import dev.runode.mobile.protocol.MediaClientMsg
import dev.runode.mobile.protocol.MediaHostMsg
import dev.runode.mobile.protocol.maxMediaBytes
import dev.runode.mobile.protocol.mediaVersion
import java.io.ByteArrayInputStream
import java.io.EOFException
import java.io.File
import java.io.FileInputStream
import java.io.InputStream
import kotlin.time.Duration
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** 发文件的进度：已经交给连接 [sent] 字节，一共 [total] 字节。在调用 [MediaConnection.send] 的协程里报。 */
typealias MediaProgress = (sent: Long, total: Long) -> Unit

/**
 * 交给一台电脑图片和视频的连接：找地址、TLS、门禁、`media_hello`，等到电脑的 `ready`。一个一个 [send]，拿回文件在电脑上的
 * 路径；不要了就 [close]。同一时刻只发一个。
 */
class MediaConnection private constructor(
    private val transport: FrameTransport,
    /** 读电脑回话用的；`ready` 之后已经收到、还没切成帧的字节在它里面。 */
    private val reader: FrameReader,
) {
    /** 发一个编好的文件的字节，等电脑存好，返回它在电脑上的绝对路径。电脑存不了时抛 [MediaFailure]。 */
    suspend fun send(data: ByteArray, progress: MediaProgress? = null): String =
        send(ByteArrayInputStream(data), data.size.toLong(), progress)

    /** 边读边发 [file]，不整个读进内存，等电脑存好，返回它在电脑上的绝对路径。电脑存不了时抛 [MediaFailure]。 */
    suspend fun send(file: File, progress: MediaProgress? = null): String {
        val (input, length) = withContext(Dispatchers.IO) { FileInputStream(file) to file.length() }
        return input.use { send(it, length, progress) }
    }

    /**
     * 从 [input] 读正好 [length] 字节发出去，读一块发一块，排着没写出去的不超过 [drainEvery] 块；[input] 由调用方关。[length]
     * 在 1 到 `maxMediaBytes` 之间：电脑对空文件回 failed 后断开。没读够 [length] 字节就读完时抛 [EOFException]。
     */
    suspend fun send(input: InputStream, length: Long, progress: MediaProgress? = null): String {
        require(length in 1..maxMediaBytes.toLong()) { "the file has $length bytes, not what the computer takes" }
        try {
            transport.send(Frame.control(MediaClientMsg.Media(length.toUInt())).encoded())
            val buffer = ByteArray(chunkBytes)
            var sent = 0L
            var chunks = 0
            while (sent < length) {
                val count = minOf(chunkBytes.toLong(), length - sent).toInt()
                withContext(Dispatchers.IO) { readFully(input, buffer, count) }
                // `encoded` 把载荷拷进新的数组，缓冲区下一块接着用。
                transport.send(Frame(FrameKind.Media, 0u, if (count == buffer.size) buffer else buffer.copyOf(count)).encoded())
                sent += count
                chunks += 1
                if (chunks % drainEvery == 0) {
                    withLinkTimeout(saveTimeout, onTimeout = transport::close) { transport.drain() }
                }
                progress?.invoke(sent, length)
            }
            return withLinkTimeout(saveTimeout, onTimeout = transport::close) { readSaved() }
        } catch (failure: MediaFailure) {
            throw failure
        } catch (error: Throwable) {
            // 发到一半或等回话时出的错：连接上的帧已经对不齐了。
            transport.close()
            throw error
        }
    }

    private suspend fun readSaved(): String {
        while (true) {
            when (val message = reader.nextControl().message(MediaHostMsg.serializer())) {
                is MediaHostMsg.Saved -> return message.path
                is MediaHostMsg.Failed -> throw MediaFailure(message.reason)
                else -> {}
            }
        }
    }

    fun close() {
        transport.close()
    }

    companion object {
        /** 一帧 [FrameKind.Media] 最多带这么多字节，不超过 `maxMediaFrame`。 */
        internal const val chunkBytes: Int = 256 shl 10

        /** 每发这么多块等一次之前的都写出去（[FrameTransport.drain]）。 */
        internal const val drainEvery: Int = 4

        /**
         * 等之前的块写出去、发完最后一块后等电脑存好，各最多这么久：电脑合盖睡着时连接不断、socket 写也没有超时，不设上限就一直
         * 等下去。
         */
        internal val saveTimeout: Duration = 120.seconds

        /** 连上 [machine] 收图片和视频的地方。 */
        suspend fun open(
            machine: MachineRecord,
            keyStore: DeviceKeyStore,
            discovery: HostDiscovery,
            open: TransportOpener = { target, fingerprint -> TlsChannel.open(target, fingerprint) },
            tunnel: TunnelOpener? = { node, fingerprint -> IrohDialer.shared.open(node, fingerprint) },
        ): MediaConnection = dial(
            machine,
            keyStore,
            discovery,
            open,
            tunnel,
            handshake = { transport, key -> handshake(transport, machine.deviceId, key) },
            close = { it.close() },
        )

        private suspend fun handshake(transport: FrameTransport, deviceId: String, key: DeviceKey): MediaConnection {
            val outcome = GateClient.run(transport, GateCredential.Auth(deviceId, key))
            transport.send(Frame.control(MediaClientMsg.MediaHello(mediaVersion)).encoded())
            val reader = FrameReader(transport, outcome.decoder)
            withLinkTimeout(GateClient.timeout, onTimeout = transport::close) {
                when (val message = reader.nextControl().message(MediaHostMsg.serializer())) {
                    MediaHostMsg.Ready -> {}
                    is MediaHostMsg.Incompatible -> throw LinkFailure.Incompatible("media version ${message.version}")
                    else -> throw LinkFailure.ProtocolViolation("the computer did not say it was ready for media first")
                }
            }
            return MediaConnection(transport, reader)
        }

        private fun readFully(input: InputStream, into: ByteArray, count: Int) {
            var filled = 0
            while (filled < count) {
                val read = input.read(into, filled, count - filled)
                if (read < 0) throw EOFException("the file ended ${count - filled} bytes early")
                filled += read
            }
        }
    }
}

/** 电脑回了存不了这个文件；[reason] 是电脑给的英文原因。 */
data class MediaFailure(val reason: String) : Exception("the computer could not save the file: $reason")
