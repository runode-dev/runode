package dev.runode.mobile.connection

import dev.runode.mobile.protocol.CertificateFingerprint
import dev.runode.mobile.protocol.GateRejection
import kotlin.coroutines.cancellation.CancellationException
import kotlin.time.Duration
import kotlin.time.Duration.Companion.milliseconds
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.selects.select
import kotlinx.coroutines.supervisorScope

/** 经 iroh 按电脑的公钥（[MachineRecord.node]）开一条连接。 */
typealias TunnelOpener = suspend (node: String, fingerprint: CertificateFingerprint) -> FrameTransport

/** iroh 比局域网的几条路晚这么久才拨，在同一个局域网里时直连先到。 */
internal val tunnelDelay: Duration = 500.milliseconds

/**
 * 连上 [machine]、过门禁后在连接上做 [handshake]，返回它的结果。Bonjour 找到的地址、记着的地址（上次成功的、二维码
 * 里的，按顺序试）和跨网络的 iroh（[tunnel] 不为空、记录里有 `node` 时）同时试，谁先握完手用谁，其余的取消：组播
 * 不通的 Wi-Fi 上 Bonjour 要等满超时，记着的地址不跟着等；iroh 晚 [tunnelDelay] 才拨。晚到的那条也握完了手就交给
 * [close]。不能重试的失败（[LinkFailure.isFatal]）不等别的几条、直接报；都失败时报先到的那个不是「没有地址」的错，
 * 有限速的拒绝时报它，调用方才会等最长的退避。
 */
internal suspend fun <C : Any> dial(
    machine: MachineRecord,
    keyStore: DeviceKeyStore,
    discovery: HostDiscovery,
    open: TransportOpener,
    tunnel: TunnelOpener?,
    handshake: suspend (FrameTransport, DeviceKey) -> C,
    close: (C) -> Unit,
): C {
    val key = keyStore.key(machine.id) ?: throw LinkFailure.MissingKey
    suspend fun attempt(connect: suspend () -> FrameTransport): C {
        val transport = connect()
        try {
            return handshake(transport, key)
        } catch (error: Throwable) {
            transport.close()
            throw error
        }
    }
    val fallback = machine.fallbackTargets
    val fingerprint = machine.fingerprint
    val node = machine.node
    return supervisorScope {
        val pending = mutableListOf<Deferred<C>>(
            async { firstSuccess(fallback) { target -> attempt { open(target, fingerprint) } } },
            async {
                val found = discovery.locate(fingerprint, 2.seconds) ?: throw LinkFailure.NoAddress
                attempt { open(found, fingerprint) }
            },
        )
        if (tunnel != null && node != null) {
            pending += async {
                delay(tunnelDelay)
                attempt { tunnel(node, fingerprint) }
            }
        }
        val all = pending.toList()
        var winner: C? = null
        var failure: Throwable? = null
        var fatal: LinkFailure? = null
        // 有了结果也把别的等完：它们可能也连上了，要关掉，不能留着没人读的连接。
        while (pending.isNotEmpty()) {
            val done = select { pending.forEach { attempt -> attempt.onJoin { attempt } } }
            pending.remove(done)
            val result = runCatching { done.await() }
            val error = result.exceptionOrNull()
            when {
                error == null -> {
                    val connection = result.getOrThrow()
                    if (winner == null && fatal == null) {
                        winner = connection
                        all.forEach { it.cancel() }
                    } else {
                        close(connection)
                    }
                }
                error is CancellationException -> {}
                error is LinkFailure && error.isFatal && winner == null -> {
                    if (fatal == null) fatal = error
                    all.forEach { it.cancel() }
                }
                failure == null || failure == LinkFailure.NoAddress ||
                    error == LinkFailure.Rejected(GateRejection.RateLimited) -> failure = error
            }
        }
        fatal?.let { throw it }
        winner ?: throw (failure ?: LinkFailure.NoAddress)
    }
}
