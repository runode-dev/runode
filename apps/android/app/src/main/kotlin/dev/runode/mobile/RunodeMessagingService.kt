package dev.runode.mobile

import androidx.core.content.ContextCompat
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage
import dev.runode.mobile.features.model.PushMessage

/**
 * FCM 交来的 token 和消息。token 交给 [AndroidPushNotifications]，由 `PushRegistration` 向各台电脑登记；消息只带数据
 * （宿主的 `push::fcm_message`），照 [PushMessage] 拼成通知，App 在前台时先问 [AndroidPushNotifications.shows] 出不出。
 */
class RunodeMessagingService : FirebaseMessagingService() {
    override fun onNewToken(token: String) {
        AndroidPushNotifications.shared(this)?.received(token)
    }

    override fun onMessageReceived(message: RemoteMessage) {
        val push = PushMessage.parse(message.data) ?: return
        val system = AndroidPushNotifications.shared(this) ?: return
        // 收消息在后台线程上；出不出要读界面的状态，回主线程问。
        ContextCompat.getMainExecutor(this).execute {
            if (system.shows(push.link)) system.show(push)
        }
    }
}
