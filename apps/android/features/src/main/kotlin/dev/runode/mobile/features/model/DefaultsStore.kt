package dev.runode.mobile.features.model

import android.content.SharedPreferences
import java.util.UUID
import kotlinx.serialization.KSerializer
import kotlinx.serialization.descriptors.PrimitiveKind
import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder
import kotlinx.serialization.json.Json
import kotlinx.serialization.serializer

/** 按键存字符串的地方：App 里是 [SharedPreferencesDefaults]，测试和演示模式用 [MemoryDefaults]。 */
interface Defaults {
    fun string(key: String): String?

    fun setString(key: String, value: String?)
}

/** 存在 SharedPreferences 里。 */
class SharedPreferencesDefaults(private val preferences: SharedPreferences) : Defaults {
    override fun string(key: String): String? = preferences.getString(key, null)

    override fun setString(key: String, value: String?) {
        preferences.edit().apply { if (value == null) remove(key) else putString(key, value) }.apply()
    }
}

/** 只记在内存里，进程结束就没了。 */
class MemoryDefaults : Defaults {
    private val values = mutableMapOf<String, String>()

    override fun string(key: String): String? = synchronized(values) { values[key] }

    override fun setString(key: String, value: String?) {
        synchronized(values) { if (value == null) values.remove(key) else values[key] = value }
    }
}

/**
 * 存成 JSON 记在 [Defaults] 里的一份值：上次打开的终端、设置、上次用的主题各一份，里面都没有秘密。读不出来（没存过、
 * 格式变了）时为空；缺了的字段按默认值读，多出来的字段不管。
 */
class DefaultsStore<Value>(
    private val key: String,
    private val serializer: KSerializer<Value>,
    private val defaults: Defaults = MemoryDefaults(),
) {
    fun load(): Value? = defaults.string(key)?.let { runCatching { json.decodeFromString(serializer, it) }.getOrNull() }

    fun save(value: Value) {
        defaults.setString(key, runCatching { json.encodeToString(serializer, value) }.getOrNull())
    }

    companion object {
        private val json = Json {
            ignoreUnknownKeys = true
            coerceInputValues = true
        }

        inline operator fun <reified Value> invoke(key: String, defaults: Defaults = MemoryDefaults()) =
            DefaultsStore(key, serializer<Value>(), defaults)
    }
}

/** UUID 存成小写的带横线写法。 */
internal object UuidSerializer : KSerializer<UUID> {
    override val descriptor = PrimitiveSerialDescriptor("dev.runode.mobile.features.UUID", PrimitiveKind.STRING)

    override fun serialize(encoder: Encoder, value: UUID) = encoder.encodeString(value.toString())

    override fun deserialize(decoder: Decoder): UUID = UUID.fromString(decoder.decodeString())
}
