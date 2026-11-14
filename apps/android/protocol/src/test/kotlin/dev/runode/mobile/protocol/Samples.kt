package dev.runode.mobile.protocol

import java.io.File
import java.math.BigDecimal
import kotlinx.serialization.DeserializationStrategy
import kotlinx.serialization.SerializationStrategy
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject

/**
 * 宿主那边 `runode_protocol` 的样例，和 iOS 的测试读同一批文件。仓库根由构建经 `runode.repo` 传进来；
 * 找不到样例时测试失败，不跳过。
 */
object Samples {
    val repository: File = File(
        System.getProperty("runode.repo") ?: error("runode.repo is not set; run the tests through Gradle"),
    )
    val fixtures: File = repository.resolve("crates/protocol/tests/fixtures")

    fun file(dir: String, name: String): File = fixtures.resolve(dir).resolve(name).also {
        check(it.isFile) { "no sample $it" }
    }

    fun text(dir: String, name: String): String = file(dir, name).readText()

    fun json(dir: String, name: String): JsonElement = Json.parseToJsonElement(text(dir, name))

    /** 十六进制写的画面载荷。 */
    fun hex(dir: String, name: String): ByteArray {
        val text = text(dir, name).trim()
        check(text.length % 2 == 0) { "$name has an odd number of hex digits" }
        return ByteArray(text.length / 2) { text.substring(2 * it, 2 * it + 2).toInt(16).toByte() }
    }

    /** `messages.json` 的整份内容：`messages` 按名字存着各条消息，`agents` 是各种 agent 的显示名和转圈。 */
    val messagesRoot: JsonObject by lazy { Json.parseToJsonElement(fixtures.resolve("messages.json").readText()).jsonObject }

    fun message(name: String): JsonElement =
        messagesRoot.getValue("messages").jsonObject[name] ?: error("no sample $name")
}

fun <T> encodeToJson(serializer: SerializationStrategy<T>, value: T): JsonElement =
    Json.parseToJsonElement(protocolJson.encodeToString(serializer, value))

fun <T> decode(deserializer: DeserializationStrategy<T>, json: JsonElement): T =
    protocolJson.decodeFromString(deserializer, json.toString())

fun <T> decode(deserializer: DeserializationStrategy<T>, json: String): T =
    protocolJson.decodeFromString(deserializer, json)

/** 两段 JSON 解出来的值相等：键的顺序、空白、数字的写法（`1` 和 `1.0`）不算，`null` 和缺省算不同。 */
fun sameJson(a: JsonElement, b: JsonElement): Boolean = when {
    a is JsonObject && b is JsonObject -> a.keys == b.keys && a.keys.all { sameJson(a.getValue(it), b.getValue(it)) }
    a is JsonArray && b is JsonArray -> a.size == b.size && a.indices.all { sameJson(a[it], b[it]) }
    a is JsonNull || b is JsonNull -> a is JsonNull && b is JsonNull
    a is JsonPrimitive && b is JsonPrimitive -> when {
        a.isString || b.isString -> a.isString == b.isString && a.content == b.content
        else -> {
            val left = a.content.toBigDecimalOrNull()
            val right = b.content.toBigDecimalOrNull()
            if (left != null && right != null) left.compareTo(right) == 0 else a.content == b.content
        }
    }
    else -> false
}

private fun String.toBigDecimalOrNull(): BigDecimal? = runCatching { BigDecimal(this) }.getOrNull()
