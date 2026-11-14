package dev.runode.mobile.features.model

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import java.util.Locale
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/** 终端页以外的界面用哪套配色；终端页总按各自会话的主题上色。 */
@Serializable
enum class AppAppearance {
    /** 跟电脑上终端的主题走，按 [AppModel.computerTheme] 挑电脑。 */
    @SerialName("computer") Computer,

    /** 内置的浅色，[AppTheme.light]。 */
    @SerialName("light") Light,

    /** 内置的深色，[AppTheme.dark]。 */
    @SerialName("dark") Dark,

    /** 系统是深色模式时用 [AppTheme.dark]，否则用 [AppTheme.light]。 */
    @SerialName("system") System,
}

/** 界面用哪种语言；[tag] 是 BCP 47 的语言标签，跟随系统时为空。 */
@Serializable
enum class AppLanguage(val tag: String?) {
    @SerialName("system") System(null),
    @SerialName("en") English("en"),
    @SerialName("zh-Hans") SimplifiedChinese("zh-Hans"),
    @SerialName("zh-Hant") TraditionalChinese("zh-Hant"),
    ;

    companion object {
        /** 系统给本 App 定的语言（逗号分隔的标签）对应哪一项：只看第一个，不是英文和中文时算跟随系统。 */
        fun fromLanguageTags(tags: String): AppLanguage {
            val first = tags.substringBefore(',').trim()
            if (first.isEmpty()) return System
            val locale = Locale.forLanguageTag(first)
            return when (locale.language) {
                "en" -> English
                "zh" -> when (locale.script) {
                    "Hant" -> TraditionalChinese
                    "" -> if (locale.country in traditionalRegions) TraditionalChinese else SimplifiedChinese
                    else -> SimplifiedChinese
                }
                else -> System
            }
        }

        /** 不写文字系统时按繁体中文算的地区。 */
        private val traditionalRegions = setOf("TW", "HK", "MO")
    }
}

/** 设置页里能改的东西，整份存下来。 */
@Serializable
data class AppPreferences(
    /** 新打开的终端页用哪种尺寸方式；终端页上还能临时换。 */
    val defaultSize: SizePreference = SizePreference.Automatic,
    /** 终端的字号（sp）；为空时跟随系统的字体大小。 */
    val fontSize: Double? = null,
    /** 终端里的程序响铃时震一下。 */
    val bellHaptics: Boolean = true,
    /** 报给电脑的设备名；为空时用系统给的名字。 */
    val deviceName: String? = null,
    /** agent 停下来等回答时，由电脑经推送发通知提醒。关掉时向各台电脑注销推送。 */
    val alertsBlockedAgents: Boolean = true,
    val dictationSource: DictationSource = DictationSource.Phone,
    val appearance: AppAppearance = AppAppearance.Computer,
    val language: AppLanguage = AppLanguage.System,
) {
    companion object {
        /** 手动字号能调的范围。 */
        val fontSizes: ClosedFloatingPointRange<Double> = 9.0..28.0
    }
}

/**
 * 设置页的视图模型：改了马上存；设备名变了告诉 [AppModel]，由它转给各台电脑的连接；等回答提醒的开关变了也告诉它，
 * 由它向各台电脑登记或注销推送。
 */
class SettingsModel(
    private val store: DefaultsStore<AppPreferences>,
    /** 系统给的设备名，没自己起名字时用它。 */
    val systemDeviceName: String,
) {
    private var current by mutableStateOf(store.load() ?: AppPreferences())

    internal var deviceNameDidChange: (String) -> Unit = {}
    internal var alertsDidChange: (Boolean) -> Unit = {}

    var preferences: AppPreferences
        get() = current
        set(value) {
            val old = current
            if (value == old) return
            current = value
            store.save(value)
            if (value.deviceName != old.deviceName) deviceNameDidChange(deviceName)
            if (value.alertsBlockedAgents != old.alertsBlockedAgents) alertsDidChange(value.alertsBlockedAgents)
        }

    /** 报给电脑的设备名。 */
    val deviceName: String get() = preferences.deviceName ?: systemDeviceName

    /** 改设备名：去掉首尾空白，空的或者和系统名字一样就是不自己起名字。 */
    fun setDeviceName(name: String) {
        val trimmed = name.trim()
        preferences = preferences.copy(deviceName = trimmed.takeUnless { it.isEmpty() || it == systemDeviceName })
    }

    /** 手动字号；为空时跟随系统。超出范围的拉回范围里。 */
    fun setFontSize(size: Double?) {
        preferences = preferences.copy(
            fontSize = size?.let { Math.round(it).toDouble().coerceIn(AppPreferences.fontSizes) },
        )
    }
}
