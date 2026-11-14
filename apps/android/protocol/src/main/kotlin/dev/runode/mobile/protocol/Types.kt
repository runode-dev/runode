package dev.runode.mobile.protocol

import kotlinx.serialization.KSerializer
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.descriptors.PrimitiveKind
import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject

// 宿主那边 `runode_types` 里各端共用的数据，JSON 的样子照 serde 的默认写法：
// 结构体是对象，元组结构体（`Rgb`）是数组，`rename_all = "snake_case"` 的枚举是字符串，
// 带数据的枚举（`TerminalColor::Rgb`、`IntegrationMode::Force`）是 `{"变体": 值}`。
// 读的时候比宿主宽松：缺了的字段按默认值读，不认识的取值不让整条消息失败，宿主加了字段或取值时
// 旧的 app 照样能用。

/** 一个终端会话的标识：32 个小写十六进制数字，宿主重启、交接后不变。 */
@Serializable(with = SessionIdSerializer::class)
class SessionId private constructor(val rawValue: String) {
    override fun equals(other: Any?): Boolean = other is SessionId && rawValue == other.rawValue

    override fun hashCode(): Int = rawValue.hashCode()

    override fun toString(): String = rawValue

    companion object {
        /** 写法不对（不是正好 32 个 ASCII 十六进制数字）时为空。 */
        operator fun invoke(text: String): SessionId? {
            if (text.length != 32 || !text.all { it in '0'..'9' || it in 'a'..'f' || it in 'A'..'F' }) return null
            return SessionId(text.lowercase())
        }
    }
}

internal object SessionIdSerializer : KSerializer<SessionId> {
    override val descriptor = PrimitiveSerialDescriptor("SessionId", PrimitiveKind.STRING)

    override fun serialize(encoder: Encoder, value: SessionId) = encoder.encodeString(value.rawValue)

    override fun deserialize(decoder: Decoder): SessionId =
        SessionId(decoder.decodeString()) ?: throw SerializationException("a session id is 32 hexadecimal digits")
}

/** 网格的行列数和单元格的像素尺寸。 */
@Serializable
data class GridSize(
    val cols: UShort,
    val rows: UShort,
    @SerialName("cell_width_px") val cellWidthPx: UShort,
    @SerialName("cell_height_px") val cellHeightPx: UShort,
)

/** 一个 RGB 颜色，JSON 里是 `[r, g, b]`。 */
@Serializable(with = RgbSerializer::class)
data class Rgb(val r: UByte, val g: UByte, val b: UByte) {
    /** `0xRRGGBB`。 */
    constructor(hex: UInt) : this((hex shr 16).toUByte(), (hex shr 8).toUByte(), hex.toUByte())
}

internal object RgbSerializer : JsonElementSerializer<Rgb>("Rgb") {
    override fun toJson(value: Rgb): JsonElement =
        JsonArray(listOf(value.r, value.g, value.b).map { JsonPrimitive(it.toInt()) })

    override fun fromJson(element: JsonElement): Rgb {
        val items = element.asArray("rgb")
        if (items.size < 3) throw SerializationException("an rgb color has three components")
        fun component(i: Int) = protocolJson.decodeFromJsonElement(UByte.serializer(), items[i])
        return Rgb(component(0), component(1), component(2))
    }
}

/** 光标和选区的颜色：固定色，或者跟随所在单元格的前景、背景色。 */
@Serializable(with = TerminalColorSerializer::class)
sealed class TerminalColor {
    data class Rgb(val color: dev.runode.mobile.protocol.Rgb) : TerminalColor()

    data object CellForeground : TerminalColor()

    data object CellBackground : TerminalColor()
}

internal object TerminalColorSerializer : JsonElementSerializer<TerminalColor>("TerminalColor") {
    override fun toJson(value: TerminalColor): JsonElement = when (value) {
        is TerminalColor.Rgb -> buildJsonObject { put("rgb", RgbSerializer.toJson(value.color)) }
        TerminalColor.CellForeground -> JsonPrimitive("cell_foreground")
        TerminalColor.CellBackground -> JsonPrimitive("cell_background")
    }

    override fun fromJson(element: JsonElement): TerminalColor {
        if (element is JsonPrimitive && element.isString) {
            return when (element.content) {
                "cell_foreground" -> TerminalColor.CellForeground
                "cell_background" -> TerminalColor.CellBackground
                else -> throw SerializationException("unknown terminal color ${element.content}")
            }
        }
        return TerminalColor.Rgb(element.asObject("terminal color").required("rgb", RgbSerializer))
    }
}

/** 配置的光标样式。 */
@Serializable
enum class CursorStyle {
    @SerialName("block") Block,
    @SerialName("block_hollow") BlockHollow,
    @SerialName("bar") Bar,
    @SerialName("underline") Underline,
}

/** macOS 上 Option 键当不当 Alt 用；手机的硬件键盘上 Option 键照同样的设置处理。 */
@Serializable
enum class OptionAsAlt {
    /** 没配置：桌面按键盘布局定，手机上读不到硬件键盘的布局，当作不当 Alt。 */
    @SerialName("auto") Auto,
    @SerialName("false") No,
    @SerialName("true") Yes,
    @SerialName("left") Left,
    @SerialName("right") Right,
}

/**
 * 宿主 VT 套着的主题，[HostMsg.Attached]、[HostMsg.ThemeApplied] 里带来。和宿主的 `TermSettings`
 * 字段一一对应；编码时每个字段都写（宿主那边这些字段都是必填的），读的时候缺的、读不了的按 [default] 里的。
 */
@Serializable(with = TermSettingsSerializer::class)
data class TermSettings(
    val background: Rgb,
    val foreground: Rgb,
    /** 覆盖默认 256 色中的若干项。 */
    val palette: List<PaletteEntry>,
    val cursorStyle: CursorStyle,
    /** 为空表示默认闪烁。 */
    val cursorBlink: Boolean?,
    /** 为空表示用前景色。 */
    val cursorColor: TerminalColor?,
    /** 实心块状光标下文字的颜色，为空表示用背景色。 */
    val cursorText: TerminalColor?,
    val selectionBackground: TerminalColor?,
    val selectionForeground: TerminalColor?,
    val searchBackground: TerminalColor,
    val searchForeground: TerminalColor,
    val searchSelectedBackground: TerminalColor,
    val searchSelectedForeground: TerminalColor,
    val optionAsAlt: OptionAsAlt,
    /** 回滚历史最多占多少字节。 */
    val scrollbackLimit: ULong,
    /** 宿主给 shell 集成打开哪些功能；手机不用，只为编解码和宿主一致。 */
    val shellFeatures: ShellFeatures = ShellFeatures(),
) {
    /** 宿主 `ShellFeatures` 的线上样子；缺的项按打开读。 */
    @Serializable
    data class ShellFeatures(
        /** 提示符上把光标换成竖线。 */
        val cursor: Boolean = true,
    )

    /** 调色板里的一项，JSON 里是 `[下标, [r, g, b]]`。 */
    @Serializable(with = PaletteEntrySerializer::class)
    data class PaletteEntry(val index: UByte, val color: Rgb)

    companion object {
        /** 宿主没配置时的设置，宿主的 `TermSettings::default()`。 */
        val default: TermSettings = TermSettings(
            background = Rgb(0x171618u),
            foreground = Rgb(0xE6E1D8u),
            palette = listOf(
                0x393A3Du, 0xFF1261u, 0x2AD947u, 0xFCBA28u, 0x2D9AFFu, 0xDD30FFu, 0x17D5DFu, 0xE7E7E7u,
                0x6B6B6Bu, 0xC55555u, 0xAAC474u, 0xFECA88u, 0x82B8C8u, 0xC28CB8u, 0x93D3C3u, 0xF8F8F8u,
            ).mapIndexed { index, hex -> PaletteEntry(index.toUByte(), Rgb(hex)) },
            cursorStyle = CursorStyle.Block,
            cursorBlink = null,
            cursorColor = null,
            cursorText = null,
            selectionBackground = null,
            selectionForeground = null,
            searchBackground = TerminalColor.Rgb(Rgb(0xFFE795u)),
            searchForeground = TerminalColor.Rgb(Rgb(0x000000u)),
            searchSelectedBackground = TerminalColor.Rgb(Rgb(0xF2A57Eu)),
            searchSelectedForeground = TerminalColor.Rgb(Rgb(0x000000u)),
            optionAsAlt = OptionAsAlt.Auto,
            scrollbackLimit = 10uL * 1024uL * 1024uL,
        )
    }
}

internal object PaletteEntrySerializer : JsonElementSerializer<TermSettings.PaletteEntry>("PaletteEntry") {
    override fun toJson(value: TermSettings.PaletteEntry): JsonElement =
        JsonArray(listOf(JsonPrimitive(value.index.toInt()), RgbSerializer.toJson(value.color)))

    override fun fromJson(element: JsonElement): TermSettings.PaletteEntry {
        val items = element.asArray("palette entry")
        if (items.size < 2) throw SerializationException("a palette entry is [index, [r, g, b]]")
        return TermSettings.PaletteEntry(
            protocolJson.decodeFromJsonElement(UByte.serializer(), items[0]),
            RgbSerializer.fromJson(items[1]),
        )
    }
}

internal object TermSettingsSerializer : JsonElementSerializer<TermSettings>("TermSettings") {
    override fun toJson(value: TermSettings): JsonElement = buildJsonObject {
        put("background", RgbSerializer.toJson(value.background))
        put("foreground", RgbSerializer.toJson(value.foreground))
        put("palette", JsonArray(value.palette.map(PaletteEntrySerializer::toJson)))
        put("cursor_style", CursorStyle.serializer(), value.cursorStyle)
        put("cursor_blink", JsonPrimitive(value.cursorBlink))
        put("cursor_color", color(value.cursorColor))
        put("cursor_text", color(value.cursorText))
        put("selection_background", color(value.selectionBackground))
        put("selection_foreground", color(value.selectionForeground))
        put("search_background", color(value.searchBackground))
        put("search_foreground", color(value.searchForeground))
        put("search_selected_background", color(value.searchSelectedBackground))
        put("search_selected_foreground", color(value.searchSelectedForeground))
        put("option_as_alt", OptionAsAlt.serializer(), value.optionAsAlt)
        put("scrollback_limit", JsonPrimitive(value.scrollbackLimit.toLong()))
        put("shell_features", TermSettings.ShellFeatures.serializer(), value.shellFeatures)
    }

    private fun color(value: TerminalColor?): JsonElement = value?.let(TerminalColorSerializer::toJson) ?: JsonNull

    override fun fromJson(element: JsonElement): TermSettings {
        val c = element.asObject("settings")
        val fallback = TermSettings.default
        return TermSettings(
            background = c.optional("background", RgbSerializer) ?: fallback.background,
            foreground = c.optional("foreground", RgbSerializer) ?: fallback.foreground,
            palette = c.optional("palette", ListSerializer(PaletteEntrySerializer)) ?: fallback.palette,
            cursorStyle = c.lenient("cursor_style", CursorStyle.serializer()) ?: fallback.cursorStyle,
            cursorBlink = c.optional("cursor_blink", Boolean.serializer()),
            cursorColor = c.lenient("cursor_color", TerminalColorSerializer),
            cursorText = c.lenient("cursor_text", TerminalColorSerializer),
            selectionBackground = c.lenient("selection_background", TerminalColorSerializer),
            selectionForeground = c.lenient("selection_foreground", TerminalColorSerializer),
            searchBackground = c.lenient("search_background", TerminalColorSerializer) ?: fallback.searchBackground,
            searchForeground = c.lenient("search_foreground", TerminalColorSerializer) ?: fallback.searchForeground,
            searchSelectedBackground = c.lenient("search_selected_background", TerminalColorSerializer)
                ?: fallback.searchSelectedBackground,
            searchSelectedForeground = c.lenient("search_selected_foreground", TerminalColorSerializer)
                ?: fallback.searchSelectedForeground,
            optionAsAlt = c.lenient("option_as_alt", OptionAsAlt.serializer()) ?: fallback.optionAsAlt,
            scrollbackLimit = c.optional("scrollback_limit", ULong.serializer()) ?: fallback.scrollbackLimit,
            shellFeatures = c.lenient("shell_features", TermSettings.ShellFeatures.serializer())
                ?: fallback.shellFeatures,
        )
    }
}

/** agent 当前的状态。宿主新加的状态读成 [Unknown]，界面当作没有 agent 状态处理。 */
@Serializable(with = AgentStateSerializer::class)
sealed class AgentState(val wireName: String) {
    data object Working : AgentState("working")

    data object Idle : AgentState("idle")

    /** 停下来等用户回答。 */
    data object Blocked : AgentState("blocked")

    data class Unknown(val text: String) : AgentState(text)
}

internal object AgentStateSerializer : WireNameSerializer<AgentState>(
    "AgentState",
    known = listOf(AgentState.Working, AgentState.Idle, AgentState.Blocked),
    wireName = AgentState::wireName,
    unknown = AgentState::Unknown,
)

/**
 * 是哪个 agent：宿主 `AgentKind` 的短名（`claude`、`codex` 等）。按字符串存，宿主新认得的 agent
 * 不让消息解析失败。
 */
@Serializable(with = AgentKindSerializer::class)
data class AgentKind(val label: String) {
    /** 工作中的转圈：各帧和每帧的秒数。 */
    data class Spinner(val frames: List<String>, val frameTime: Double)

    /** 给人看的名字，宿主的 `AgentKind::display_name`；不认识的原样显示。 */
    val displayName: String get() = displayNames[label] ?: label

    /**
     * 工作中的转圈，宿主的 `AgentKind::spinner`：跟各 agent 自己界面里的工作动画一样，没有专门动画的用盲文
     * 点阵转圈。
     */
    val spinner: Spinner
        get() = when (label) {
            "claude" -> Spinner(listOf("·", "✢", "✳", "✶", "✻", "✽", "✽", "✻", "✶", "✳", "✢", "·"), 0.12)
            "codex" -> Spinner(listOf("•", "◦"), 0.6)
            else -> Spinner(listOf("⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"), 0.08)
        }

    /** 转圈的颜色，宿主的 `AgentKind::spinner_color`；为空时沿用文字颜色。 */
    val spinnerColor: Rgb? get() = if (label == "claude") Rgb(0xD7u, 0x77u, 0x57u) else null
}

private val displayNames = mapOf(
    "pi" to "Pi", "claude" to "Claude Code", "codex" to "Codex", "gemini" to "Gemini CLI",
    "cursor" to "Cursor Agent", "devin" to "Devin", "antigravity" to "Antigravity", "cline" to "Cline",
    "omp" to "Oh My Pi", "mastracode" to "Mastra Code", "open_code" to "OpenCode",
    "github_copilot" to "GitHub Copilot", "kimi" to "Kimi Code", "kiro" to "Kiro", "droid" to "Droid",
    "amp" to "Amp", "grok" to "Grok", "hermes" to "Hermes", "kilo" to "Kilo Code", "qodercli" to "Qoder CLI",
    "qwen" to "Qwen Code", "letta" to "Letta Code", "maki" to "Maki", "muse" to "Muse",
    "aider" to "Aider", "goose" to "Goose", "crush" to "Crush", "auggie" to "Auggie",
    "continue_cli" to "Continue", "junie" to "Junie", "open_hands" to "OpenHands", "trae" to "Trae Agent",
    "code_buddy" to "CodeBuddy Code", "iflow" to "iFlow CLI", "codebuff" to "Codebuff",
    "mistral_vibe" to "Mistral Vibe", "jules" to "Jules", "plandex" to "Plandex",
    "other" to "Agent",
)

internal object AgentKindSerializer : KSerializer<AgentKind> {
    override val descriptor = PrimitiveSerialDescriptor("AgentKind", PrimitiveKind.STRING)

    override fun serialize(encoder: Encoder, value: AgentKind) = encoder.encodeString(value.label)

    override fun deserialize(decoder: Decoder): AgentKind = AgentKind(decoder.decodeString())
}

/** 前台 agent 和它的状态。宿主给 agent 加的字段在这里加，一律带默认值。 */
@Serializable
data class Agent(
    val kind: AgentKind,
    val state: AgentState,
)

internal object LenientAgent : LenientSerializer<Agent>(Agent.serializer())

/** 一个终端会话对外公布的状态。只读界面用得到的字段，其余（shell 报告的名字、PATH 等）忽略；缺的字段按默认值读。 */
@Serializable
data class SessionMeta(
    val title: String? = null,
    @SerialName("fallback_title") val fallbackTitle: String? = null,
    @Serializable(with = LenientAgent::class) val agent: Agent? = null,
    /** 宿主的 `PathBuf` 不是合法 UTF-8 时序列化不出来，能读到的都是字符串；读不了的按没有。 */
    @Serializable(with = LenientString::class) val cwd: String? = null,
    @SerialName("foreground_is_shell") val foregroundIsShell: Boolean = false,
    val foreground: String? = null,
    /** 前台 agent 进入现在这个种类和状态的时刻，宿主系统时钟的 Unix 毫秒；前台没有 agent 时为空。 */
    @SerialName("agent_since_ms") val agentSinceMs: Long? = null,
) {
    /**
     * 标签上显示的名字：shell 在前台时是 shell 的程序名（目录另有一行，shell 设的标题多半是缩写的路径），
     * 否则是程序设置的标题，没有时用前台程序名或目录名。
     */
    val displayTitle: String?
        get() {
            if (foregroundIsShell) {
                val program = foreground?.let(::programLabel)
                if (!program.isNullOrEmpty()) return program
            }
            if (!title.isNullOrEmpty()) return title
            if (!fallbackTitle.isNullOrEmpty()) return fallbackTitle
            return null
        }
}

/** 界面上写的程序名：进程名去掉 Windows 可执行文件的 `.exe`。 */
internal fun programLabel(name: String): String {
    if (name.length <= 4 || !name.lowercase().endsWith(".exe")) return name
    return name.dropLast(4)
}

/** shell 的种类。 */
@Serializable
enum class Shell {
    @SerialName("zsh") Zsh,
    @SerialName("bash") Bash,
    @SerialName("fish") Fish,
}

/** shell 集成怎么注入，JSON 里是 `"detect"`、`"off"` 或 `{"force": "zsh"}`。 */
@Serializable(with = IntegrationModeSerializer::class)
sealed class IntegrationMode {
    data object Detect : IntegrationMode()

    data object Off : IntegrationMode()

    data class Force(val shell: Shell) : IntegrationMode()
}

internal object IntegrationModeSerializer : JsonElementSerializer<IntegrationMode>("IntegrationMode") {
    override fun toJson(value: IntegrationMode): JsonElement = when (value) {
        IntegrationMode.Detect -> JsonPrimitive("detect")
        IntegrationMode.Off -> JsonPrimitive("off")
        is IntegrationMode.Force -> buildJsonObject { put("force", Shell.serializer(), value.shell) }
    }

    override fun fromJson(element: JsonElement): IntegrationMode {
        if (element is JsonPrimitive && element.isString) {
            return when (element.content) {
                "detect" -> IntegrationMode.Detect
                "off" -> IntegrationMode.Off
                else -> throw SerializationException("unknown integration mode ${element.content}")
            }
        }
        return IntegrationMode.Force(element.asObject("integration mode").required("force", Shell.serializer()))
    }
}
