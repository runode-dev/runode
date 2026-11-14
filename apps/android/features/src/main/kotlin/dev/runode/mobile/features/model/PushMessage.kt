package dev.runode.mobile.features.model

import androidx.annotation.StringRes
import dev.runode.mobile.features.R
import dev.runode.mobile.protocol.SessionLink

/**
 * 电脑经中转、FCM 推来的一条通知：宿主 `push::fcm_message` 的 `data`，值都是字符串。FCM 只送数据，通知由 App 用自己的
 * 文案出：标题按 [kind] 和 agent 定，副标题是会话标题和电脑名，正文是屏幕上的几行或任务的结果。
 */
data class PushMessage(
    val link: SessionLink,
    val kind: Kind,
    val title: UiText,
    val subtitle: UiText,
    /** 用换行连着的几行；没有时为空。 */
    val body: String,
) {
    /** 宿主的 `push::NotificationKind`。 */
    enum class Kind(val wire: String) {
        Blocked("blocked"),
        TodoDone("todo_done"),
        TodoFailed("todo_failed"),
    }

    /** 发到哪个通知渠道：等回答的单独一个，用户能只留它。 */
    val channel: PushChannel get() = if (kind == Kind.Blocked) PushChannel.Blocked else PushChannel.Tasks

    /** 通知的 tag：同一个会话的新通知替掉旧的。 */
    val tag: String get() = link.session

    /** 通知的分组：同一台电脑的归在一起。 */
    val group: String get() = link.machine.toString().uppercase()

    companion object {
        /** 读 FCM 消息的 `data`；`machine`、`session` 不对，或者是比自己新的电脑才有的 `kind` 时为空，不出通知。 */
        fun parse(data: Map<String, String>): PushMessage? {
            val link = SessionLink(data) ?: return null
            val kind = Kind.entries.firstOrNull { it.wire == data["kind"] } ?: return null
            val agent = data["agent"].orEmpty()
            val title = when (kind) {
                Kind.Blocked ->
                    if (agent.isEmpty()) UiText.res(R.string.agent_blocked_title_unnamed) else UiText.res(R.string.agent_blocked_title, agent)
                Kind.TodoDone -> UiText.res(R.string.todo_done_title)
                Kind.TodoFailed -> UiText.res(R.string.todo_failed_title)
            }
            val subtitle = UiText.res(R.string.detail_separator, data["title"].orEmpty(), data["machine_name"].orEmpty())
            return PushMessage(link, kind, title, subtitle, data["body"].orEmpty())
        }
    }
}

/** 推送通知的渠道：[id] 是建渠道时用的，[label] 是系统设置里显示的名字。 */
enum class PushChannel(val id: String, @param:StringRes val label: Int) {
    Blocked("agent_blocked", R.string.notification_channel_blocked),
    Tasks("tasks", R.string.notification_channel_tasks),
}
