package dev.runode.mobile.features.model

import dev.runode.mobile.features.R
import dev.runode.mobile.protocol.ClientMsg
import java.io.File
import java.util.Locale
import javax.xml.parsers.DocumentBuilderFactory
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.rules.TestWatcher
import org.junit.runner.Description
import org.w3c.dom.Element

/** 把 `Dispatchers.Main` 换成测试的调度器：视图模型的协程都开在它上面，`runTest` 也跟着用它的时钟。 */
@OptIn(ExperimentalCoroutinesApi::class)
class MainDispatcherRule(val dispatcher: TestDispatcher = StandardTestDispatcher()) : TestWatcher() {
    override fun starting(description: Description) {
        Dispatchers.setMain(dispatcher)
    }

    override fun finished(description: Description) {
        Dispatchers.resetMain()
    }
}

/** 发出去的消息里的 `Attach`。 */
fun attaches(sent: List<ClientMsg>): List<ClientMsg.Attach> = sent.filterIsInstance<ClientMsg.Attach>()

/**
 * 按资源目录 [directory]（`values`、`values-zh` 这样）里的 strings.xml 把 [UiText] 拼成那种语言的字，测试据此比对给用户看的
 * 字。JVM 上没有 `Resources`，这里自己读资源文件、按 `R` 里的编号找名字。单复数只分 `one` 和 `other`，中文只有 `other`。
 */
class ResourceStrings(directory: String) {
    private val strings = mutableMapOf<String, String>()
    private val plurals = mutableMapOf<String, Map<String, String>>()

    init {
        val document = DocumentBuilderFactory.newInstance().newDocumentBuilder()
            .parse(File("src/main/res/$directory/strings.xml"))
        val stringNodes = document.getElementsByTagName("string")
        for (i in 0 until stringNodes.length) {
            val node = stringNodes.item(i) as Element
            strings[node.getAttribute("name")] = unescape(node.textContent)
        }
        val pluralNodes = document.getElementsByTagName("plurals")
        for (i in 0 until pluralNodes.length) {
            val node = pluralNodes.item(i) as Element
            val items = node.getElementsByTagName("item")
            plurals[node.getAttribute("name")] = (0 until items.length).associate {
                val item = items.item(it) as Element
                item.getAttribute("quantity") to unescape(item.textContent)
            }
        }
    }

    private fun unescape(text: String): String = text.replace("\\'", "'").replace("\\\"", "\"")

    private fun name(type: Class<*>, id: Int): String =
        type.fields.first { it.getInt(null) == id }.name

    fun render(text: UiText): String = when (text) {
        is UiText.Raw -> text.text
        is UiText.Res -> String.format(Locale.US, strings.getValue(name(R.string::class.java, text.id)), *args(text.args))
        is UiText.Plural -> {
            val forms = plurals.getValue(name(R.plurals::class.java, text.id))
            val form = if (text.count == 1) forms["one"] ?: forms.getValue("other") else forms.getValue("other")
            String.format(Locale.US, form, *args(text.args))
        }
    }

    private fun args(args: List<Any>): Array<Any> = args.map { if (it is UiText) render(it) else it }.toTypedArray()
}

/** 默认的英文。 */
val English = ResourceStrings("values")

/** 简体中文。 */
val SimplifiedChinese = ResourceStrings("values-zh")

/** 拼成英文的字；为空时为空。 */
val UiText?.english: String? get() = this?.let(English::render)

/** 拼成简体中文的字；为空时为空。 */
val UiText?.simplifiedChinese: String? get() = this?.let(SimplifiedChinese::render)
