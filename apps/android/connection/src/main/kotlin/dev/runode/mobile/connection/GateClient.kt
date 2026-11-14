package dev.runode.mobile.connection

import dev.runode.mobile.protocol.Frame
import dev.runode.mobile.protocol.FrameDecoder
import dev.runode.mobile.protocol.FrameKind
import dev.runode.mobile.protocol.GateMessage
import dev.runode.mobile.protocol.GatePurpose
import dev.runode.mobile.protocol.GateSignature
import dev.runode.mobile.protocol.gateVersion
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.coroutines.cancellation.CancellationException
import kotlin.time.Duration
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** 门禁时出示的凭据。 */
sealed class GateCredential {
    /** 已配对过：宿主发的 `device_id` 加这台电脑的设备私钥。 */
    class Auth(val deviceId: String, val key: DeviceKey) : GateCredential()

    /** 第一次配对：二维码里的口令、报给电脑的设备名、新生成的设备私钥。 */
    class Pair(val secret: ByteArray, val deviceName: String, val key: DeviceKey) : GateCredential()
}

/** 门禁的结果。 */
class GateOutcome(
    /** 宿主给的设备编号。 */
    val deviceId: String,
    /** `remote_challenge` 里报的主机名。 */
    val hostName: String,
    /** `remote_challenge` 里报的跨网络连接的地址，电脑没开时为空。 */
    val node: String?,
    /** 门禁之后剩下、还没切成帧的字节；`Hello` 接着它读。 */
    val decoder: FrameDecoder,
)

/** 一边读帧一边攒着没切完的字节。 */
internal class FrameReader(val transport: FrameTransport, val decoder: FrameDecoder) {
    /** 下一帧；对面关了连接时报 [LinkFailure.HostClosed]。 */
    suspend fun next(): Frame {
        while (true) {
            decoder.next()?.let { return it }
            val chunk = transport.receive()
            if (chunk == null) {
                decoder.finish()
                throw LinkFailure.HostClosed
            }
            decoder.append(chunk)
        }
    }

    /** 下一条控制帧；门禁和握手阶段宿主不发别的帧。 */
    suspend fun nextControl(): Frame {
        val frame = next()
        if (frame.kind != FrameKind.Control || frame.channel != 0u) {
            throw LinkFailure.ProtocolViolation("got a ${frame.kind} frame before logging in")
        }
        return frame
    }
}

/**
 * 走一遍门禁：等宿主的 `remote_challenge`，签好名回 `remote_auth` 或 `remote_pair`，等宿主放行。门禁阶段限时
 * （[timeout]）走完，单帧载荷不超过 [Frame.gateMaxPayload]；超时或被取消时关掉连接。
 */
object GateClient {
    /** 宿主的 `remote::GATE_TIMEOUT`。 */
    val timeout: Duration = 10.seconds

    suspend fun run(transport: FrameTransport, credential: GateCredential, timeout: Duration = GateClient.timeout): GateOutcome =
        try {
            withLinkTimeout(timeout, onTimeout = transport::close) { exchange(transport, credential) }
        } catch (cancelled: CancellationException) {
            // 关掉连接才停得下来，免得配对页关了还把 `remote_pair` 发出去。
            transport.close()
            throw cancelled
        }

    private suspend fun exchange(transport: FrameTransport, credential: GateCredential): GateOutcome {
        val reader = FrameReader(transport, FrameDecoder(maxPayload = Frame.gateMaxPayload))
        val challenge = reader.nextControl().message(GateMessage.serializer()) as? GateMessage.Challenge
            ?: throw LinkFailure.ProtocolViolation("the first message is not remote_challenge")
        if (challenge.version != gateVersion) throw LinkFailure.GateVersion(challenge.version)
        if (challenge.nonce.size != GateSignature.nonceLength) throw LinkFailure.ProtocolViolation("the nonce has the wrong length")
        val exporter = transport.exporter()
        // Keystore 签名可能要走一趟安全芯片，不占着调用方的线程。
        val reply = withContext(Dispatchers.IO) {
            when (credential) {
                is GateCredential.Auth -> GateMessage.Auth(
                    deviceId = credential.deviceId,
                    signature = credential.key.sign(GateSignature.payload(challenge.nonce, exporter, GatePurpose.Auth)),
                )
                is GateCredential.Pair -> GateMessage.Pair(
                    secret = credential.secret,
                    deviceName = credential.deviceName,
                    publicKey = credential.key.publicKeyX963,
                    signature = credential.key.sign(GateSignature.payload(challenge.nonce, exporter, GatePurpose.Pair)),
                )
            }
        }
        currentCoroutineContext().ensureActive()
        transport.send(Frame.control(reply).encoded())
        return when (val verdict = reader.nextControl().message(GateMessage.serializer())) {
            is GateMessage.Accepted -> {
                reader.decoder.maxPayload = Frame.maxPayload
                GateOutcome(verdict.deviceId, challenge.hostName, challenge.node, reader.decoder)
            }
            is GateMessage.Rejected -> throw LinkFailure.Rejected(verdict.reason)
            else -> throw LinkFailure.ProtocolViolation("unexpected gate reply")
        }
    }
}

/**
 * 在 [duration] 内做完 [block]，否则调 [onTimeout]（一般是关掉连接，让卡着的读返回）、取消 [block]，报
 * [LinkFailure.Timeout]：超时后卡着的读报的是连接断开，这里统一报超时。
 */
internal suspend fun <T> withLinkTimeout(duration: Duration, onTimeout: () -> Unit, block: suspend () -> T): T {
    val timedOut = AtomicBoolean(false)
    try {
        return coroutineScope {
            val work = async { block() }
            val watchdog = launch {
                delay(duration)
                timedOut.set(true)
                onTimeout()
                work.cancel()
            }
            try {
                work.await()
            } finally {
                watchdog.cancel()
            }
        }
    } catch (error: Throwable) {
        if (timedOut.get()) throw LinkFailure.Timeout
        throw error
    }
}
