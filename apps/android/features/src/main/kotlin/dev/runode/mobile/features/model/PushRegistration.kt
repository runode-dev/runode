package dev.runode.mobile.features.model

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import dev.runode.mobile.connection.HostLink
import dev.runode.mobile.connection.MachineRecord
import dev.runode.mobile.connection.PushRelay
import dev.runode.mobile.protocol.ApnsEnv
import dev.runode.mobile.protocol.ClientMsg
import dev.runode.mobile.protocol.HostMsg
import dev.runode.mobile.protocol.PushService
import dev.runode.mobile.protocol.RelayRegister
import dev.runode.mobile.protocol.SessionLink
import java.util.UUID
import kotlin.coroutines.cancellation.CancellationException
import kotlin.time.Duration
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.launch

/**
 * 通知的系统接口：Android 上是 FCM 和 `NotificationManager`，由 App 入口给，测试换成假的。等回答的通知由电脑经中转、FCM
 * 推来，App 只要通知权限、拿推送的 token、在前台时清掉已经答完的。
 */
interface PushNotifications {
    /** 要通知权限：没问过时弹系统的询问框，问过的直接给现在的结果。允许时为真。 */
    suspend fun requestAuthorization(): Boolean

    /** 要推送的 token，拿到后从 [deviceTokens] 给出来。 */
    fun registerForRemoteNotifications()

    /** 推送的 token：每拿到一次给一次，可能和上次一样。 */
    fun deviceTokens(): Flow<String>

    /** 要 token 失败了，每次给出原因。 */
    fun registrationFailures(): Flow<String>

    /** 通知栏里还留着的推送通知。 */
    suspend fun delivered(): List<DeliveredNotification>

    /** 从通知栏删掉这些（[DeliveredNotification.id]）。 */
    fun removeDelivered(ids: List<String>)
}

/** 通知栏里的一条推送通知：[id] 是系统接口给的标识，[link] 是它是哪台电脑上哪个会话的。 */
data class DeliveredNotification(val id: String, val link: SessionLink)

/** 登记推送时报给电脑的 App 身份：APNs 环境、bundle id（Android 上是 applicationId）和推送经哪家送到。 */
data class PushIdentity(val environment: ApnsEnv, val bundle: String, val service: PushService) {
    companion object {
        /**
         * 按 App 的 applicationId 定：官方 App（[RelayRegister.officialBundle]）经中转、FCM 推，环境固定填 production；别的
         * applicationId 没有能推它的 FCM 项目，为空，不登记推送。
         */
        fun of(applicationId: String): PushIdentity? =
            if (applicationId == RelayRegister.officialBundle) {
                PushIdentity(ApnsEnv.Production, applicationId, PushService.Fcm)
            } else {
                null
            }
    }
}

/**
 * 向各台电脑登记推送：电脑上有会话的 agent 停下来等回答时，用这里交出的凭据给手机推一条通知，手机不用连着。有中转服务
 * （[relay]）时交的是先为这台电脑换来的句柄，电脑拿不到推送的 token；换不到时和电脑回错一样记下原因。
 *
 * 打开着提醒时，每台电脑连上（`Ready`）都要一次通知权限，有了结果才登记；允许了才要推送的 token，token 变了再登记一次。
 * 系统不允许、或设置里关掉提醒时对电脑注销（`token` 为空的同一条消息）：电脑记着登记，不注销它会接着推问题的内容。
 *
 * 电脑回 `Done` 算办好了；回带编号的 `Error` 记下原因，只在设置页里显示；[replyTimeout] 内没回话就不再等。在主线程上用。
 */
class PushRegistration(
    private val system: PushNotifications?,
    private val identity: PushIdentity?,
    private val relay: PushRelay? = null,
    enabled: Boolean,
    /** 等电脑回话最多等多久。 */
    internal val replyTimeout: Duration = 5.seconds,
) {
    /** 一台电脑上登记的结果。 */
    sealed class Status {
        /** 电脑收下了这次登记（或注销）。 */
        data object Registered : Status()

        /** 电脑回了错误，或者换不到句柄。 */
        data class Failed(val message: UiText) : Status()
    }

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    /** 各台电脑最近一次登记的结果；还没结果的没有。断开后留着，重新登记有了结果再换。 */
    var statuses by mutableStateOf<Map<UUID, Status>>(emptyMap())
        private set

    /** 系统设置里允许本 App 发通知；还没问过、不知道（没有系统接口）时当作允许。 */
    var systemAllowed by mutableStateOf(true)
        private set

    /** 最近一次要推送的 token 失败的原因；之后拿到 token 时清掉。设置页据此提示。 */
    var tokenError by mutableStateOf<UiText?>(null)
        private set

    private var enabledValue by mutableStateOf(enabled)

    /** 设置里打开了等回答提醒。 */
    var enabled: Boolean
        get() = enabledValue
        set(value) {
            if (value == enabledValue) return
            enabledValue = value
            if (value) authorize(peers.keys.toList()) else registerAll()
        }

    /** 为连着的各台电脑换来的句柄和换时的 token；token 变了再换。 */
    private val handles = mutableMapOf<UUID, Pair<String, String>>()

    /** 现在的推送 token；还没拿到时为空。 */
    internal var token: String? = null
        private set

    /** 连着、能发消息的电脑。 */
    private val peers = mutableMapOf<UUID, Peer>()

    /** 每台电脑在等回话的那次登记；新登记一次就换掉旧的。 */
    private val pending = mutableMapOf<UUID, Pending>()
    private var attempts = 0

    /**
     * 要删掉、正在注销的电脑：直到 [forget] 都不再登记，不然权限、token 晚到的那次登记会顶掉注销，删掉以后电脑还记着这部
     * 手机的 token。
     */
    private val leaving = mutableSetOf<UUID>()

    private class Peer(var name: String, val link: HostLink)

    private class Pending(val attempt: Int) {
        /** 取到请求编号、发出去以后才有。 */
        var req: UInt? = null
        var job: Job? = null
    }

    init {
        if (system != null) {
            scope.launch { system.deviceTokens().collect { tokenChanged(it) } }
            scope.launch { system.registrationFailures().collect { tokenError = UiText.Raw(it) } }
        }
    }

    /** 这台电脑连上了（`HostEvent.Ready`）：登记一次；打开着提醒时先看系统还允不允许发通知，有了结果再登记。 */
    internal fun connected(machine: MachineRecord, link: HostLink) {
        peers[machine.id] = Peer(machine.name, link)
        // 删掉了、等着注销的（见 `unregister`），连上了也不再登记。
        if (machine.deletedAt != null) leaving += machine.id
        if (enabled) authorize(listOf(machine.id)) else register(machine.id)
    }

    /**
     * 这台电脑断开了：不再等它的回话，也忘掉为它换来的句柄。电脑不认句柄时会删掉登记、不告诉手机，下次连上重新换一个，
     * 不再交回同一个。
     */
    internal fun disconnected(id: UUID) {
        peers.remove(id)
        pending.remove(id)?.job?.cancel()
        handles.remove(id)
    }

    /** 这台电脑在手机上改了名字：连着的话按新名字再登记一次，推送里带的电脑名跟着变。 */
    internal fun renamed(machine: MachineRecord) {
        val peer = peers[machine.id] ?: return
        if (peer.name == machine.name) return
        peer.name = machine.name
        register(machine.id)
    }

    /**
     * 这台电脑要删掉了：连着的话先注销，等电脑回话（最多 [timeout]）再返回，免得删掉以后电脑上的 agent 等回答时还往这部
     * 手机推它的提醒，点开又找不到这台电脑。电脑注销好了或者从来不登记推送时为真；电脑回错（写不了设备表，登记还在）、
     * 没回话时为假，没连着时马上返回假。
     */
    internal suspend fun unregister(id: UUID, timeout: Duration): Boolean {
        leaving += id
        if (system == null || identity == null) return true
        if (peers[id] == null) return false
        statuses = statuses - id
        register(id, unregistering = true, timeout = timeout)
        // 回话到了（`handle`）、断开了（`disconnected`）或者超时，等回话的协程都会结束。
        pending[id]?.job?.join()
        return statuses[id] == Status.Registered
    }

    /** 这台电脑删掉了：连它的结果一起忘掉。 */
    internal fun forget(id: UUID) {
        disconnected(id)
        statuses = statuses - id
        leaving -= id
    }

    /** 这台电脑发来的消息：对上在等的那次登记的回话就记下结果。 */
    internal fun handle(message: HostMsg, from: UUID) {
        val req = pending[from]?.req ?: return
        val status = when {
            message is HostMsg.Done && message.req == req -> Status.Registered
            message is HostMsg.Error && message.req == req -> Status.Failed(UiText.Raw(message.message))
            else -> return
        }
        pending.remove(from)?.job?.cancel()
        statuses = statuses + (from to status)
    }

    private fun tokenChanged(token: String) {
        tokenError = null
        if (token == this.token) return
        this.token = token
        // 系统不允许时已经注销过了，新 token 用不上。
        if (!enabled || !systemAllowed) return
        registerAll()
    }

    /**
     * 要通知权限，记下系统允不允许，允许时要推送的 token；然后对 [ids] 这几台登记（不允许时是注销）。允不允许变了时对连着的
     * 都重来一遍（token 没变时 [tokenChanged] 不会再登记）。
     */
    private fun authorize(ids: List<UUID>) {
        val system = system ?: return
        if (identity == null) return
        scope.launch {
            val allowed = system.requestAuthorization()
            val changed = systemAllowed != allowed
            systemAllowed = allowed
            if (allowed) system.registerForRemoteNotifications()
            if (!enabled) return@launch
            if (changed) registerAll() else ids.forEach { register(it) }
        }
    }

    private fun registerAll() {
        for (id in peers.keys.toList()) register(id)
    }

    /**
     * 对这台电脑登记（关着提醒、系统不允许发通知或者 [unregistering] 时注销）。打开着提醒却还没拿到 token 时先不发，拿到了
     * 再发。[timeout] 为空时等 [replyTimeout]。
     */
    private fun register(id: UUID, unregistering: Boolean = false, timeout: Duration? = null) {
        val peer = peers[id] ?: return
        val identity = identity ?: return
        if (system == null || (!unregistering && id in leaving)) return
        val token = if (enabled && systemAllowed && !unregistering) this.token ?: return else null
        pending.remove(id)?.job?.cancel()
        attempts += 1
        val entry = Pending(attempts)
        pending[id] = entry
        val wait = timeout ?: replyTimeout
        entry.job = scope.launch {
            val credential = try {
                credential(id, token, identity)
            } catch (error: CancellationException) {
                throw error
            } catch (error: Exception) {
                if (pending[id] !== entry) return@launch
                pending.remove(id)
                statuses = statuses + (id to Status.Failed(UiText.Raw(error.message ?: error.toString())))
                return@launch
            }
            val req = peer.link.nextRequestId()
            if (pending[id] !== entry) return@launch
            entry.req = req
            peer.link.send(
                ClientMsg.PushRegister(
                    req = req,
                    token = credential,
                    env = identity.environment,
                    bundle = identity.bundle,
                    machine = id.toString().uppercase(),
                    machineName = peer.name,
                    service = identity.service,
                ),
            )
            delay(wait)
            if (pending[id] === entry) pending.remove(id)
        }
    }

    /** 交给这台电脑的凭据：有中转服务时是换来的句柄，没有时就是推送的 token；注销（[token] 为空）时为空。 */
    private suspend fun credential(id: UUID, token: String?, identity: PushIdentity): String? {
        val relay = relay
        if (token == null || relay == null) return token
        handles[id]?.let { (known, handle) -> if (known == token) return handle }
        val request = RelayRegister(
            env = identity.environment,
            token = token,
            machine = id.toString().uppercase(),
            service = identity.service,
        )
        val handle = relay.handle(request)
        handles[id] = token to handle
        return handle
    }
}
