package dev.runode.mobile.protocol

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.builtins.nullable
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.put

/** 宿主协议的版本，宿主的 `PROTOCOL_VERSION`。宿主在 `Hello` 里比对，对不上回 [HostMsg.Incompatible]。 */
const val protocolVersion: UInt = 4u

/** 连上会话时要什么，宿主的 `AttachMode`。 */
@Serializable
enum class AttachMode {
    /** 同一个构建才解得了的快照；手机的构建和宿主不同，用不上。 */
    @SerialName("snapshot") Snapshot,

    /** 重画当前屏幕的 VT 序列，喂给一份新的 VT。 */
    @SerialName("vt_replay") VtReplay,

    /** 不要屏幕，只要 `Meta` 这些状态。 */
    @SerialName("meta_only") MetaOnly,
}

/** [ClientMsg.Open] 的新终端放在桌面 app 里的哪里，宿主的 `Placement`。 */
@Serializable
enum class Placement {
    /** 紧跟在旁边那个终端的标签后面的新标签。 */
    @SerialName("tab") Tab,

    /** 把旁边那个终端一分为二，新终端在右边。 */
    @SerialName("right") Right,
}

/** 前端能做什么，缺的项按不能。 */
@Serializable
data class Caps(
    val snapshot: Boolean,
    @SerialName("vt_replay") val vtReplay: Boolean,
    /** 宿主可以把 shell 集成报告的 PATH 和函数名从 `meta` 里拆出来、没变时不发。手机不读这些，说了能省流量。 */
    @SerialName("shell_report") val shellReport: Boolean,
)

/** 前端的种类。手机只会说自己是 `mobile`。 */
@Serializable
enum class ClientKind {
    @SerialName("mobile") Mobile,
}

/**
 * 推送 token 所属的 APNs 环境，宿主的 `push::ApnsEnv`：App Store、TestFlight 装的是 `production`，
 * Xcode 直接装的是 `development`。
 */
@Serializable
enum class ApnsEnv {
    @SerialName("production") Production,
    @SerialName("development") Development,
}

/**
 * 推送经哪家的服务送到手机，宿主的 `push::PushService`：iPhone 是 `apns`，Android 是 `fcm`。是 [Apns] 时不写出来，
 * 只认 APNs 的电脑和中转照旧读得懂。
 */
@Serializable
enum class PushService {
    @SerialName("apns") Apns,
    @SerialName("fcm") Fcm,
}

/**
 * 前端发给宿主的消息。只定义手机这个前端会发的几种；宿主对手机连接上的 `Shutdown`、交接、`UiReply`、
 * `SetOptions`、`SetTheme` 只回 `Error`，这里故意不定义它们，手机就发不出去。JSON 的样子和宿主的
 * `ClientMsg` 一致，可缺省的字段也照宿主序列化的样子写出 `null`。
 */
@Serializable(with = ClientMsgSerializer::class)
sealed class ClientMsg {
    /** 连上后的第一条消息。手机的 `client` 是 `mobile`，`caps` 只要 VT 重放。 */
    data class Hello(
        val protocol: UInt,
        val build: String,
        val client: ClientKind,
        val caps: Caps,
        val session: SessionId?,
        val device: String?,
    ) : ClientMsg()

    data object ListSessions : ClientMsg()

    /** 要电脑上的 app 里各个终端摆在哪，宿主转给桌面的界面，回 [HostMsg.Layout]；没有桌面的界面时回 `Error`。 */
    data class Layout(val req: UInt) : ClientMsg()

    /**
     * 请电脑上的 app 在窗口里开一个新终端，宿主转给桌面的界面，回 [HostMsg.Opened]；没有桌面窗口时回
     * `Error`。[near] 为空时放在最前面那个窗口当前的分屏旁边，[cwd] 为空时沿用旁边那个终端的目录，
     * [focus] 为假时不切过去。
     */
    data class Open(
        val req: UInt,
        val placement: Placement,
        val near: SessionId?,
        val cwd: String?,
        val focus: Boolean,
    ) : ClientMsg()

    /**
     * 请电脑上的 app 在最前面那个窗口里新建一个目录是 [dir]（绝对路径）的工作区，回 [HostMsg.Opened]，带着它的
     * 第一个终端；已经有这个目录的工作区时在它里面新开一个标签，回这个新终端。[focus] 为假时电脑上不切
     * 过去。[name] 是新工作区在侧栏里的名字，为空时按目录取；已经有这个目录的工作区时不改它的名字。
     */
    data class OpenWorkspace(
        val req: UInt,
        val dir: String,
        val focus: Boolean,
        val name: String? = null,
    ) : ClientMsg()

    /**
     * 把第 [window] 个窗口里的第 [workspace] 个工作区改名为 [name]，序号见 [WindowLayout]、[WorkspaceLayout]。
     * 宿主转给桌面的界面，办好回 [HostMsg.Done]，办不成回带编号的 `Error`。
     */
    data class RenameWorkspace(val req: UInt, val window: UInt, val workspace: UInt, val name: String) : ClientMsg()

    /** 列电脑上一个目录（绝对路径）里的子目录，为空时是家目录，宿主回 [HostMsg.Dirs]，出错时回 `Error`。 */
    data class ListDirs(val req: UInt, val path: String?) : ClientMsg()

    /**
     * 列电脑上一个目录（绝对路径）里能跑的命令（Makefile 的目标、package.json 的 scripts），宿主回
     * [HostMsg.Tasks]，出错时回 `Error`。
     */
    data class ListTasks(val req: UInt, val dir: String) : ClientMsg()

    /**
     * 在电脑上按 [path]（为空时按新起的 shell 会拿到的 PATH）找 [programs] 这几个命令（个数不超过宿主的
     * `protocol::MAX_FIND_COMMANDS`），宿主回 [HostMsg.Commands]。只发给在 `Welcome` 里说认得它的宿主。
     */
    data class FindCommands(val req: UInt, val path: String?, val programs: List<String>) : ClientMsg()

    /** 新开一个会话；宿主回 [HostMsg.Spawned]，开好的会话要另外 [Attach]。 */
    data class Spawn(
        val req: UInt,
        val size: GridSize,
        val cwd: String?,
        val integration: IntegrationMode,
        val start: Boolean,
    ) : ClientMsg()

    /** 连上会话。[size] 为空时不改会话的尺寸，也不算一次交互。 */
    data class Attach(val id: SessionId, val size: GridSize?, val mode: AttachMode) : ClientMsg()

    data class Detach(val id: SessionId) : ClientMsg()

    /** 视图的尺寸变了：宿主改好后在输出流里标出 [HostMsg.Resized]。 */
    data class Resize(val id: SessionId, val size: GridSize) : ClientMsg()

    /** 这个会话在前端被看着；[focused] 为真算一次交互，可能轮到这条连接决定尺寸。 */
    data class Focus(val id: SessionId, val focused: Boolean) : ClientMsg()

    data class Kill(val id: SessionId) : ClientMsg()

    /**
     * 读会话屏幕底部的文字：从最后一个有字的行往上 [lines] 行（含回滚历史），为空时是当前一屏。
     * [command] 为 n 时不看 [lines]，读倒数第 n 条命令（1 是最近一条）的输出，不含提示符，要 shell
     * 集成标出的提示符。宿主回 [HostMsg.ScreenText]，出错时回带着会话标识的 `Error`。不用连上会话。给了 [req] 时，
     * 回话和 `Error` 都带着它；不给时都不带，只能按会话认。
     */
    data class ReadScreen(val id: SessionId, val lines: UInt?, val command: UInt? = null, val req: UInt? = null) : ClientMsg()

    /**
     * 发控制键（`enter`、`esc`、`up`、`1` 这类写法），宿主按它那份 VT 当前的模式编码后写进去，回
     * [HostMsg.Done]。不用连上会话。
     */
    data class SendKeys(val req: UInt, val id: SessionId, val keys: List<String>) : ClientMsg()

    /** 粘贴一段文字，程序开着括号粘贴时宿主套上括号，回 [HostMsg.Done]。不用连上会话。 */
    data class Paste(val req: UInt, val id: SessionId, val text: String) : ClientMsg()

    /**
     * 在会话 shell 当前所在的仓库里读写 git，回 [HostMsg.GitStatus]、[HostMsg.GitDiff] 或 [HostMsg.GitBranches]；
     * 办不了时回只带 `req`、不带会话 `id` 的 `Error`。一条连接上的这些请求宿主按先后一件一件办。
     */
    data class Git(val req: UInt, val id: SessionId, val request: GitRequest) : ClientMsg()

    /**
     * 登记这台电脑推等回答通知用的凭据（[token] 为空时注销）：官方 App 交中转服务换来的句柄，别的交 APNs
     * 的 device token（十六进制）。回 [HostMsg.Done]，办不了时回带编号的 `Error`。[bundle] 是 App 的 bundle id，
     * [machine] 是给这台电脑编的 UUID、[machineName] 是显示的电脑名，推送时原样带回。由电脑上的远程访问
     * 记进配对设备表，不到宿主。[service] 是推送经哪家送到手机，Android 的官方 App 是 [PushService.Fcm]、[env] 填
     * [ApnsEnv.Production]。
     */
    data class PushRegister(
        val req: UInt,
        val token: String?,
        val env: ApnsEnv,
        val bundle: String,
        val machine: String,
        val machineName: String,
        val service: PushService = PushService.Apns,
    ) : ClientMsg()

    /** 不再连这台电脑：电脑上的远程访问把这部手机从配对设备表里撤掉（推送登记一起），回 [HostMsg.Done]，随后断开。不到宿主。 */
    data class Unpair(val req: UInt) : ClientMsg()

    /**
     * 看着终端时每隔一小会儿发一条，宿主不回：一直有包来回，手机的 Wi-Fi 就不省电，打字不卡。只发给在
     * `Welcome` 里说认得它的宿主。
     */
    data object KeepAwake : ClientMsg()
}

internal object ClientMsgSerializer : EncodeOnlySerializer<ClientMsg>("ClientMsg") {
    override fun toJson(value: ClientMsg): JsonElement = when (value) {
        is ClientMsg.Hello -> typed("hello") {
            putUInt("protocol", value.protocol)
            put("build", value.build)
            put("client", ClientKind.serializer(), value.client)
            put("caps", Caps.serializer(), value.caps)
            put("session", SessionIdSerializer.nullable, value.session)
            put("device", value.device)
        }
        ClientMsg.ListSessions -> typed("list_sessions")
        is ClientMsg.Layout -> typed("layout") { putUInt("req", value.req) }
        is ClientMsg.Open -> typed("open") {
            putUInt("req", value.req)
            put("placement", Placement.serializer(), value.placement)
            put("near", SessionIdSerializer.nullable, value.near)
            put("cwd", value.cwd)
            put("focus", value.focus)
        }
        is ClientMsg.OpenWorkspace -> typed("open_workspace") {
            putUInt("req", value.req)
            put("dir", value.dir)
            put("focus", value.focus)
            put("name", value.name)
        }
        is ClientMsg.RenameWorkspace -> typed("rename_workspace") {
            putUInt("req", value.req)
            putUInt("window", value.window)
            putUInt("workspace", value.workspace)
            put("name", value.name)
        }
        is ClientMsg.ListDirs -> typed("list_dirs") {
            putUInt("req", value.req)
            put("path", value.path)
        }
        is ClientMsg.ListTasks -> typed("list_tasks") {
            putUInt("req", value.req)
            put("dir", value.dir)
        }
        is ClientMsg.FindCommands -> typed("find_commands") {
            putUInt("req", value.req)
            put("path", value.path)
            putStrings("programs", value.programs)
        }
        is ClientMsg.Spawn -> typed("spawn") {
            putUInt("req", value.req)
            put("size", GridSize.serializer(), value.size)
            put("cwd", value.cwd)
            put("integration", IntegrationModeSerializer, value.integration)
            put("start", value.start)
            put("shell", JsonNull)
            put("settings", JsonNull)
        }
        is ClientMsg.Attach -> typed("attach") {
            put("id", SessionIdSerializer, value.id)
            put("size", GridSize.serializer().nullable, value.size)
            put("mode", AttachMode.serializer(), value.mode)
        }
        is ClientMsg.Detach -> typed("detach") { put("id", SessionIdSerializer, value.id) }
        is ClientMsg.Resize -> typed("resize") {
            put("id", SessionIdSerializer, value.id)
            put("size", GridSize.serializer(), value.size)
        }
        is ClientMsg.Focus -> typed("focus") {
            put("id", SessionIdSerializer, value.id)
            put("focused", value.focused)
        }
        is ClientMsg.Kill -> typed("kill") { put("id", SessionIdSerializer, value.id) }
        is ClientMsg.ReadScreen -> typed("read_screen") {
            value.req?.let { putUInt("req", it) }
            put("id", SessionIdSerializer, value.id)
            putUInt("lines", value.lines)
            putUInt("command", value.command)
        }
        is ClientMsg.SendKeys -> typed("send_keys") {
            putUInt("req", value.req)
            put("id", SessionIdSerializer, value.id)
            putStrings("keys", value.keys)
        }
        is ClientMsg.Paste -> typed("paste") {
            putUInt("req", value.req)
            put("id", SessionIdSerializer, value.id)
            put("text", value.text)
        }
        is ClientMsg.Git -> typed("git") {
            putUInt("req", value.req)
            put("id", SessionIdSerializer, value.id)
            put("request", GitRequestSerializer.toJson(value.request))
        }
        is ClientMsg.PushRegister -> typed("push_register") {
            putUInt("req", value.req)
            put("token", value.token)
            put("env", ApnsEnv.serializer(), value.env)
            put("bundle", value.bundle)
            put("machine", value.machine)
            put("machine_name", value.machineName)
            if (value.service != PushService.Apns) put("service", PushService.serializer(), value.service)
        }
        is ClientMsg.Unpair -> typed("unpair") { putUInt("req", value.req) }
        ClientMsg.KeepAwake -> typed("keep_awake")
    }
}

/** [HostMsg.SessionList] 里的一个会话。 */
@Serializable
data class SessionInfo(
    val id: SessionId,
    val size: GridSize,
    val meta: SessionMeta,
    /** 现在连着它的前端有几个。 */
    val clients: UInt = 0u,
    /** 有桌面的界面连着它。 */
    val claimed: Boolean = false,
    /** shell 已经退出，会话还留着。 */
    val exited: Boolean = false,
    /** 现在决定这个会话尺寸的前端的设备名；没有 owner 或者它没报设备名时为空。 */
    @SerialName("size_owner") val sizeOwner: String? = null,
)

/** 宿主为什么断开。比自己新的宿主才有的原因读成 [Unknown]。 */
@Serializable(with = GoodbyeReasonSerializer::class)
sealed class GoodbyeReason {
    data object Shutdown : GoodbyeReason()

    /** 交接给了新版本的宿主，重新连上就是新宿主。 */
    data object Handoff : GoodbyeReason()

    data class Error(val message: String) : GoodbyeReason()

    data class Unknown(val kind: String) : GoodbyeReason()
}

internal object GoodbyeReasonSerializer : DecodeOnlySerializer<GoodbyeReason>("GoodbyeReason") {
    override fun fromJson(element: JsonElement): GoodbyeReason {
        val c = element.asObject("goodbye reason")
        return when (val kind = c.string("kind")) {
            "shutdown" -> GoodbyeReason.Shutdown
            "handoff" -> GoodbyeReason.Handoff
            "error" -> GoodbyeReason.Error(c.optional("message", String.serializer()) ?: "")
            else -> GoodbyeReason.Unknown(kind)
        }
    }
}

/** shell 集成报告的东西，手机只读 shell 自己的 PATH。宿主在 PATH 不是合法 UTF-8 时写成一串字节，那时为空。 */
@Serializable
data class ShellReport(
    @Serializable(with = LenientString::class) val path: String? = null,
)

/**
 * [HostMsg.Error] 的 `reason`：宿主标出的几种原因（`not_found`、`not_a_directory`、`permission_denied`、
 * `not_absolute`），界面按 [kind] 换成界面语言的说法；认不出的种类或者没有 [path] 时照旧显示宿主写的 `message`。
 */
@Serializable
data class HostErrorReason(
    val kind: String,
    val path: String? = null,
)


/**
 * 宿主发给前端的消息，和宿主的 `HostMsg` 一一对应。不认识的种类读成 [Unknown]（宿主的 `#[serde(other)]`），
 * 认识的种类缺了必填字段时整条解析失败。手机用不上的几种（界面转来的请求、交接、跑完的命令）只解出编号，
 * 内容不读；[Welcome] 只读 `keep_awake` 和 `shells`。
 */
@Serializable(with = HostMsgSerializer::class)
sealed class HostMsg {
    /** [keepAwake]：宿主认得 [ClientMsg.KeepAwake]；[findsCommands]：宿主认得 [ClientMsg.FindCommands]。 */
    data class Welcome(val keepAwake: Boolean, val findsCommands: Boolean) : HostMsg()

    data class Incompatible(val protocol: UInt, val build: String, val reason: String) : HostMsg()

    data class SessionList(val sessions: List<SessionInfo>) : HostMsg()

    data class Spawned(val req: UInt, val id: SessionId) : HostMsg()

    /** 回 [ClientMsg.Attach]。之后先是快照帧和 [SnapshotEnd]（[AttachMode.MetaOnly] 时没有），再是输出。 */
    data class Attached(
        val id: SessionId,
        /** 这个会话的帧在这条连接上用的通道。 */
        val channel: UInt,
        val size: GridSize,
        /** 宿主实际给的屏幕。 */
        val mode: AttachMode,
        val meta: SessionMeta,
        /** 宿主那份 VT 现在套着的主题；VT 重放时按它和 [size] 新建自己的 VT。 */
        val settings: TermSettings?,
        /** shell 集成报告的东西的第一份，之后变了在 [Meta] 里带。 */
        val shell: ShellReport? = null,
    ) : HostMsg()

    data class SnapshotEnd(val id: SessionId) : HostMsg()

    data class Resized(val id: SessionId, val size: GridSize) : HostMsg()

    data class ThemeApplied(val id: SessionId, val settings: TermSettings) : HostMsg()

    /** [shell] 是 shell 集成报告的东西变了时带的新一份，为空是没变。 */
    data class Meta(val id: SessionId, val meta: SessionMeta, val shell: ShellReport? = null) : HostMsg()

    data class CommandFinished(val id: SessionId) : HostMsg()

    data class Resync(val id: SessionId, val reason: String) : HostMsg()

    data class Exited(val id: SessionId, val status: Int?) : HostMsg()

    data class Bell(val id: SessionId) : HostMsg()

    data class Opened(val req: UInt, val id: SessionId) : HostMsg()

    data class Done(val req: UInt) : HostMsg()

    /** 读到的屏幕文字；[req] 是回的那条 [ClientMsg.ReadScreen] 的 `req`。 */
    data class ScreenText(val id: SessionId, val text: String, val truncated: Boolean, val req: UInt? = null) : HostMsg()

    /** 回 [ClientMsg.Layout]。 */
    data class Layout(val req: UInt, val windows: List<WindowLayout>) : HostMsg()

    /** 电脑上 app 的布局变了，想知道新的布局再发 [ClientMsg.Layout]。只发给这条连接上问过布局的。 */
    data object LayoutChanged : HostMsg()

    /**
     * 回 [ClientMsg.ListDirs]：实际列的目录（规范化后的绝对路径）和它的子目录名，按名字排好；[truncated] 为真时
     * 子目录太多，只给了前面一部分。
     */
    data class Dirs(val req: UInt, val path: String, val dirs: List<String>, val truncated: Boolean) : HostMsg()

    /**
     * 回 [ClientMsg.ListTasks]：请求里的目录和找到的 Makefile、package.json 里的命令，先 Makefile 后
     * package.json，一样都没找到时为空。
     */
    data class Tasks(val req: UInt, val dir: String, val sources: List<TaskSource>) : HostMsg()

    /** 回 [ClientMsg.FindCommands]：和请求里的 `programs` 一一对应，找到的是命令的文件名（Windows 上带着扩展名），找不到的为空。 */
    data class Commands(val req: UInt, val found: List<String?>) : HostMsg()

    /** 回 [ClientMsg.Git] 里读状态和改仓库的操作：办完以后仓库的样子；会话的目录不在 git 仓库里时为空。 */
    data class GitStatus(
        val req: UInt,
        val id: SessionId,
        val status: dev.runode.mobile.protocol.GitStatus?,
    ) : HostMsg()

    /** 回 [GitRequest.Diff]；这个文件在那一段里已经没有改动时为空。 */
    data class GitDiff(val req: UInt, val id: SessionId, val diff: GitFileDiff?) : HostMsg()

    /** 回 [GitRequest.Branches]：本地分支在前，远端分支在后，各按最近一次提交的时间倒序。 */
    data class GitBranches(val req: UInt, val id: SessionId, val branches: List<GitBranch>) : HostMsg()

    data class UiRequest(val ui: ULong) : HostMsg()

    /** [message] 是宿主写的原文；宿主标了原因时 [reason] 不为空，界面按它给出界面语言的说法。 */
    data class Error(
        val req: UInt?,
        val id: SessionId?,
        val message: String,
        val reason: HostErrorReason? = null,
    ) : HostMsg()

    data class Goodbye(val reason: GoodbyeReason) : HostMsg()

    data object HandoffRefused : HostMsg()

    data class HandoffBegin(val format: UInt, val sessions: UInt) : HostMsg()

    data class SizeOwner(val id: SessionId, val mine: Boolean, val owner: String?) : HostMsg()

    /** 比自己新的宿主才有的消息，忽略。 */
    data class Unknown(val type: String) : HostMsg()

    /** 这条消息说的是哪个会话；不针对某个会话的消息为空。 */
    val sessionId: SessionId?
        get() = when (this) {
            is Attached -> id
            is SnapshotEnd -> id
            is Resized -> id
            is ThemeApplied -> id
            is Meta -> id
            is CommandFinished -> id
            is Resync -> id
            is Exited -> id
            is Bell -> id
            is ScreenText -> id
            is SizeOwner -> id
            is Spawned -> id
            is Opened -> id
            is Error -> id
            else -> null
        }

    companion object {
        /**
         * [Error.message] 以它开头：这条连接在宿主那边积压着，请求没办、会话还在，过一会儿再来。宿主的
         * `protocol::ERROR_BACKED_UP`，是认错误种类的约定。
         */
        const val backedUpPrefix: String = "the connection is backed up"

        /** [Error.message] 以它开头：会话已经没了。宿主的 `protocol::ERROR_NO_SESSION`。 */
        const val noSessionPrefix: String = "no session"
    }
}

internal object HostMsgSerializer : DecodeOnlySerializer<HostMsg>("HostMsg") {
    override fun fromJson(element: JsonElement): HostMsg {
        val c = element.asObject("host message")
        fun id() = c.required("id", SessionIdSerializer)
        fun req() = c.uint("req")
        return when (val type = c.type()) {
            "welcome" -> HostMsg.Welcome(
                keepAwake = c.optional("keep_awake", Boolean.serializer()) ?: false,
                findsCommands = c.optional("shells", Boolean.serializer()) ?: false,
            )
            "incompatible" -> HostMsg.Incompatible(c.uint("protocol"), c.string("build"), c.string("reason"))
            "session_list" -> HostMsg.SessionList(c.required("sessions", ListSerializer(SessionInfo.serializer())))
            "spawned" -> HostMsg.Spawned(req(), id())
            "attached" -> HostMsg.Attached(
                id = id(),
                channel = c.uint("channel"),
                size = c.required("size", GridSize.serializer()),
                mode = c.required("mode", AttachMode.serializer()),
                meta = c.required("meta", SessionMeta.serializer()),
                settings = c.optional("settings", TermSettingsSerializer),
                shell = c.optional("shell", ShellReport.serializer()),
            )
            "snapshot_end" -> HostMsg.SnapshotEnd(id())
            "resized" -> HostMsg.Resized(id(), c.required("size", GridSize.serializer()))
            "theme_applied" -> HostMsg.ThemeApplied(id(), c.required("settings", TermSettingsSerializer))
            "meta" -> HostMsg.Meta(
                id(),
                c.required("meta", SessionMeta.serializer()),
                c.optional("shell", ShellReport.serializer()),
            )
            "command_finished" -> HostMsg.CommandFinished(id())
            "resync" -> HostMsg.Resync(id(), c.string("reason"))
            "exited" -> HostMsg.Exited(id(), c.optional("status", Int.serializer()))
            "bell" -> HostMsg.Bell(id())
            "opened" -> HostMsg.Opened(req(), id())
            "done" -> HostMsg.Done(req())
            "screen_text" -> HostMsg.ScreenText(
                id(),
                c.string("text"),
                c.optional("truncated", Boolean.serializer()) ?: false,
                c.optional("req", UInt.serializer()),
            )
            "layout" -> HostMsg.Layout(req(), c.required("windows", ListSerializer(WindowLayout.serializer())))
            "layout_changed" -> HostMsg.LayoutChanged
            "dirs" -> HostMsg.Dirs(
                req(),
                c.string("path"),
                c.required("dirs", ListSerializer(String.serializer())),
                c.optional("truncated", Boolean.serializer()) ?: false,
            )
            "tasks" -> HostMsg.Tasks(
                req(),
                c.string("dir"),
                c.required("sources", ListSerializer(TaskSource.serializer())),
            )
            "commands" -> HostMsg.Commands(req(), c.required("found", ListSerializer(String.serializer().nullable)))
            "git_status" -> HostMsg.GitStatus(req(), id(), c.optional("status", GitStatus.serializer()))
            "git_diff" -> HostMsg.GitDiff(req(), id(), c.optional("diff", GitFileDiff.serializer()))
            "git_branches" -> HostMsg.GitBranches(
                req(),
                id(),
                c.required("branches", ListSerializer(GitBranch.serializer())),
            )
            "ui_request" -> HostMsg.UiRequest(c.required("ui", ULong.serializer()))
            "error" -> HostMsg.Error(
                req = c.optional("req", UInt.serializer()),
                id = c.optional("id", SessionIdSerializer),
                message = c.string("message"),
                reason = c.lenient("reason", HostErrorReason.serializer()),
            )
            "goodbye" -> HostMsg.Goodbye(c.required("reason", GoodbyeReasonSerializer))
            "handoff_refused" -> HostMsg.HandoffRefused
            "handoff_begin" -> HostMsg.HandoffBegin(c.uint("format"), c.uint("sessions"))
            "size_owner" -> HostMsg.SizeOwner(
                id(),
                c.bool("mine"),
                c.optional("owner", String.serializer()),
            )
            else -> HostMsg.Unknown(type)
        }
    }
}
