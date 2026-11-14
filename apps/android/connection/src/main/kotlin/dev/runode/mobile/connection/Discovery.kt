package dev.runode.mobile.connection

import android.annotation.TargetApi
import android.content.Context
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import dev.runode.mobile.protocol.CertificateFingerprint
import dev.runode.mobile.protocol.bonjourFingerprintKey
import java.net.Inet4Address
import java.net.InetAddress
import java.util.concurrent.Executor
import kotlin.coroutines.cancellation.CancellationException
import kotlin.time.Duration
import kotlin.time.Duration.Companion.hours
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.flow.firstOrNull
import kotlinx.coroutines.flow.mapNotNull
import kotlinx.coroutines.isActive
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withTimeoutOrNull

/** 按证书指纹在局域网里找电脑现在的地址。 */
interface HostDiscovery {
    /** [timeout] 内找到 TXT 里 `fp` 对得上的服务就返回它，找不到返回空。 */
    suspend fun locate(fingerprint: CertificateFingerprint, timeout: Duration): TransportTarget?

    /**
     * 一直等到这台电脑在局域网里公布服务（电脑离线时连接据此知道它回来了），只有取消才提前返回。默认一直找到为止；
     * 找不了（比如没有 Wi-Fi）时 [locate] 会马上返回空，歇一会儿再找，不空转。
     */
    suspend fun awaitPresence(fingerprint: CertificateFingerprint) {
        while (currentCoroutineContext().isActive) {
            if (locate(fingerprint, 1.hours) != null) return
            delay(30.seconds)
        }
    }
}

/** 连这台电脑时要挨个试的地址：Bonjour 找到的在前，再是 [fallback]。 */
internal suspend fun HostDiscovery.targets(
    fingerprint: CertificateFingerprint,
    fallback: List<TransportTarget>,
): List<TransportTarget> = listOfNotNull(locate(fingerprint, 2.seconds)) + fallback

/**
 * 按顺序试 [targets]，返回第一个成功的结果。不能重试的失败（[LinkFailure.isFatal]）和取消直接抛，其余的记下，
 * 都不成时抛最后一个；一个地址也没有时抛 [LinkFailure.NoAddress]。
 */
internal suspend fun <R> firstSuccess(targets: List<TransportTarget>, attempt: suspend (TransportTarget) -> R): R {
    var lastError: Throwable = LinkFailure.NoAddress
    for (target in targets) {
        currentCoroutineContext().ensureActive()
        try {
            return attempt(target)
        } catch (failure: LinkFailure) {
            if (failure.isFatal) throw failure
            lastError = failure
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (error: Exception) {
            lastError = error
        }
    }
    throw lastError
}

/** 不找，直接用记下的地址；测试和没有局域网权限时用。 */
object NoDiscovery : HostDiscovery {
    override suspend fun locate(fingerprint: CertificateFingerprint, timeout: Duration): TransportTarget? = null

    /** 找不着，只能等别的事（兜底的定时、用户点重试）来叫。 */
    override suspend fun awaitPresence(fingerprint: CertificateFingerprint): Unit = awaitCancellation()
}

/**
 * 用 NSD 找 `_runode._tcp`：电脑开着远程访问时公布这个服务，TXT 记录里有门禁版本和证书指纹（[bonjourFingerprintKey]）。
 * 找的时候拿着 Wi-Fi 的 MulticastLock，不然组播的回答会被丢掉。
 */
class NsdDiscovery(context: Context) : HostDiscovery {
    private val nsd: NsdManager = context.getSystemService(NsdManager::class.java)
    private val wifi: WifiManager? = context.applicationContext.getSystemService(WifiManager::class.java)

    override suspend fun locate(fingerprint: CertificateFingerprint, timeout: Duration): TransportTarget? {
        val lock = wifi?.createMulticastLock("runode-bonjour")?.apply { setReferenceCounted(false) }
        lock?.acquire()
        try {
            return withTimeoutOrNull(timeout) {
                found()
                    .mapNotNull { resolve(it) }
                    .firstOrNull { info ->
                        val fp = info.attributes[bonjourFingerprintKey]?.decodeToString()
                        fp != null && CertificateFingerprint(fp) == fingerprint
                    }
                    ?.let { info -> address(info)?.let { TransportTarget(it, info.port.toUShort(), "Bonjour: ${info.serviceName}") } }
            }
        } finally {
            lock?.release()
        }
    }

    /** 浏览到的服务，还没解析；浏览开不起来时流就结束。 */
    private fun found(): Flow<NsdServiceInfo> = callbackFlow {
        val listener = object : NsdManager.DiscoveryListener {
            override fun onServiceFound(info: NsdServiceInfo) {
                trySend(info)
            }

            override fun onStartDiscoveryFailed(serviceType: String?, errorCode: Int) {
                close()
            }

            override fun onDiscoveryStarted(serviceType: String?) {}

            override fun onDiscoveryStopped(serviceType: String?) {}

            override fun onServiceLost(info: NsdServiceInfo?) {}

            override fun onStopDiscoveryFailed(serviceType: String?, errorCode: Int) {}
        }
        nsd.discoverServices(SERVICE_TYPE, NsdManager.PROTOCOL_DNS_SD, listener)
        awaitClose { runCatching { nsd.stopServiceDiscovery(listener) } }
    }

    /** 解析出地址和 TXT；一个服务解析不出来时跳过它，不耽误别的。 */
    private suspend fun resolve(info: NsdServiceInfo): NsdServiceInfo? = withTimeoutOrNull(RESOLVE_TIMEOUT) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) resolveWithCallback(info) else resolveLegacy(info)
    }

    @TargetApi(Build.VERSION_CODES.UPSIDE_DOWN_CAKE)
    private suspend fun resolveWithCallback(info: NsdServiceInfo): NsdServiceInfo? {
        val result = CompletableDeferred<NsdServiceInfo?>()
        val callback = object : NsdManager.ServiceInfoCallback {
            override fun onServiceInfoCallbackRegistrationFailed(errorCode: Int) {
                result.complete(null)
            }

            override fun onServiceUpdated(info: NsdServiceInfo) {
                if (info.hostAddresses.isNotEmpty()) result.complete(info)
            }

            override fun onServiceLost() {}

            override fun onServiceInfoCallbackUnregistered() {}
        }
        nsd.registerServiceInfoCallback(info, directExecutor, callback)
        try {
            return result.await()
        } finally {
            runCatching { nsd.unregisterServiceInfoCallback(callback) }
        }
    }

    /** 旧系统上同一时刻只能解析一个服务，几台电脑的连接一起找时排队。 */
    @Suppress("DEPRECATION")
    private suspend fun resolveLegacy(info: NsdServiceInfo): NsdServiceInfo? = legacyResolves.withLock {
        val result = CompletableDeferred<NsdServiceInfo?>()
        nsd.resolveService(
            info,
            object : NsdManager.ResolveListener {
                override fun onResolveFailed(info: NsdServiceInfo?, errorCode: Int) {
                    result.complete(null)
                }

                override fun onServiceResolved(info: NsdServiceInfo) {
                    result.complete(info)
                }
            },
        )
        result.await()
    }

    /** 解析出的地址里先挑 IPv4：链路本地的 IPv6 常常缺着作用域，连不上。 */
    @Suppress("DEPRECATION")
    private fun address(info: NsdServiceInfo): String? {
        val addresses: List<InetAddress> =
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) info.hostAddresses else listOfNotNull(info.host)
        return (addresses.firstOrNull { it is Inet4Address } ?: addresses.firstOrNull())?.hostAddress
    }

    companion object {
        const val SERVICE_TYPE: String = "_runode._tcp"
        private val RESOLVE_TIMEOUT = 3.seconds
        private val directExecutor = Executor { it.run() }
        private val legacyResolves = Mutex()
    }
}

/**
 * 本机网络的变化。经记着的地址连的电脑离线时，[HostConnection] 据此再试一次，离上次失败不短于退避的时长。
 *
 * shortcut: 失败和开始监听之间网络恰好恢复时漏一次唤醒，由 offlineRetry 兜底；要根治就让监听跟着连接常驻、按网络的代数比较
 */
class NetworkChanges(context: Context) {
    private val connectivity: ConnectivityManager = context.getSystemService(ConnectivityManager::class.java)

    /**
     * 等本机网络恢复或变化：开始等时的默认网络算基准，之后换了默认网络（Wi-Fi 回来、Wi-Fi 换成蜂窝）或者它的
     * 链路属性变了（VPN 连好、换了地址）时返回。
     */
    suspend fun await() {
        val changed = CompletableDeferred<Unit>()
        val baseline = connectivity.activeNetwork
        val callback = object : ConnectivityManager.NetworkCallback() {
            /** 注册时系统先把基准网络现在的链路属性报一次，那一次不算变化。 */
            private var baselineReported = false

            override fun onAvailable(network: Network) {
                if (network != baseline) changed.complete(Unit)
            }

            override fun onLinkPropertiesChanged(network: Network, linkProperties: LinkProperties) {
                if (network != baseline || baselineReported) changed.complete(Unit) else baselineReported = true
            }
        }
        connectivity.registerDefaultNetworkCallback(callback)
        try {
            changed.await()
        } finally {
            runCatching { connectivity.unregisterNetworkCallback(callback) }
        }
    }
}
