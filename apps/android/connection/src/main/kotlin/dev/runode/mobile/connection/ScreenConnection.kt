package dev.runode.mobile.connection

import dev.runode.mobile.protocol.Frame
import dev.runode.mobile.protocol.FrameDecoder
import dev.runode.mobile.protocol.FrameKind
import dev.runode.mobile.protocol.ScreenClientMsg
import dev.runode.mobile.protocol.ScreenDisplay
import dev.runode.mobile.protocol.ScreenHostMsg
import dev.runode.mobile.protocol.VideoFrame
import dev.runode.mobile.protocol.screenVersion
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow

/** 远程桌面的连接上收到的东西。 */
sealed class ScreenEvent {
    data class Message(val message: ScreenHostMsg) : ScreenEvent()

    data class Video(val frame: VideoFrame) : ScreenEvent()
}

/** 一台电脑的远程桌面：找地址、TLS、门禁、`ScreenHello`，等到电脑的 `Ready`。断了不自动重连，由页面重新打开。 */
class ScreenConnection private constructor(
    private val transport: FrameTransport,
    /** `Ready` 之后已经收到、还没切成帧的字节。 */
    private val decoder: FrameDecoder,
    ready: ScreenHostMsg.Ready,
) {
    /** 电脑握手后报的各块屏幕，主屏排第一，至少一块。 */
    val displays: List<ScreenDisplay> = ready.displays

    /** 电脑有没有屏幕录制的授权；没有时 `Start` 只换来 `Stopped`。 */
    val screenCapture: Boolean = ready.screenCapture

    /** 电脑有没有注入键鼠的授权；没有时输入被系统悄悄丢掉。 */
    val postEvent: Boolean = ready.postEvent

    private val collected = AtomicBoolean(false)

    /**
     * 电脑发来的消息和画面，到连接断开为止，断开时以错误结束，结束时连接已经关了；只能收集一次。收集方处理完一个
     * 才读下一个：页面处理不过来时不再往下读，由 TCP 压住电脑，不在手机上越积越多。
     */
    fun events(): Flow<ScreenEvent> = flow {
        check(collected.compareAndSet(false, true)) { "screen events can only be collected once" }
        val reader = FrameReader(transport, decoder)
        try {
            while (true) emit(event(reader.next()))
        } finally {
            transport.close()
        }
    }

    fun send(message: ScreenClientMsg) {
        val frame = try {
            Frame.control(message).encoded()
        } catch (_: Exception) {
            return
        }
        transport.send(frame)
    }

    fun close() {
        transport.close()
    }

    companion object {
        /** 连上 [machine] 的远程桌面。 */
        suspend fun open(
            machine: MachineRecord,
            keyStore: DeviceKeyStore,
            discovery: HostDiscovery,
            open: TransportOpener = { target, fingerprint -> TlsChannel.open(target, fingerprint) },
            tunnel: TunnelOpener? = { node, fingerprint -> IrohDialer.shared.open(node, fingerprint) },
        ): ScreenConnection = dial(
            machine,
            keyStore,
            discovery,
            open,
            tunnel,
            handshake = { transport, key -> handshake(transport, machine.deviceId, key) },
            close = { it.close() },
        )

        private suspend fun handshake(transport: FrameTransport, deviceId: String, key: DeviceKey): ScreenConnection {
            val outcome = GateClient.run(transport, GateCredential.Auth(deviceId, key))
            transport.send(Frame.control(ScreenClientMsg.ScreenHello(screenVersion)).encoded())
            val reader = FrameReader(transport, outcome.decoder)
            val ready = withLinkTimeout(GateClient.timeout, onTimeout = transport::close) {
                when (val message = reader.nextControl().message(ScreenHostMsg.serializer())) {
                    is ScreenHostMsg.Ready -> message.takeIf { it.displays.isNotEmpty() }
                        ?: throw LinkFailure.ProtocolViolation("the computer reported no displays")
                    is ScreenHostMsg.Incompatible -> throw LinkFailure.Incompatible("remote desktop version ${message.version}")
                    else -> throw LinkFailure.ProtocolViolation("the computer did not report its displays first")
                }
            }
            return ScreenConnection(transport, reader.decoder, ready)
        }

        private fun event(frame: Frame): ScreenEvent = when (frame.kind) {
            FrameKind.Control -> ScreenEvent.Message(
                try {
                    frame.message(ScreenHostMsg.serializer())
                } catch (_: Exception) {
                    ScreenHostMsg.Unknown
                },
            )
            FrameKind.Video -> ScreenEvent.Video(
                VideoFrame(frame.payload) ?: throw LinkFailure.ProtocolViolation("malformed video frame"),
            )
            FrameKind.Output, FrameKind.Input, FrameKind.Snapshot, FrameKind.Audio ->
                throw LinkFailure.ProtocolViolation("got a ${frame.kind} frame on the remote desktop")
        }
    }
}
