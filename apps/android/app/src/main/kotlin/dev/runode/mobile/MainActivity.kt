package dev.runode.mobile

import android.Manifest
import android.content.Intent
import android.hardware.biometrics.BiometricManager.Authenticators
import android.hardware.biometrics.BiometricPrompt
import android.os.Bundle
import android.os.CancellationSignal
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import dev.runode.mobile.features.RunodeApp
import dev.runode.mobile.features.model.AppModel
import dev.runode.mobile.features.model.UiText
import dev.runode.mobile.protocol.SessionLink
import java.net.URI
import javax.crypto.Cipher
import kotlin.coroutines.resume
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.suspendCancellableCoroutine

/**
 * 唯一的 Activity：放 [RunodeApp]，接系统交来的 `runode://` 链接和点开的通知，替界面弹系统的询问框（通知、麦克风权限）
 * 和身份验证。
 */
class MainActivity : ComponentActivity() {
    private lateinit var app: AppModel

    /** 在等系统询问框回话的那次要权限；同一时刻只问一样。 */
    private var permissionReply: CompletableDeferred<Boolean>? = null
    private val permission = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        permissionReply?.complete(granted)
        permissionReply = null
    }
    private val askNotifications: suspend () -> Boolean = { ask(Manifest.permission.POST_NOTIFICATIONS) }
    private val askMicrophone: suspend () -> Boolean = { ask(Manifest.permission.RECORD_AUDIO) }
    private val authenticate: suspend (Cipher, UiText) -> Cipher? = { cipher, reason -> authenticate(cipher, reason) }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val demo = intent?.getStringExtra(DemoComposition.EXTRA)
        app = (application as RunodeApplication).appModel(demo)
        AndroidPushNotifications.shared(this)?.askPermission = askNotifications
        app.dictation.requestPermission = askMicrophone
        (application as RunodeApplication).dependencies?.authenticate = authenticate
        if (savedInstanceState == null) {
            handle(intent)
            if (demo != null) DemoComposition.open(app, demo)
        }
        setContent { RunodeApp(app) }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handle(intent)
    }

    override fun onDestroy() {
        // 询问框跟着这个 Activity 没了，等着的那次当作没允许，下次要时再问。
        permissionReply?.complete(false)
        permissionReply = null
        AndroidPushNotifications.shared(this)?.let { if (it.askPermission === askNotifications) it.askPermission = null }
        if (app.dictation.requestPermission === askMicrophone) app.dictation.requestPermission = null
        (application as RunodeApplication).dependencies?.let { if (it.authenticate === authenticate) it.authenticate = null }
        super.onDestroy()
    }

    /** 系统交来的 `runode://` 链接，或者点开了推送通知（带着电脑和会话）。 */
    private fun handle(intent: Intent?) {
        intent ?: return
        intent.data?.let { uri ->
            runCatching { URI(uri.toString()) }.getOrNull()?.let(app::open)
            return
        }
        val machine = intent.getStringExtra(AndroidPushNotifications.EXTRA_MACHINE) ?: return
        val session = intent.getStringExtra(AndroidPushNotifications.EXTRA_SESSION) ?: return
        app.open(SessionLink(mapOf("machine" to machine, "session" to session)) ?: return)
    }

    private suspend fun ask(name: String): Boolean {
        val reply = CompletableDeferred<Boolean>()
        permissionReply?.complete(false)
        permissionReply = reply
        permission.launch(name)
        return reply.await()
    }

    /** 弹系统的身份验证（生物识别或锁屏密码），过了交回解开的 [cipher]；取消、失败时为空。 */
    private suspend fun authenticate(cipher: Cipher, reason: UiText): Cipher? = suspendCancellableCoroutine { continuation ->
        val cancel = CancellationSignal()
        continuation.invokeOnCancellation { cancel.cancel() }
        val prompt = BiometricPrompt.Builder(this)
            .setTitle(reason.resolve(resources))
            .setAllowedAuthenticators(Authenticators.BIOMETRIC_STRONG or Authenticators.DEVICE_CREDENTIAL)
            .build()
        prompt.authenticate(
            BiometricPrompt.CryptoObject(cipher),
            cancel,
            mainExecutor,
            object : BiometricPrompt.AuthenticationCallback() {
                override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                    if (continuation.isActive) continuation.resume(result.cryptoObject?.cipher)
                }

                override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
                    if (continuation.isActive) continuation.resume(null)
                }
            },
        )
    }
}
