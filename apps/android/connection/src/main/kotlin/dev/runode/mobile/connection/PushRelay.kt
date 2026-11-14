package dev.runode.mobile.connection

import dev.runode.mobile.protocol.PushError
import dev.runode.mobile.protocol.RelayHandle
import dev.runode.mobile.protocol.RelayRegister
import dev.runode.mobile.protocol.protocolJson
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URI
import kotlin.time.Duration
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** 官方 App 的推送中转服务：给一台电脑换推送句柄，测试换成假的。 */
interface PushRelay {
    suspend fun handle(request: RelayRegister): String
}

/** 中转服务回了 200 以外的状态，或者没回话（[status] 为 0）。 */
data class PushRelayError(val status: Int, val reason: String) :
    Exception("push relay: HTTP $status" + if (reason.isEmpty()) "" else " $reason")

/** 经 HTTPS 找中转服务换句柄。 */
class HttpPushRelay(val base: URI) : PushRelay {
    override suspend fun handle(request: RelayRegister): String = withContext(Dispatchers.IO) {
        val url = URI(base.toString().trimEnd('/') + RelayRegister.path).toURL()
        val connection = url.openConnection() as HttpURLConnection
        try {
            connection.requestMethod = "POST"
            connection.connectTimeout = timeout.inWholeMilliseconds.toInt()
            connection.readTimeout = timeout.inWholeMilliseconds.toInt()
            connection.doOutput = true
            connection.setRequestProperty("Content-Type", "application/json")
            connection.outputStream.use {
                it.write(protocolJson.encodeToString(RelayRegister.serializer(), request).encodeToByteArray())
            }
            val status = try {
                connection.responseCode
            } catch (_: IOException) {
                throw PushRelayError(0, "")
            }
            if (status != 200) {
                val body = connection.errorStream?.use { it.readBytes().decodeToString() }.orEmpty()
                val reason = runCatching { protocolJson.decodeFromString(PushError.serializer(), body).reason }.getOrDefault("")
                throw PushRelayError(status, reason)
            }
            val body = connection.inputStream.use { it.readBytes().decodeToString() }
            protocolJson.decodeFromString(RelayHandle.serializer(), body).handle
        } finally {
            connection.disconnect()
        }
    }

    companion object {
        /** 官方中转服务。 */
        val official: HttpPushRelay = HttpPushRelay(URI("https://push.runode.dev"))

        /** 换一次句柄最多等这么久。 */
        internal val timeout: Duration = 10.seconds

        /** 按构建时配的基址（比如 BuildConfig 里的值）建；没配、配的不是带主机的 https 时用 [official]。 */
        fun configured(raw: String?): HttpPushRelay {
            val uri = raw?.trim()?.let { runCatching { URI(it) }.getOrNull() }
                ?.takeIf { it.scheme == "https" && !it.host.isNullOrEmpty() }
            return uri?.let(::HttpPushRelay) ?: official
        }
    }
}
