package dev.runode.mobile.protocol

import java.net.URI
import java.util.UUID

/**
 * 要打开的某台电脑上的某个会话。两处来的：点开电脑推来的等回答通知（[invoke] 的 `userInfo`），以及系统交来的
 * 深链接 `runode://session?machine=<UUID>&session=<会话编号>`（[invoke] 的 `url`）。
 */
data class SessionLink(
    /** 手机登记推送时给这台电脑编的 UUID。 */
    val machine: UUID,
    /** 会话编号，原样带着；对不对由打开它的一方核对。 */
    val session: String,
) {
    /** 深链接；`machine` 按大写的 UUID 写。 */
    val url: URI?
        get() = runCatching {
            URI("runode://$host?machine=${machine.toString().uppercase()}&session=${percentEncode(session)}")
        }.getOrNull()

    companion object {
        /** 链接里 `runode://` 后面的那一段。 */
        const val host: String = "session"

        private val uuidPattern = Regex("^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$")

        /** 认出这种链接；别的链接、缺参数、`machine` 不是 UUID 时为空。 */
        operator fun invoke(url: URI): SessionLink? {
            val items = queryItems(url.toString(), scheme = "runode", host = host) ?: return null
            fun value(name: String) = items.firstOrNull { it.first == name }?.second?.takeIf { it.isNotEmpty() }
            val machine = value("machine")?.let(::parseUuid) ?: return null
            val session = value("session") ?: return null
            return SessionLink(machine, session)
        }

        /**
         * 从电脑推来的等回答通知的数据读：宿主的 `push::notify_payload` 在 `aps` 外面带着 `machine`（登记时报的
         * UUID）和 `session`。不是这种通知、`machine` 不是 UUID、`session` 为空时为空。
         */
        operator fun invoke(userInfo: Map<*, *>): SessionLink? {
            val machine = (userInfo["machine"] as? String)?.let(::parseUuid) ?: return null
            val session = (userInfo["session"] as? String)?.takeIf { it.isNotEmpty() } ?: return null
            return SessionLink(machine, session)
        }

        /** 只认 8-4-4-4-12 的写法，大小写都行。 */
        private fun parseUuid(text: String): UUID? =
            if (uuidPattern.matches(text)) UUID.fromString(text) else null
    }
}
