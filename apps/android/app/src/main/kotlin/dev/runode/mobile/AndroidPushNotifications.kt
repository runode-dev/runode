package dev.runode.mobile

import android.Manifest
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import com.google.firebase.FirebaseApp
import com.google.firebase.messaging.FirebaseMessaging
import dev.runode.mobile.connection.HttpPushRelay
import dev.runode.mobile.features.model.AppDependencies
import dev.runode.mobile.features.model.DeliveredNotification
import dev.runode.mobile.features.model.PushChannel
import dev.runode.mobile.features.model.PushIdentity
import dev.runode.mobile.features.model.PushMessage
import dev.runode.mobile.features.model.PushNotifications
import dev.runode.mobile.protocol.SessionLink
import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableSharedFlow

/**
 * 用 FCM 拿推送的 token，用 `NotificationManager` 出通知、查和清通知栏。进程里只有一份（[shared]），[RunodeMessagingService]
 * 交来的 token 和消息都经它。
 */
class AndroidPushNotifications private constructor(private val context: Context) : PushNotifications {
    private val manager: NotificationManager = context.getSystemService(NotificationManager::class.java)

    /** 最近一次的 token 留着，后建的 [deviceTokens] 订阅方也拿得到。 */
    private val tokens = MutableSharedFlow<String>(replay = 1, onBufferOverflow = BufferOverflow.DROP_OLDEST)
    private val failures = MutableSharedFlow<String>(extraBufferCapacity = 4, onBufferOverflow = BufferOverflow.DROP_OLDEST)

    /** 界面层装上的、弹系统的询问框要通知权限的一步，在主线程上调；没装时不问，只看现在有没有权限。 */
    @Volatile var askPermission: (suspend () -> Boolean)? = null

    /** 推来的通知出不出，由 App 入口装上，没装时都出；在主线程上调。 */
    @Volatile var shows: (SessionLink) -> Boolean = { true }

    override suspend fun requestAuthorization(): Boolean {
        if (!permitted()) {
            val ask = askPermission ?: return false
            if (!ask()) return false
        }
        return manager.areNotificationsEnabled()
    }

    override fun registerForRemoteNotifications() {
        FirebaseMessaging.getInstance().token.addOnCompleteListener { task ->
            if (task.isSuccessful) {
                task.result?.let(::received)
            } else {
                failures.tryEmit(task.exception?.message ?: "cannot get an FCM token")
            }
        }
    }

    /** FCM 给的 token（[RunodeMessagingService.onNewToken] 和 [registerForRemoteNotifications] 拿到的）。 */
    fun received(token: String) {
        tokens.tryEmit(token)
    }

    override fun deviceTokens(): Flow<String> = tokens

    override fun registrationFailures(): Flow<String> = failures

    override suspend fun delivered(): List<DeliveredNotification> = manager.activeNotifications.mapNotNull { status ->
        val tag = status.tag ?: return@mapNotNull null
        if (status.id != NOTIFICATION_ID) return@mapNotNull null
        val extras = status.notification.extras
        val link = SessionLink(mapOf("machine" to extras.getString(EXTRA_MACHINE), "session" to extras.getString(EXTRA_SESSION)))
        link?.let { DeliveredNotification(tag, it) }
    }

    override fun removeDelivered(ids: List<String>) {
        for (tag in ids) manager.cancel(tag, NOTIFICATION_ID)
    }

    /**
     * 出一条通知：tag 是会话，同一个会话的新通知替掉旧的；同一台电脑的归一组；点开时带着电脑和会话打开 [MainActivity]。
     * 系统不让本 App 发通知时不出。
     */
    fun show(message: PushMessage) {
        if (!manager.areNotificationsEnabled()) return
        createChannels()
        val resources = context.resources
        val subtitle = message.subtitle.resolve(resources)
        val machine = message.link.machine.toString().uppercase()
        val open = Intent(context, MainActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
            .putExtra(EXTRA_MACHINE, machine)
            .putExtra(EXTRA_SESSION, message.link.session)
        val pending = PendingIntent.getActivity(
            context,
            message.tag.hashCode(),
            open,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val extras = Bundle().apply {
            putString(EXTRA_MACHINE, machine)
            putString(EXTRA_SESSION, message.link.session)
        }
        val blocked = message.kind == PushMessage.Kind.Blocked
        val builder = NotificationCompat.Builder(context, message.channel.id)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(message.title.resolve(resources))
            .setContentIntent(pending)
            .setAutoCancel(true)
            .setGroup(message.group)
            .setCategory(if (blocked) NotificationCompat.CATEGORY_MESSAGE else NotificationCompat.CATEGORY_STATUS)
            .setPriority(if (blocked) NotificationCompat.PRIORITY_HIGH else NotificationCompat.PRIORITY_DEFAULT)
            .addExtras(extras)
        if (message.body.isEmpty()) {
            builder.setContentText(subtitle)
        } else {
            builder.setSubText(subtitle)
                .setContentText(message.body)
                .setStyle(NotificationCompat.BigTextStyle().bigText(message.body))
        }
        manager.notify(message.tag, NOTIFICATION_ID, builder.build())
    }

    private fun permitted(): Boolean =
        Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED

    /** 建通知渠道；已经建过的系统不再改它，用户在系统设置里改过的照旧。 */
    private fun createChannels() {
        for (channel in PushChannel.entries) {
            val importance =
                if (channel == PushChannel.Blocked) NotificationManager.IMPORTANCE_HIGH else NotificationManager.IMPORTANCE_DEFAULT
            manager.createNotificationChannel(NotificationChannel(channel.id, context.getString(channel.label), importance))
        }
    }

    companion object {
        /** 推送通知都用这个编号，靠 tag（会话）区分。 */
        const val NOTIFICATION_ID: Int = 1

        /** 点开通知交给 [MainActivity] 的电脑（登记推送时报的 UUID）和会话编号。 */
        const val EXTRA_MACHINE: String = "dev.runode.mobile.extra.MACHINE"
        const val EXTRA_SESSION: String = "dev.runode.mobile.extra.SESSION"

        @Volatile private var instance: AndroidPushNotifications? = null

        /** 进程里的那一份；这个构建没带 Firebase 的配置（没有 google-services.json）时为空，不登记推送。 */
        fun shared(context: Context): AndroidPushNotifications? {
            if (FirebaseApp.getApps(context).isEmpty()) return null
            return instance ?: synchronized(this) {
                instance ?: AndroidPushNotifications(context.applicationContext).also { instance = it }
            }
        }
    }
}

/** 把推送用到的三项装进来：通知的系统接口、按 applicationId 定的 App 身份、编译时配的中转。 */
fun AppDependencies.installPush(context: Context) {
    notifications = AndroidPushNotifications.shared(context)
    pushIdentity = PushIdentity.of(BuildConfig.APPLICATION_ID)
    pushRelay = HttpPushRelay.configured(BuildConfig.RUNODE_PUSH_RELAY)
}
