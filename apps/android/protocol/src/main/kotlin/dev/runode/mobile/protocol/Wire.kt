package dev.runode.mobile.protocol

import kotlinx.serialization.DeserializationStrategy
import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.KSerializer
import kotlinx.serialization.SerializationException
import kotlinx.serialization.SerializationStrategy
import kotlinx.serialization.builtins.nullable
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.descriptors.PrimitiveKind
import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor
import kotlinx.serialization.descriptors.SerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonDecoder
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonEncoder
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonObjectBuilder
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject

/**
 * 编解协议消息用的 JSON 设置：读的时候不认识的字段忽略，有默认值的字段遇到 `null` 或不认识的枚举值按默认值读；
 * 写的时候默认值和 `null` 都照写，和宿主 serde 写出来的一样。
 */
val protocolJson: Json = Json {
    ignoreUnknownKeys = true
    coerceInputValues = true
    encodeDefaults = true
    explicitNulls = true
}

/** 只用于 [protocolJson] 的序列化器：先整段读成 [JsonElement] 再转换，写法和 serde 手写的一样直接。 */
internal abstract class JsonElementSerializer<T>(serialName: String) : KSerializer<T> {
    override val descriptor: SerialDescriptor = PrimitiveSerialDescriptor(serialName, PrimitiveKind.STRING)

    abstract fun toJson(value: T): JsonElement

    abstract fun fromJson(element: JsonElement): T

    override fun serialize(encoder: Encoder, value: T) {
        val json = encoder as? JsonEncoder ?: throw SerializationException("${descriptor.serialName} only encodes to JSON")
        json.encodeJsonElement(toJson(value))
    }

    override fun deserialize(decoder: Decoder): T {
        val json = decoder as? JsonDecoder ?: throw SerializationException("${descriptor.serialName} only decodes JSON")
        return fromJson(json.decodeJsonElement())
    }
}

/** 只解不编的消息（宿主发来的）。 */
internal abstract class DecodeOnlySerializer<T>(serialName: String) : JsonElementSerializer<T>(serialName) {
    override fun toJson(value: T): JsonElement =
        throw SerializationException("${descriptor.serialName} is only sent by the host")
}

/** 只编不解的消息（手机发出去的）。 */
internal abstract class EncodeOnlySerializer<T>(serialName: String) : JsonElementSerializer<T>(serialName) {
    override fun fromJson(element: JsonElement): T =
        throw SerializationException("${descriptor.serialName} is only sent by the phone")
}

/**
 * 线上是一个字符串、取值可能比这边认得的多的类型：[known] 里按 [wireName] 认，认不出的交给 [unknown]，
 * 写的时候原样写回。
 */
internal open class WireNameSerializer<T>(
    serialName: String,
    private val known: List<T>,
    private val wireName: (T) -> String,
    private val unknown: (String) -> T,
) : KSerializer<T> {
    override val descriptor: SerialDescriptor = PrimitiveSerialDescriptor(serialName, PrimitiveKind.STRING)

    fun fromWire(text: String): T = known.firstOrNull { wireName(it) == text } ?: unknown(text)

    override fun serialize(encoder: Encoder, value: T) = encoder.encodeString(wireName(value))

    override fun deserialize(decoder: Decoder): T = fromWire(decoder.decodeString())
}

/** 读坏了不让整条消息失败的可选字段：读不出来按没有。 */
internal open class LenientSerializer<T : Any>(private val inner: KSerializer<T>) : KSerializer<T?> {
    override val descriptor: SerialDescriptor = inner.nullable.descriptor

    @OptIn(ExperimentalSerializationApi::class)
    override fun serialize(encoder: Encoder, value: T?) {
        if (value == null) encoder.encodeNull() else encoder.encodeSerializableValue(inner, value)
    }

    override fun deserialize(decoder: Decoder): T? {
        val json = decoder as? JsonDecoder ?: throw SerializationException("${descriptor.serialName} only decodes JSON")
        val element = json.decodeJsonElement()
        if (element is JsonNull) return null
        return runCatching { json.json.decodeFromJsonElement(inner, element) }.getOrNull()
    }
}

internal object LenientString : LenientSerializer<String>(String.serializer())

internal fun JsonElement.asObject(what: String): JsonObject =
    this as? JsonObject ?: throw SerializationException("$what is not a JSON object")

internal fun JsonElement.asArray(what: String): JsonArray =
    this as? JsonArray ?: throw SerializationException("$what is not a JSON array")

/** 必填的字段：缺了或者是 `null` 时整条读不了。 */
internal fun <T> JsonObject.required(key: String, deserializer: DeserializationStrategy<T>): T {
    val element = this[key]?.takeUnless { it is JsonNull } ?: throw SerializationException("missing field $key")
    return protocolJson.decodeFromJsonElement(deserializer, element)
}

/** 可缺的字段：缺了或者是 `null` 时为空，有值但读不了时整条读不了。 */
internal fun <T : Any> JsonObject.optional(key: String, deserializer: DeserializationStrategy<T>): T? {
    val element = this[key]?.takeUnless { it is JsonNull } ?: return null
    return protocolJson.decodeFromJsonElement(deserializer, element)
}

/** 可缺的字段，有值但读不了时也按没有。 */
internal fun <T : Any> JsonObject.lenient(key: String, deserializer: DeserializationStrategy<T>): T? =
    runCatching { optional(key, deserializer) }.getOrNull()

internal fun JsonObject.string(key: String): String = required(key, String.serializer())

internal fun JsonObject.uint(key: String): UInt = required(key, UInt.serializer())

internal fun JsonObject.bool(key: String): Boolean = required(key, Boolean.serializer())

internal fun JsonObject.type(): String = string("type")

internal fun <T> JsonObjectBuilder.put(key: String, serializer: SerializationStrategy<T>, value: T) {
    put(key, protocolJson.encodeToJsonElement(serializer, value))
}

internal fun JsonObjectBuilder.putUInt(key: String, value: UInt?) {
    put(key, value?.let { JsonPrimitive(it.toLong()) } ?: JsonNull)
}

internal fun JsonObjectBuilder.putStrings(key: String, values: List<String>) {
    put(key, JsonArray(values.map(::JsonPrimitive)))
}

internal fun typed(type: String, fields: JsonObjectBuilder.() -> Unit = {}): JsonObject = buildJsonObject {
    put("type", JsonPrimitive(type))
    fields()
}

/** RFC 3986 的非保留字符（字母、数字、`-._~`）原样，其余按 UTF-8 的每个字节写成大写的 `%XX`。 */
internal fun percentEncode(text: String): String = buildString {
    for (byte in text.encodeToByteArray()) {
        val c = byte.toInt() and 0xFF
        val ch = c.toChar()
        if (c < 0x80 && (ch.isLetterOrDigit() || ch in "-._~")) append(ch) else append("%%%02X".format(c))
    }
}

/** 解百分号编码，`+` 不当空格；`%` 后面不是两位十六进制数字、或者解出来不是合法 UTF-8 时为空。 */
internal fun percentDecode(text: String): String? {
    if ('%' !in text) return text
    val bytes = java.io.ByteArrayOutputStream(text.length)
    var i = 0
    while (i < text.length) {
        val ch = text[i]
        if (ch == '%') {
            val hex = text.substring(i + 1, (i + 3).coerceAtMost(text.length))
            if (hex.length != 2 || !hex.all { it in '0'..'9' || it in 'a'..'f' || it in 'A'..'F' }) return null
            bytes.write(hex.toInt(16))
            i += 3
        } else {
            val next = text.indexOf('%', i).let { if (it < 0) text.length else it }
            bytes.write(text.substring(i, next).encodeToByteArray())
            i = next
        }
    }
    return runCatching { bytes.toByteArray().decodeToString(throwOnInvalidSequence = true) }.getOrNull()
}

/**
 * 认 `<scheme>://<host>?<query>` 这种链接：scheme 和 host 不分大小写地对上时，按出现的先后返回查询参数（名字和值都解过百分号
 * 编码，没有 `=` 的值为空串）；对不上或者百分号编码坏了时为空。
 */
internal fun queryItems(text: String, scheme: String, host: String): List<Pair<String, String>>? {
    val schemeEnd = text.indexOf("://")
    if (schemeEnd < 0 || !text.substring(0, schemeEnd).equals(scheme, ignoreCase = true)) return null
    val rest = text.substring(schemeEnd + 3).substringBefore('#')
    val authorityEnd = rest.indexOfFirst { it == '/' || it == '?' }.let { if (it < 0) rest.length else it }
    if (!rest.substring(0, authorityEnd).equals(host, ignoreCase = true)) return null
    val query = rest.substringAfter('?', "")
    if (query.isEmpty()) return emptyList()
    return query.split('&').map { item ->
        val name = percentDecode(item.substringBefore('=')) ?: return null
        val value = percentDecode(item.substringAfter('=', "")) ?: return null
        name to value
    }
}
