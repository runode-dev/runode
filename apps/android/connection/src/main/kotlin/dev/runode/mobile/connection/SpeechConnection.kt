package dev.runode.mobile.connection

import dev.runode.mobile.protocol.Frame
import dev.runode.mobile.protocol.FrameDecoder
import dev.runode.mobile.protocol.FrameKind
import dev.runode.mobile.protocol.SpeechClientMsg
import dev.runode.mobile.protocol.SpeechHostMsg
import dev.runode.mobile.protocol.speechAudioPayload
import dev.runode.mobile.protocol.speechVersion
import kotlin.time.Duration
import kotlin.time.Duration.Companion.seconds

/**
 * 交给一台电脑识别的一段录音：找地址、TLS、门禁、`speech_hello`，等到电脑的 `ready`。边录边 [send]，录完 [finish]
 * 拿文字；一条连接识别一段话，不要了就 [close]。
 */
class SpeechConnection private constructor(
    private val transport: FrameTransport,
    /** `ready` 之后已经收到、还没切成帧的字节。 */
    private val decoder: FrameDecoder,
    /** 电脑上下好了它设置的识别模型；没有时识别只会失败。 */
    val installed: Boolean,
) {
    /** 发一段录音（`speechSampleRate` 的单声道，-1 到 1）。 */
    fun send(samples: FloatArray) {
        val frame = try {
            Frame(FrameKind.Audio, 0u, speechAudioPayload(samples)).encoded()
        } catch (_: Exception) {
            return
        }
        transport.send(frame)
    }

    /** 录完了：等电脑识别完，返回文字；没听出话来时是空的。之后连接关掉。 */
    suspend fun finish(): String {
        try {
            transport.send(Frame.control(SpeechClientMsg.Finish).encoded())
            val reader = FrameReader(transport, decoder)
            return withLinkTimeout(finishTimeout, onTimeout = transport::close) { readText(reader) }
        } finally {
            transport.close()
        }
    }

    private suspend fun readText(reader: FrameReader): String {
        while (true) {
            when (val message = reader.nextControl().message(SpeechHostMsg.serializer())) {
                is SpeechHostMsg.Text -> return message.text
                is SpeechHostMsg.Failed -> throw SpeechFailure(message.reason)
                else -> {}
            }
        }
    }

    fun close() {
        transport.close()
    }

    companion object {
        /**
         * 等电脑识别完最多这么久：电脑可能先在本机识别、出错再交给云端，最后还要润色，每步都要等网络；电脑合盖睡着时
         * 连接不断，不设上限就一直等下去。
         */
        internal val finishTimeout: Duration = 180.seconds

        /** 连上 [machine] 的识别。 */
        suspend fun open(
            machine: MachineRecord,
            keyStore: DeviceKeyStore,
            discovery: HostDiscovery,
            open: TransportOpener = { target, fingerprint -> TlsChannel.open(target, fingerprint) },
            tunnel: TunnelOpener? = { node, fingerprint -> IrohDialer.shared.open(node, fingerprint) },
        ): SpeechConnection = dial(
            machine,
            keyStore,
            discovery,
            open,
            tunnel,
            handshake = { transport, key -> handshake(transport, machine.deviceId, key) },
            close = { it.close() },
        )

        private suspend fun handshake(transport: FrameTransport, deviceId: String, key: DeviceKey): SpeechConnection {
            val outcome = GateClient.run(transport, GateCredential.Auth(deviceId, key))
            transport.send(Frame.control(SpeechClientMsg.SpeechHello(speechVersion)).encoded())
            val reader = FrameReader(transport, outcome.decoder)
            val installed = withLinkTimeout(GateClient.timeout, onTimeout = transport::close) {
                when (val message = reader.nextControl().message(SpeechHostMsg.serializer())) {
                    is SpeechHostMsg.Ready -> message.installed
                    is SpeechHostMsg.Incompatible -> throw LinkFailure.Incompatible("speech version ${message.version}")
                    else -> throw LinkFailure.ProtocolViolation("the computer did not report its speech status first")
                }
            }
            return SpeechConnection(transport, reader.decoder, installed)
        }
    }
}

/** 电脑回了识别失败；[reason] 是电脑给的英文原因。 */
data class SpeechFailure(val reason: String) : Exception("speech recognition failed: $reason")
