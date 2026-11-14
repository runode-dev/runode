package dev.runode.mobile.connection

import dev.runode.mobile.protocol.Caps
import dev.runode.mobile.protocol.ClientKind
import dev.runode.mobile.protocol.ClientMsg
import dev.runode.mobile.protocol.GateRejection
import dev.runode.mobile.protocol.GoodbyeReason
import dev.runode.mobile.protocol.HostMsg
import dev.runode.mobile.protocol.protocolVersion
import java.time.Instant
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.StateFlow

/** 连接推给上层的事件，按发生的先后到达。 */
sealed class HostEvent {
    /** 连接状态变了。订阅时先收到一次当前的状态。 */
    data class State(val state: LinkState) : HostEvent()

    /**
     * 门禁和 `Hello` 都过了，可以发消息了。每连上一次 [generation] 加一：断线重连以后通道编号全部作废，
     * 各个页面要重新 `Attach`。
     */
    data class Ready(val generation: ULong) : HostEvent()

    /** 宿主发来的控制消息。 */
    data class Message(val message: HostMsg) : HostEvent()

    /** 宿主发来的输出或快照帧，带着收到它的那次连接的 [generation]。 */
    data class Frame(val frame: dev.runode.mobile.protocol.Frame, val generation: ULong) : HostEvent()
}

/** 连接的状态。 */
sealed class LinkState {
    data object Idle : LinkState()

    data object Connecting : LinkState()

    data class Connected(val hostName: String, val address: String?) : LinkState()

    /**
     * 断了或者连不上，[retryAt] 时自动再试。[offline] 是电脑离线（连不上、没开远程访问）：不一遍遍试，
     * 它在局域网里重新公布服务时马上连，[retryAt] 只是兜底。
     */
    data class Waiting(val reason: LinkFailure, val retryAt: Instant, val offline: Boolean = false) : LinkState()

    /** 不会自动重试的失败（被拒绝、版本不兼容、私钥没了等），要用户处理后手动重试。 */
    data class Failed(val failure: LinkFailure) : LinkState()

    val isConnected: Boolean get() = this is Connected
}

/**
 * 连接或配对失败的原因。[message] 是给日志看的英文，给人看的说法由界面按种类给出。不带调用栈：这些是
 * 意料之中的失败，几个无参的种类是单例，也不能让协程往上面挂被压制的异常。
 */
sealed class LinkFailure(message: String) : Exception(message, null, false, false) {
    data class Rejected(val reason: GateRejection) : LinkFailure("the computer rejected the connection: ${reason.wireName}")

    /** 电脑上的门禁版本这个 app 不认识。 */
    data class GateVersion(val version: UInt) : LinkFailure("unsupported gate version $version")

    /** 宿主的协议版本、远程桌面或语音识别的版本和这个 app 不一样；[reason] 是英文的说明。 */
    data class Incompatible(val reason: String) : LinkFailure("incompatible: $reason")

    /** 这台电脑的设备私钥不在 [DeviceKeyStore] 里了。 */
    data object MissingKey : LinkFailure("the device key for this computer is missing")

    /** 电脑出示的证书和配对时记下的指纹不一样。 */
    data object FingerprintMismatch : LinkFailure("the certificate fingerprint does not match")

    data object Timeout : LinkFailure("timed out")

    /** 一个能试的地址都没有。 */
    data object NoAddress : LinkFailure("no address to try")

    /** 二维码过期了。 */
    data object InvitationExpired : LinkFailure("the pairing invitation has expired")

    data class ProtocolViolation(val detail: String) : LinkFailure("protocol violation: $detail")

    data class ConnectionFailed(val detail: String) : LinkFailure("connection failed: $detail")

    /** 连接出错断开，[detail] 是系统给的说明。 */
    data class Closed(val detail: String) : LinkFailure("connection closed: $detail")

    /** 电脑没说原因就关了连接。 */
    data object HostClosed : LinkFailure("the computer closed the connection")

    /** 电脑说了 `Goodbye` 再断开：升级交接、退出或出错。 */
    data class Goodbye(val reason: GoodbyeReason) : LinkFailure("the computer said goodbye: $reason")

    /** 读写 Android Keystore 出错。 */
    data class KeyStore(val detail: String) : LinkFailure("keystore: $detail")

    /** 自动重试也没用的失败。 */
    val isFatal: Boolean
        get() = when (this) {
            // 设备被撤销、签名不对、口令作废：再试也一样，签名不对和口令作废电脑还会计入限速。远程访问正在关掉
            // （`Disabled`）不算：电脑升级交接、重开远程访问时监听也会停一下，当电脑离线处理（[isOffline]）。
            is Rejected -> when (reason) {
                GateRejection.UnknownDevice, GateRejection.BadSignature, GateRejection.PairingInvalid -> true
                else -> false
            }
            is GateVersion, is Incompatible, MissingKey, InvitationExpired, is KeyStore -> true
            else -> false
        }

    /**
     * 电脑离线：连不上、超时、没有地址，或者远程访问没开（正在关掉）。这种失败不按退避一遍遍试，等电脑重新上线
     * （[ReconnectPolicy.offlineRetry]）。
     */
    val isOffline: Boolean
        get() = when (this) {
            Timeout, NoAddress, is ConnectionFailed -> true
            is Rejected -> reason == GateRejection.Disabled
            else -> false
        }
}

/**
 * 一台电脑上宿主的连接，各个视图模型经它收发。实现是 [HostConnection]；单元测试换成假的。[send]、[sendInput]
 * 不挂起：消息排进一条队列，由连接按先后写出去，调用方不必等，也不会因为各自开的协程先后不定而乱序。
 */
interface HostLink {
    /** 当前的连接状态；变化同样经 [events] 按先后送到。 */
    val state: StateFlow<LinkState>

    /** 订阅事件：开始收集时先收到一次当前状态（已连上时还有一次 [HostEvent.Ready]），之后的事件按先后到达。 */
    fun events(): Flow<HostEvent>

    /** 发一条控制消息；没连上时丢掉，连上后各页面在 [HostEvent.Ready] 时重新发要的东西。 */
    fun send(message: ClientMsg)

    /** 发输入；[generation] 不是当前这次连接的就丢掉，免得旧通道号落到新连接的别的会话上。 */
    fun sendInput(data: ByteArray, channel: UInt, generation: ULong)

    /** 要连接：没连着就开始连，断了自动重连。停在不能重试的失败（[LinkState.Failed]）上时什么都不做，要 [reconnectNow]。 */
    suspend fun start()

    /** 不要连接了：断开，不再重连。 */
    suspend fun stop()

    /** 正在等下一次重试时立刻重试；失败后停着的也重新开始。 */
    suspend fun reconnectNow()

    /** 用户在不在看这台电脑（它的页面在不在导航栈里）：不在看的连不上时重试得慢一些。默认在看。 */
    fun setFocused(focused: Boolean)

    /** 用户改了设备名：以后每次连上时在 `Hello` 里报这个名字。 */
    suspend fun setDeviceName(name: String)

    /** `Spawn` 这类请求的编号，一条连接上不重复，不发 0。不挂起：取编号和发请求之间连接状态不会变。 */
    fun nextRequestId(): UInt
}

/** 前端报给宿主的身份。 */
data class ClientIdentity(
    /** 构建标识，宿主只用它判断能不能用快照。 */
    val build: String,
    /** 设备名，宿主告诉别的前端「尺寸由谁控制」时显示。 */
    val deviceName: String,
) {
    /** 连上后的第一条消息：手机，只要 VT 重放。 */
    val hello: ClientMsg
        get() = ClientMsg.Hello(
            protocol = protocolVersion,
            build = build,
            client = ClientKind.Mobile,
            caps = Caps(snapshot = false, vtReplay = true, shellReport = true),
            session = null,
            device = deviceName,
        )

    companion object {
        /**
         * 手机报给宿主的构建标识，固定不变。宿主只拿它和自己的比，一样才给快照；桌面端的是 `<版本>.<提交号>`，
         * 和这个永远对不上，所以手机一律用 VT 重放。
         */
        const val mobileBuild: String = "dev.runode.mobile"
    }
}
