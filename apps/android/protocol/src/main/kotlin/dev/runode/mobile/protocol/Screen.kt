package dev.runode.mobile.protocol

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.put

/** 远程桌面这套消息和画面格式的版本，宿主的 `protocol::screen::SCREEN_VERSION`。 */
const val screenVersion: UInt = 2u

/** 远程桌面的修饰键。 */
@Serializable
data class ScreenMods(
    val shift: Boolean = false,
    val ctrl: Boolean = false,
    val alt: Boolean = false,
    /** Command 键。 */
    val cmd: Boolean = false,
) {
    val isEmpty: Boolean get() = this == ScreenMods()
}

@Serializable
enum class ScreenMouseButton {
    @SerialName("left") Left,
    @SerialName("right") Right,
    @SerialName("middle") Middle,
}

/** 交给电脑的一次输入。坐标是画面上的比例，左上角 (0, 0)，右下角 (1, 1)。 */
@Serializable(with = ScreenInputSerializer::class)
sealed class ScreenInput {
    /** 指针挪到这里；按着键时是拖动。 */
    data class MouseMove(val x: Double, val y: Double) : ScreenInput()

    data class MouseButton(
        val button: ScreenMouseButton,
        val down: Boolean,
        val x: Double,
        val y: Double,
        val mods: ScreenMods = ScreenMods(),
    ) : ScreenInput()

    /** 滚动的点数，和手指拖动的方向一致：正的是内容往下、往右走。 */
    data class Scroll(val dx: Int, val dy: Int) : ScreenInput()

    /** 按一下再松开。[key] 是电脑那边 `Key` 的名字（`ArrowUp`、`Enter`、`A`）。 */
    data class Key(val key: String, val mods: ScreenMods = ScreenMods()) : ScreenInput()

    /** 一段文字，不管键盘布局。 */
    data class Text(val text: String) : ScreenInput()
}

internal object ScreenInputSerializer : EncodeOnlySerializer<ScreenInput>("ScreenInput") {
    override fun toJson(value: ScreenInput): JsonElement = when (value) {
        is ScreenInput.MouseMove -> typed("mouse_move") {
            put("x", value.x)
            put("y", value.y)
        }
        is ScreenInput.MouseButton -> typed("mouse_button") {
            put("button", ScreenMouseButton.serializer(), value.button)
            put("down", value.down)
            put("x", value.x)
            put("y", value.y)
            put("mods", ScreenMods.serializer(), value.mods)
        }
        is ScreenInput.Scroll -> typed("scroll") {
            put("dx", value.dx)
            put("dy", value.dy)
        }
        is ScreenInput.Key -> typed("key") {
            put("key", value.key)
            put("mods", ScreenMods.serializer(), value.mods)
        }
        is ScreenInput.Text -> typed("text") { put("text", value.text) }
    }
}

/** 手机发给电脑的远程桌面消息。 */
@Serializable(with = ScreenClientMsgSerializer::class)
sealed class ScreenClientMsg {
    /** 过了门禁后的第一条，这条连接就接到远程桌面上。 */
    data class ScreenHello(val version: UInt) : ScreenClientMsg()

    /**
     * 开始采集；采集中再发时按新的参数重新开始（切屏也是）。[display] 是 [ScreenDisplay.id]，为空时是主屏；
     * [bitrate] 单位 bit/s。[codecs] 是这部手机解得了的编码，想要的排前面，为空时电脑只编 HEVC。
     */
    data class Start(
        val display: UInt?,
        val width: UInt,
        val height: UInt,
        val fps: UInt,
        val bitrate: UInt,
        val codecs: List<Codec> = emptyList(),
    ) : ScreenClientMsg()

    /** 下一帧要关键帧。 */
    data object Keyframe : ScreenClientMsg()

    data class Bitrate(val bitrate: UInt) : ScreenClientMsg()

    data class Input(val input: ScreenInput) : ScreenClientMsg()

    /** 电脑锁着屏时用登录密码解锁，电脑按美式键盘敲进密码框再回车；解没解开看随后的 [ScreenHostMsg.Lock]。 */
    data class Unlock(val password: String) : ScreenClientMsg()
}

internal object ScreenClientMsgSerializer : EncodeOnlySerializer<ScreenClientMsg>("ScreenClientMsg") {
    override fun toJson(value: ScreenClientMsg): JsonElement = when (value) {
        is ScreenClientMsg.ScreenHello -> typed("screen_hello") { putUInt("version", value.version) }
        is ScreenClientMsg.Start -> typed("start") {
            value.display?.let { putUInt("display", it) }
            putUInt("width", value.width)
            putUInt("height", value.height)
            putUInt("fps", value.fps)
            putUInt("bitrate", value.bitrate)
            if (value.codecs.isNotEmpty()) putStrings("codecs", value.codecs.map(Codec::wireName))
        }
        ScreenClientMsg.Keyframe -> typed("keyframe")
        is ScreenClientMsg.Bitrate -> typed("bitrate") { putUInt("bitrate", value.bitrate) }
        is ScreenClientMsg.Input -> typed("input") { put("input", ScreenInputSerializer.toJson(value.input)) }
        is ScreenClientMsg.Unlock -> typed("unlock") { put("password", value.password) }
    }
}

/** 被采集的那块屏幕。 */
@Serializable
data class ScreenDisplay(
    /** 电脑上这块屏幕的编号，插拔以后可能变。 */
    val id: UInt,
    /** 点数。 */
    val width: UInt,
    val height: UInt,
    /** 像素数。 */
    @SerialName("pixel_width") val pixelWidth: UInt,
    @SerialName("pixel_height") val pixelHeight: UInt,
)

/** 远程桌面停下的原因归到哪类。 */
enum class ScreenStopCause(val rawValue: String) {
    /** 电脑屏幕上没人登录。 */
    SignedOut("signed_out"),

    /** 屏幕上没有 Runode 所用的那个用户登录着的桌面（X11 或 Wayland）。 */
    NoDesktop("no_desktop"),

    /** 屏幕上登录着别的用户。 */
    OtherUser("other_user"),

    /** Mac 没给 Runode 屏幕录制权限。 */
    NoScreenCapture("no_screen_capture"),

    /** 屏幕共享的授权框被拒绝了，或者没人点。 */
    Declined("declined"),

    /** 建不出能编这个 app 解得了的格式的编码器，重连也看不了。 */
    NoEncoder("no_encoder"),
    ;

    companion object {
        /** 不认得的类为空。 */
        fun fromRaw(rawValue: String): ScreenStopCause? = entries.firstOrNull { it.rawValue == rawValue }
    }
}

/** 电脑发给手机的远程桌面消息。 */
@Serializable(with = ScreenHostMsgSerializer::class)
sealed class ScreenHostMsg {
    /**
     * 握手后的第一条。[displays] 是开着的各块屏幕，主屏排第一。授权不够时采集（[screenCapture]）或输入
     * （[postEvent]）办不了，要用户到电脑的系统设置里打开。
     */
    data class Ready(
        val displays: List<ScreenDisplay>,
        val screenCapture: Boolean,
        val postEvent: Boolean,
    ) : ScreenHostMsg()

    /** 电脑的版本和这个 app 不同，随后断开。 */
    data class Incompatible(val version: UInt) : ScreenHostMsg()

    /** 开始采集了，接下来第一帧是关键帧；[display] 是采的那块屏幕，之后的输入落在它上面。 */
    data class Started(val display: UInt, val width: UInt, val height: UInt) : ScreenHostMsg()

    /**
     * 采集停了或者开不起来。[reason] 是电脑给的原文；[cause] 是归了的类。电脑没给 `cause` 或者给了这个 app 不认得的类时
     * 照 `signed_out`、`no_encoder` 两个标志归类，都没有时为空。
     */
    data class Stopped(val reason: String, val cause: ScreenStopCause?) : ScreenHostMsg()

    /** 电脑锁没锁屏：[Ready] 之后马上报一次，之后变了再报。 */
    data class Lock(val locked: Boolean) : ScreenHostMsg()

    /**
     * 用户在电脑上打开或关掉了输入授权，[postEvent] 的含义和 [Ready] 里的一样。[screenCapture] 是 [Ready] 时的值，
     * 屏幕录制的授权要重新连接才认。
     */
    data class Permissions(val screenCapture: Boolean, val postEvent: Boolean) : ScreenHostMsg()

    /** [ScreenClientMsg.Unlock] 没敲成（有美式键盘打不出的字符），[reason] 给日志看。 */
    data class UnlockFailed(val reason: String) : ScreenHostMsg()

    /** 这个 app 不认识的消息，忽略。 */
    data object Unknown : ScreenHostMsg()
}

internal object ScreenHostMsgSerializer : DecodeOnlySerializer<ScreenHostMsg>("ScreenHostMsg") {
    override fun fromJson(element: JsonElement): ScreenHostMsg {
        val c = element.asObject("screen message")
        return when (c.type()) {
            "ready" -> ScreenHostMsg.Ready(
                displays = c.required("displays", ListSerializer(ScreenDisplay.serializer())),
                screenCapture = c.bool("screen_capture"),
                postEvent = c.bool("post_event"),
            )
            "incompatible" -> ScreenHostMsg.Incompatible(c.uint("version"))
            "started" -> ScreenHostMsg.Started(c.uint("display"), c.uint("width"), c.uint("height"))
            "stopped" -> {
                val cause = c.optional("cause", String.serializer())?.let(ScreenStopCause::fromRaw)
                val signedOut = c.optional("signed_out", Boolean.serializer()) ?: false
                val noEncoder = c.optional("no_encoder", Boolean.serializer()) ?: false
                ScreenHostMsg.Stopped(
                    reason = c.string("reason"),
                    cause = cause ?: when {
                        signedOut -> ScreenStopCause.SignedOut
                        noEncoder -> ScreenStopCause.NoEncoder
                        else -> null
                    },
                )
            }
            "lock" -> ScreenHostMsg.Lock(c.bool("locked"))
            "permissions" -> ScreenHostMsg.Permissions(c.bool("screen_capture"), c.bool("post_event"))
            "unlock_failed" -> ScreenHostMsg.UnlockFailed(c.string("reason"))
            else -> ScreenHostMsg.Unknown
        }
    }
}

/** 画面的编码，宿主的 `runode_types::screen::Codec`。 */
enum class Codec(val wireName: String) {
    Hevc("hevc"),

    /** 手机在 [ScreenClientMsg.Start] 里列了它，电脑才会用。 */
    H264("h264"),
}

/** 编好的一帧，[FrameKind.Video] 的载荷。 */
class VideoFrame(
    /** NAL 单元，每个前面是 4 字节的大端长度。 */
    val data: ByteArray,
    /** 解码器能从这一帧开始解。 */
    val keyframe: Boolean,
    /** 关键帧的参数集（HEVC 的 VPS、SPS、PPS，H.264 的 SPS、PPS），不带长度前缀；别的帧是空的。 */
    val parameterSets: List<ByteArray>,
    /** 采集时刻，微秒，只用来比先后。 */
    val ptsMicros: Long,
    val codec: Codec = Codec.Hevc,
) {
    override fun equals(other: Any?): Boolean =
        other is VideoFrame && codec == other.codec && keyframe == other.keyframe && ptsMicros == other.ptsMicros &&
            data.contentEquals(other.data) && parameterSets.size == other.parameterSets.size &&
            parameterSets.zip(other.parameterSets).all { (a, b) -> a.contentEquals(b) }

    override fun hashCode(): Int =
        listOf(codec, data.contentHashCode(), keyframe, parameterSets.map { it.contentHashCode() }, ptsMicros).hashCode()

    override fun toString(): String =
        "VideoFrame(codec=$codec, data=${data.size} bytes, keyframe=$keyframe, parameterSets=${parameterSets.size}, " +
            "ptsMicros=$ptsMicros)"

    companion object {
        /**
         * 解 [FrameKind.Video] 的载荷：`u8 标志（第 0 位是关键帧，第 1 位是 H.264）| i64 LE pts | u8 参数集个数 |
         * 每个参数集 u32 LE 长度和内容 | 帧的数据`。格式不对时为空。
         */
        operator fun invoke(payload: ByteArray): VideoFrame? {
            var at = 0
            fun take(count: Long): ByteArray? {
                if (count < 0 || payload.size - at < count) return null
                return payload.copyOfRange(at, at + count.toInt()).also { at += count.toInt() }
            }
            fun little(bytes: ByteArray): Long =
                bytes.foldIndexed(0L) { i, value, byte -> value or ((byte.toLong() and 0xFF) shl (8 * i)) }

            val flags = take(1)?.first()?.toInt() ?: return null
            val pts = take(8)?.let(::little) ?: return null
            val count = take(1)?.first()?.toInt()?.and(0xFF) ?: return null
            val sets = ArrayList<ByteArray>(count)
            repeat(count) {
                val length = take(4)?.let(::little) ?: return null
                sets += take(length) ?: return null
            }
            return VideoFrame(
                data = payload.copyOfRange(at, payload.size),
                keyframe = flags and 1 != 0,
                parameterSets = sets,
                ptsMicros = pts,
                codec = if (flags and 2 != 0) Codec.H264 else Codec.Hevc,
            )
        }
    }
}
