package dev.runode.mobile

import android.app.Application
import android.content.Context
import android.os.Build
import android.provider.Settings
import dev.runode.mobile.connection.AndroidKeyStoreDeviceKeys
import dev.runode.mobile.connection.ClientIdentity
import dev.runode.mobile.connection.FileMachineStore
import dev.runode.mobile.connection.HostConnection
import dev.runode.mobile.connection.KeystoreUnlockPasswords
import dev.runode.mobile.connection.NativeIroh
import dev.runode.mobile.connection.NetworkChanges
import dev.runode.mobile.connection.NsdDiscovery
import dev.runode.mobile.connection.RemotePairing
import dev.runode.mobile.connection.ScreenConnection
import dev.runode.mobile.connection.SpeechConnection
import dev.runode.mobile.features.model.AppDependencies
import dev.runode.mobile.features.model.AppModel
import dev.runode.mobile.features.model.AppPreferences
import dev.runode.mobile.features.model.DefaultsStore
import dev.runode.mobile.features.model.SharedPreferencesDefaults
import java.io.File
import kotlinx.coroutines.MainScope
import kotlinx.coroutines.launch

/**
 * 进程里只有一份 [AppModel]，第一个 Activity 要它时才建（[appModel]）：调试构建带着演示参数启动时建的是演示用的（[DemoComposition]），
 * 不碰真的电脑。推送的通知出不出也问它。
 */
class RunodeApplication : Application() {
    private var model: AppModel? = null

    /** 建好的那份的依赖；演示模式时为空。Activity 往里装要它才能做的事（验证身份）。 */
    var dependencies: AppDependencies? = null
        private set

    override fun onCreate() {
        super.onCreate()
        NativeIroh.context = applicationContext
    }

    /** 进程里的那份 [AppModel]；还没建时按 [demo]（演示参数，为空时是正常启动）建。 */
    fun appModel(demo: String?): AppModel {
        model?.let { return it }
        val made = demo?.let { DemoComposition.make(this, it) } ?: real()
        model = made
        AndroidPushNotifications.shared(this)?.shows = made::showsNotification
        MainScope().launch { made.machineList.load() }
        return made
    }

    private fun real(): AppModel {
        val context: Context = applicationContext
        val keyStore = AndroidKeyStoreDeviceKeys()
        val store = FileMachineStore(FileMachineStore.defaultFile(context))
        val discovery = NsdDiscovery(context)
        val network = NetworkChanges(context)
        val defaults = SharedPreferencesDefaults(getSharedPreferences(DEFAULTS, MODE_PRIVATE))
        val preferences = DefaultsStore<AppPreferences>("preferences", defaults)
        val systemName = Settings.Global.getString(contentResolver, Settings.Global.DEVICE_NAME) ?: Build.MODEL
        val dependencies = AppDependencies(
            store = store,
            keyStore = keyStore,
            pairing = RemotePairing(keyStore, discovery),
            makeLink = { machine ->
                HostConnection(
                    machine = machine,
                    keyStore = keyStore,
                    machines = store,
                    discovery = discovery,
                    identity = ClientIdentity(ClientIdentity.mobileBuild, preferences.load()?.deviceName ?: systemName),
                    networkChange = network::await,
                )
            },
            deviceName = systemName,
            recents = DefaultsStore("recentTerminal", defaults),
            preferences = preferences,
            themes = DefaultsStore("theme", defaults),
        )
        dependencies.installPush(context)
        // 连接过程中电脑的地址、iroh 公钥会更新，按存着的最新记录连。
        dependencies.openScreen = { machine ->
            ScreenConnection.open(store.all().firstOrNull { it.id == machine.id } ?: machine, keyStore, discovery)
        }
        dependencies.openSpeech = { machine ->
            SpeechConnection.open(store.all().firstOrNull { it.id == machine.id } ?: machine, keyStore, discovery)
        }
        dependencies.unlockPasswords = KeystoreUnlockPasswords(context)
        dependencies.speechModels = File(noBackupFilesDir, SPEECH_MODELS)
        dependencies.appContext = context
        this.dependencies = dependencies
        return AppModel(dependencies)
    }

    private companion object {
        const val DEFAULTS = "dev.runode.mobile.defaults"
        const val SPEECH_MODELS = "speech-models"
    }
}
