package dev.runode.mobile

import android.content.Context
import dev.runode.mobile.connection.FrameTransport
import dev.runode.mobile.connection.HostDiscovery
import dev.runode.mobile.connection.HostEvent
import dev.runode.mobile.connection.HostLink
import dev.runode.mobile.connection.LinkFailure
import dev.runode.mobile.connection.LinkState
import dev.runode.mobile.connection.MachineRecord
import dev.runode.mobile.connection.MemoryDeviceKeyStore
import dev.runode.mobile.connection.MemoryMachineStore
import dev.runode.mobile.connection.Pairing
import dev.runode.mobile.connection.ScreenConnection
import dev.runode.mobile.connection.TransportTarget
import dev.runode.mobile.features.model.AppDependencies
import dev.runode.mobile.features.model.AppModel
import dev.runode.mobile.features.model.DefaultsStore
import dev.runode.mobile.features.model.Route
import dev.runode.mobile.features.model.SharedPreferencesDefaults
import dev.runode.mobile.protocol.Agent
import dev.runode.mobile.protocol.AgentKind
import dev.runode.mobile.protocol.AgentState
import dev.runode.mobile.protocol.AttachMode
import dev.runode.mobile.protocol.CertificateFingerprint
import dev.runode.mobile.protocol.ClientMsg
import dev.runode.mobile.protocol.Frame
import dev.runode.mobile.protocol.FrameDecoder
import dev.runode.mobile.protocol.FrameKind
import dev.runode.mobile.protocol.GateMessage
import dev.runode.mobile.protocol.GitBranch
import dev.runode.mobile.protocol.GitFile
import dev.runode.mobile.protocol.GitFileDiff
import dev.runode.mobile.protocol.GitFileStatus
import dev.runode.mobile.protocol.GitHunk
import dev.runode.mobile.protocol.GitLine
import dev.runode.mobile.protocol.GitLineKind
import dev.runode.mobile.protocol.GitRequest
import dev.runode.mobile.protocol.GitStatus
import dev.runode.mobile.protocol.GridSize
import dev.runode.mobile.protocol.HostMsg
import dev.runode.mobile.protocol.PairingInvitation
import dev.runode.mobile.protocol.PaneLayout
import dev.runode.mobile.protocol.PaneRect
import dev.runode.mobile.protocol.Rgb
import dev.runode.mobile.protocol.SessionId
import dev.runode.mobile.protocol.SessionInfo
import dev.runode.mobile.protocol.SessionMeta
import dev.runode.mobile.protocol.TabLayout
import dev.runode.mobile.protocol.TermSettings
import dev.runode.mobile.protocol.WindowLayout
import dev.runode.mobile.protocol.WorkspaceLayout
import dev.runode.mobile.protocol.gateVersion
import java.time.Instant
import java.util.Base64
import java.util.Locale
import java.util.UUID
import kotlin.time.Duration
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.MainScope
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.channels.SendChannel
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.buffer
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.launch

/**
 * 只在调试构建里有的演示模式：MainActivity 的 Intent 带着字符串 [EXTRA] 时不连真的电脑，换成本地的假连接：一台列出三个
 * 会话（等你回答的 Claude、在干活的 Codex、普通的 zsh），另一台一直连不上，用来在模拟器里看界面、截图。
 *
 * [EXTRA] 的值是逗号分开的几个词：`terminal` 直接打开等回答的那个会话的终端页，`shell` 打开普通 shell 的，`list` 打开会话
 * 列表，`git` 打开普通 shell 所在仓库的 Git 页，`picker` 打开新建工作区时选目录的页，`screen` 打开远程桌面（只有黑屏，没有
 * 画面），`settings` 打开设置页，`pair` 打开配对页，`paired` 打开带着一条演示配对链接的配对页（点「配对」就配好），都不带时
 * 停在首页；再带上 `offline` 时连上一会儿后假装断线（远程桌面报电脑缺屏幕录制的授权），带上 `locked` 时远程桌面报电脑锁着屏，
 * 带上 `light` 时终端用浅色主题。演示的 [AppModel] 在进程里第一次要时按那时的参数建，换参数要先结束进程。
 */
internal object DemoComposition {
    const val EXTRA: String = "demo"

    private val machine = MachineRecord(
        id = UUID.fromString("00000000-0000-0000-0000-00000000de70"),
        name = demoMachineName,
        hostName = demoMachineName,
        fingerprint = CertificateFingerprint(ByteArray(32) { 7 })!!,
        port = 7866u,
        addresses = listOf("127.0.0.1"),
        lastAddress = "192.168.1.20",
        deviceId = "00000000000000000000000000000000",
    )

    /** 连不上的那台。 */
    private val offlineMachine = MachineRecord(
        id = UUID.fromString("00000000-0000-0000-0000-00000000de71"),
        name = "homelab",
        hostName = "homelab",
        fingerprint = CertificateFingerprint(ByteArray(32) { 8 })!!,
        port = 7866u,
        addresses = listOf("127.0.0.2"),
        lastAddress = null,
        deviceId = "00000000000000000000000000000001",
        pairedAt = Instant.now().plusSeconds(1),
    )

    fun make(context: Context, words: String): AppModel {
        val flags = words.split(',').map { it.trim() }.toSet()
        val store = MemoryMachineStore(listOf(machine, offlineMachine))
        // 上次打开的终端记在单独的一份设置里，和正式的分开；先带 `shell` 启动一次，再只带 `demo` 启动，首页就有「上次打开」。
        val defaults = SharedPreferencesDefaults(context.getSharedPreferences("demo", Context.MODE_PRIVATE))
        val dependencies = AppDependencies(
            store = store,
            keyStore = MemoryDeviceKeyStore(),
            pairing = DemoPairing,
            makeLink = { record -> if (record.id == offlineMachine.id) DemoOfflineLink() else DemoLink(flags) },
            deviceName = demoPhoneName,
            recents = DefaultsStore("recentTerminal", defaults),
        )
        val keys = MemoryDeviceKeyStore()
        dependencies.openScreen = { record ->
            // 等一会儿再连上，看得到「正在连接」。
            delay(1500)
            if (keys.key(record.id) == null) keys.generate(record.id)
            ScreenConnection.open(record, keys, DemoDiscovery, open = { _, _ -> DemoScreenTransport(record.deviceId, flags) }, tunnel = null)
        }
        return AppModel(dependencies)
    }

    fun open(app: AppModel, words: String) {
        val flags = words.split(',').map { it.trim() }.toSet()
        when {
            "terminal" in flags -> app.path = listOf(Route.Machine(machine.id), Route.Terminal(machine.id, DemoLink.claude.id))
            "shell" in flags -> app.path = listOf(Route.Machine(machine.id), Route.Terminal(machine.id, DemoLink.shell.id))
            "list" in flags -> app.path = listOf(Route.Machine(machine.id))
            "git" in flags -> app.path = listOf(Route.Machine(machine.id), Route.Git(machine.id, DemoLink.shell.id))
            "screen" in flags -> app.path = listOf(Route.Machine(machine.id), Route.Screen(machine.id))
            "picker" in flags -> {
                app.path = listOf(Route.Machine(machine.id))
                // 电脑上的窗口报上来以后才能新建工作区。
                MainScope().launch {
                    repeat(50) {
                        val list = app.sessionList(machine.id)
                        if (list != null && list.hasDesktopWindow) {
                            list.beginNewWorkspace()
                            return@launch
                        }
                        delay(100)
                    }
                }
            }
            "settings" in flags -> app.showingSettings = true
            "paired" in flags -> app.startPairing(invitationLink())
            "pair" in flags -> app.startPairing()
        }
    }

    /** 一条认得出的配对链接，指着一台演示用的电脑；[DemoPairing] 收到它就配好。 */
    private fun invitationLink(): String {
        val encoder = Base64.getUrlEncoder().withoutPadding()
        val fingerprint = encoder.encodeToString(ByteArray(32) { 9 })
        val secret = encoder.encodeToString(ByteArray(32) { 3 })
        val expires = Instant.now().plusSeconds(3600).epochSecond
        return "runode://pair?v=1&name=Studio&fp=$fingerprint&secret=$secret&port=7866&addr=192.168.1.30,fd00::30&exp=$expires"
    }

    /** 配对：稍等一下就配好，记录按邀请里的电脑填。 */
    private object DemoPairing : Pairing {
        override suspend fun pair(invitation: PairingInvitation, deviceName: String): MachineRecord {
            delay(800)
            return MachineRecord(
                name = invitation.hostName,
                hostName = invitation.hostName,
                fingerprint = invitation.fingerprint,
                port = invitation.port,
                addresses = invitation.addresses,
                lastAddress = null,
                deviceId = "00000000000000000000000000000002",
            )
        }
    }
}

/** 演示数据里显示出来的文字：系统语言是中文时用中文，否则用英文，截各语言的截图时不中英混杂。 */
private fun demoText(chinese: String, english: String): String =
    if (Locale.getDefault().language == "zh") chinese else english

private val demoMachineName = demoText("Ethan 的 MacBook Pro", "Ethan’s MacBook Pro")
private val demoPhoneName = demoText("演示手机", "Demo Phone")

/** 一直连不上的电脑：只报连接失败。 */
private class DemoOfflineLink : HostLink {
    private val failed = LinkState.Failed(LinkFailure.ConnectionFailed(demoText("找不到这台电脑", "Can’t find this computer")))
    override val state: StateFlow<LinkState> = MutableStateFlow(failed)

    override fun events(): Flow<HostEvent> = callbackFlow {
        trySend(HostEvent.State(failed))
        awaitClose {}
    }

    override fun send(message: ClientMsg) {}

    override fun sendInput(data: ByteArray, channel: UInt, generation: ULong) {}

    override suspend fun start() {}

    override suspend fun stop() {}

    override suspend fun reconnectNow() {}

    override fun setFocused(focused: Boolean) {}

    override suspend fun setDeviceName(name: String) {}

    override fun nextRequestId(): UInt = 0u
}

/** 局域网里找不到电脑，演示的远程桌面直接连 [DemoScreenTransport]。 */
private object DemoDiscovery : HostDiscovery {
    override suspend fun locate(fingerprint: CertificateFingerprint, timeout: Duration): TransportTarget? = null
}

/**
 * 假的远程桌面：过门禁（不验签名），报一块屏幕，收到 `start` 回 `started`，不发画面。带着 `offline` 时报电脑缺屏幕录制的
 * 授权、开始采集时停下，带着 `locked` 时报锁着屏。
 */
private class DemoScreenTransport(private val deviceId: String, private val flags: Set<String>) : FrameTransport {
    private val inbound = Channel<ByteArray>(Channel.UNLIMITED)
    private val decoder = FrameDecoder()
    override val remoteAddress: String = "192.168.1.20"

    init {
        push(Frame.control(GateMessage.Challenge(gateVersion, ByteArray(32) { it.toByte() }, demoMachineName)))
    }

    override suspend fun receive(): ByteArray? = inbound.receiveCatching().getOrNull()

    override fun send(data: ByteArray) {
        val messages = synchronized(decoder) {
            decoder.append(data)
            generateSequence { decoder.next() }.filter { it.kind == FrameKind.Control }.map { it.payload.decodeToString() }.toList()
        }
        val capture = "offline" !in flags
        for (message in messages) {
            when {
                "\"type\":\"remote_auth\"" in message -> push(Frame.control(GateMessage.Accepted(deviceId)))
                "\"type\":\"screen_hello\"" in message -> push(
                    """{"type":"ready","displays":[{"id":1,"width":1728,"height":1117,"pixel_width":3456,"pixel_height":2234},""" +
                        """{"id":2,"width":2560,"height":1440,"pixel_width":2560,"pixel_height":1440}],""" +
                        """"screen_capture":$capture,"post_event":true}""",
                )
                "\"type\":\"start\"" in message && !capture -> push(
                    """{"type":"stopped","reason":"no screen capture permission","cause":"no_screen_capture"}""",
                )
                "\"type\":\"start\"" in message -> {
                    push("""{"type":"started","display":1,"width":1728,"height":1117}""")
                    if ("locked" in flags) push("""{"type":"lock","locked":true}""")
                }
            }
        }
    }

    private fun push(json: String) = push(Frame(FrameKind.Control, 0u, json.encodeToByteArray()))

    private fun push(frame: Frame) {
        inbound.trySend(frame.encoded())
    }

    override fun exporter(): ByteArray = ByteArray(32)

    override fun close() {
        inbound.close()
    }
}

/** 假的宿主：收到什么就照协议回什么。[flags] 是演示参数里的词。 */
private class DemoLink(private val flags: Set<String>) : HostLink {
    private val lock = Any()
    private val subscribers = mutableListOf<SendChannel<HostEvent>>()
    private val sizes = mutableMapOf<SessionId, GridSize>()
    private var request = 0u
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private val connected = LinkState.Connected(demoMachineName, "192.168.1.20")
    private val mutableState = MutableStateFlow<LinkState>(connected)
    override val state: StateFlow<LinkState> = mutableState

    private val theme: TermSettings =
        if ("light" in flags) TermSettings.default.copy(background = Rgb(0xFBF8F1u), foreground = Rgb(0x3B3A36u)) else TermSettings.default

    override fun events(): Flow<HostEvent> = callbackFlow {
        trySend(HostEvent.State(connected))
        trySend(HostEvent.Ready(1u))
        synchronized(lock) { subscribers += channel }
        // 带着 `offline`：连上一会儿后假装断线、等着重连，用来看断线的横幅。
        if ("offline" in flags) {
            scope.launch {
                delay(1500)
                val waiting = LinkState.Waiting(
                    LinkFailure.Closed(demoText("电脑关闭了连接", "The computer closed the connection")),
                    Instant.now().plusSeconds(9),
                )
                mutableState.value = waiting
                emit(HostEvent.State(waiting))
            }
        }
        awaitClose { synchronized(lock) { subscribers -= channel } }
    }.buffer(Channel.UNLIMITED)

    private fun emit(event: HostEvent) {
        for (subscriber in synchronized(lock) { subscribers.toList() }) subscriber.trySend(event)
    }

    private fun size(session: SessionInfo): GridSize = synchronized(lock) { sizes[session.id] } ?: session.size

    override fun send(message: ClientMsg) {
        when (message) {
            ClientMsg.ListSessions -> emit(HostEvent.Message(HostMsg.SessionList(sessions.map { it.copy(size = size(it)) })))
            is ClientMsg.Attach -> attach(message)
            is ClientMsg.Layout -> emit(HostEvent.Message(HostMsg.Layout(message.req, layout)))
            is ClientMsg.Resize -> {
                if (sessions.firstOrNull { it.id == message.id }?.sizeOwner != null) return
                synchronized(lock) { sizes[message.id] = message.size }
                emit(HostEvent.Message(HostMsg.Resized(message.id, message.size)))
            }
            is ClientMsg.Focus -> {
                if (!message.focused || sessions.firstOrNull { it.id == message.id }?.sizeOwner != null) return
                emit(HostEvent.Message(HostMsg.SizeOwner(message.id, mine = true, owner = demoPhoneName)))
            }
            is ClientMsg.ReadScreen -> {
                // 普通 shell 有 shell 集成：读最近一条命令的输出时只给输出；别的会话读屏幕。
                val text = if (message.command != null && message.id == shell.id) shellLastOutput else plain(screen(message.id))
                emit(HostEvent.Message(HostMsg.ScreenText(message.id, text, truncated = false)))
            }
            is ClientMsg.SendKeys -> emit(HostEvent.Message(HostMsg.Done(message.req)))
            is ClientMsg.Paste -> emit(HostEvent.Message(HostMsg.Done(message.req)))
            is ClientMsg.Git -> emit(HostEvent.Message(git(message.req, message.id, message.request)))
            is ClientMsg.ListDirs -> {
                val path = message.path ?: home
                emit(HostEvent.Message(HostMsg.Dirs(message.req, path, folders[path].orEmpty(), truncated = false)))
            }
            // 新开终端的菜单里列出几家 agent，看得到它们的 logo。
            is ClientMsg.FindCommands -> emit(
                HostEvent.Message(HostMsg.Commands(message.req, message.programs.map { it.takeIf { it in installedPrograms } })),
            )
            else -> {}
        }
    }

    /** 假仓库：暂存、提交、切分支都改它，读状态时照它回。 */
    private var repo = GitStatus(
        root = "/Users/ethan/dev/runode",
        branch = "main",
        head = "581d7b4",
        upstream = "origin/main",
        ahead = 1u,
        behind = 2u,
        hasRemote = true,
        staged = listOf(GitFile(path = "crates/protocol/src/git.rs", status = GitFileStatus.Added, added = 182u)),
        unstaged = listOf(
            GitFile(path = "crates/host/src/server.rs", status = GitFileStatus.Modified, added = 34u, removed = 3u),
            GitFile(path = "apps/android/features/src/main/kotlin/dev/runode/mobile/features/model/GitModel.kt", status = GitFileStatus.Untracked, added = 290u),
            GitFile(path = "docs/old-notes.md", status = GitFileStatus.Deleted, removed = 41u),
        ),
    )

    private fun git(req: UInt, id: SessionId, request: GitRequest): HostMsg = synchronized(lock) {
        fun move(paths: List<String>, toStaged: Boolean) {
            val from = if (toStaged) repo.unstaged else repo.staged
            val moving = from.filter { it.path in paths }
            repo = if (toStaged) {
                repo.copy(unstaged = repo.unstaged.filterNot { it.path in paths }, staged = (repo.staged + moving).sortedBy { it.path })
            } else {
                repo.copy(staged = repo.staged.filterNot { it.path in paths }, unstaged = (repo.unstaged + moving).sortedBy { it.path })
            }
        }
        when (request) {
            GitRequest.Status, GitRequest.Fetch -> {}
            is GitRequest.Stage -> move(request.paths, toStaged = true)
            is GitRequest.Unstage -> move(request.paths, toStaged = false)
            is GitRequest.Discard -> repo = repo.copy(unstaged = repo.unstaged.filterNot { it.path in request.paths })
            GitRequest.StageAll -> move(repo.unstaged.map { it.path }, toStaged = true)
            GitRequest.UnstageAll -> move(repo.staged.map { it.path }, toStaged = false)
            is GitRequest.Commit -> repo = repo.copy(
                unstaged = if (request.stageAll) emptyList() else repo.unstaged,
                staged = emptyList(),
                ahead = repo.ahead + 1u,
                head = "9c0ffee",
            )
            GitRequest.Pull -> repo = repo.copy(behind = 0u)
            GitRequest.Push -> repo = repo.copy(ahead = 0u)
            GitRequest.Sync -> repo = repo.copy(ahead = 0u, behind = 0u)
            is GitRequest.Checkout -> repo = repo.copy(branch = request.branch.removePrefix("origin/"))
            GitRequest.Branches -> {
                val settings = demoText("feat: Android 加设置页", "feat: add Android settings")
                val branches = listOf(
                    GitBranch(name = "main", upstream = "origin/main", subject = settings, date = "2 hours ago"),
                    GitBranch(name = "mobile-git", subject = demoText("wip: 手机端 Git", "wip: Git on mobile"), date = "5 minutes ago"),
                    GitBranch(name = "origin/main", remote = true, subject = settings, date = "2 hours ago"),
                    GitBranch(name = "origin/release", remote = true, subject = "chore: 0.4.0", date = "3 days ago"),
                ).map { it.copy(current = it.name == repo.branch) }
                return HostMsg.GitBranches(req, id, branches)
            }
            is GitRequest.Diff -> {
                val file = (if (request.staged) repo.staged else repo.unstaged).firstOrNull { it.path == request.path }
                    ?: return HostMsg.GitDiff(req, id, null)
                return HostMsg.GitDiff(req, id, GitFileDiff(file, listOf(GitHunk("@@ -940,5 +940,6 @@ fn handle", diffLines))))
            }
        }
        HostMsg.GitStatus(req, id, repo)
    }

    private fun attach(message: ClientMsg.Attach) {
        val session = sessions.firstOrNull { it.id == message.id } ?: return
        val id = session.id
        when (message.mode) {
            AttachMode.VtReplay -> {
                val requested = message.size
                if (requested != null && session.sizeOwner == null) synchronized(lock) { sizes[id] = requested }
                emit(HostEvent.Message(HostMsg.Attached(id, 1u, size(session), AttachMode.VtReplay, session.meta, theme)))
                emit(HostEvent.Frame(Frame(FrameKind.Snapshot, 1u, screen(id).encodeToByteArray()), 1u))
                emit(HostEvent.Message(HostMsg.SnapshotEnd(id)))
                session.sizeOwner?.let { emit(HostEvent.Message(HostMsg.SizeOwner(id, mine = false, owner = it))) }
            }
            // 和真的宿主一样，只看状态的 `Attach` 也带着主题，首页和列表据此上色。
            else -> emit(HostEvent.Message(HostMsg.Attached(id, 0u, size(session), message.mode, session.meta, theme)))
        }
    }

    override fun sendInput(data: ByteArray, channel: UInt, generation: ULong) {
        // 回显，方便在模拟器里试键盘。
        emit(HostEvent.Frame(Frame(FrameKind.Output, channel, data), generation))
    }

    override suspend fun start() {}

    override suspend fun stop() {}

    override suspend fun reconnectNow() {}

    override fun setFocused(focused: Boolean) {}

    override suspend fun setDeviceName(name: String) {}

    override fun nextRequestId(): UInt = synchronized(lock) {
        request += 1u
        request
    }

    companion object {
        val claude = SessionInfo(
            id = SessionId("0123456789abcdef0011223344556677")!!,
            size = GridSize(64u, 20u, 16u, 32u),
            meta = SessionMeta(
                title = demoText("重构连接层", "Refactor the connection layer"),
                agent = Agent(AgentKind("claude"), AgentState.Blocked),
                cwd = "/Users/ethan/dev/runode",
            ),
            clients = 1u,
            claimed = true,
            sizeOwner = demoMachineName,
        )
        val codex = SessionInfo(
            id = SessionId("fedcba98765432100011223344556677")!!,
            size = GridSize(100u, 30u, 16u, 32u),
            meta = SessionMeta(
                title = demoText("补 iOS 的单元测试", "Add iOS unit tests"),
                agent = Agent(AgentKind("codex"), AgentState.Working),
                cwd = "/Users/ethan/dev/runode/apps/ios",
            ),
        )
        val shell = SessionInfo(
            id = SessionId("aaaabbbbccccdddd0011223344556677")!!,
            size = GridSize(80u, 24u, 16u, 32u),
            meta = SessionMeta(fallbackTitle = "zsh", cwd = "/Users/ethan/dev/runode", foregroundIsShell = true),
        )
        val sessions = listOf(claude, codex, shell)

        /** 选目录时的家目录和几个目录里的子目录；别的目录是空的。 */
        const val home = "/Users/ethan"
        val folders = mapOf(
            home to listOf(".config", "Desktop", "Documents", "Downloads", "dev"),
            "$home/dev" to listOf("notes", "runode", "website"),
            "$home/dev/runode" to listOf("apps", "crates", "docs", "scripts", "vendor"),
        )

        /** 电脑上「装着」的 agent 命令。 */
        val installedPrograms = setOf("claude", "codex", "opencode", "copilot", "muse")

        /** Git 页里看一个文件的改动时都给这一段。 */
        val diffLines = listOf(
            GitLine(GitLineKind.Context, old = 940u, new = 940u, text = "            ClientMsg::Paste { req, id, text } => {"),
            GitLine(GitLineKind.Context, old = 941u, new = 941u, text = "                self.deliver_done(req, id, DriveAction::Paste, Inbox::Paste(text))"),
            GitLine(GitLineKind.Context, old = 942u, new = 942u, text = "            }"),
            GitLine(GitLineKind.Added, new = 943u, text = "            ClientMsg::Git { req, id, request } => self.git(req, id, request),"),
            GitLine(GitLineKind.Removed, old = 943u, text = "            ClientMsg::UiReply { ui, reply } => self.ui_reply(ui, reply),"),
            GitLine(GitLineKind.Added, new = 944u, text = "            ClientMsg::UiReply { ui, reply } => self.ui_reply(ui, *reply),"),
            GitLine(
                GitLineKind.Context,
                old = 944u,
                new = 945u,
                text = demoText(
                    "            // 读写剪贴板是宿主替会话里的程序请界面办的，前端不能直接要。",
                    "            // Clipboard access is requested by the host on behalf of the session.",
                ),
            ),
        )

        /** 一个工作区：第一个标签左右分屏放两个 agent，第二个标签是普通 shell。 */
        val layout: List<WindowLayout> = run {
            val split = TabLayout(
                index = 1u,
                active = true,
                panes = listOf(
                    PaneLayout(1u, claude.id, PaneRect(0u, 0u, 500u, 1000u), focused = true),
                    PaneLayout(2u, codex.id, PaneRect(500u, 0u, 500u, 1000u)),
                ),
            )
            val shellTab = TabLayout(index = 2u, panes = listOf(PaneLayout(1u, shell.id, PaneRect(0u, 0u, 1000u, 1000u))))
            val workspace = WorkspaceLayout(1u, name = "runode", dir = "/Users/ethan/dev/runode", active = true, tabs = listOf(split, shellTab))
            listOf(WindowLayout(1u, front = true, workspaces = listOf(workspace)))
        }

        fun screen(id: SessionId): String = when (id) {
            claude.id -> claudeScreen
            codex.id -> codexScreen
            else -> shellScreen
        }

        /** 去掉 CSI 转义序列，当作读屏幕回来的纯文字。 */
        fun plain(screen: String): String = screen.replace(Regex("\u001b\\[[0-9;?]*[A-Za-z]"), "").replace("\r", "")

        val claudeScreen = listOf(
            "\u001b[38;2;215;119;87m●\u001b[0m " +
                demoText("先跑一遍协议的测试，确认帧格式没被改坏。", "Running the protocol tests first to check the frame format."),
            "",
            "\u001b[38;2;215;119;87m●\u001b[0m \u001b[1mRead\u001b[0m(crates/protocol/src/remote.rs)",
            "  ⎿  Read 412 lines",
            "",
            "\u001b[2m────────────────────────────────────────────────────────────\u001b[0m",
            " \u001b[1mBash command\u001b[0m",
            "",
            "   cargo test -p runode-protocol",
            "   \u001b[2mRun the protocol crate's tests\u001b[0m",
            "",
            " Do you want to proceed?",
            " \u001b[38;2;215;119;87m❯ 1. Yes\u001b[0m",
            "   2. Yes, and don't ask again for cargo test",
            "   3. No, and tell Claude what to do differently \u001b[2m(esc)\u001b[0m",
        ).joinToString("\r\n")

        val codexScreen = listOf(
            "\u001b[1m›\u001b[0m " +
                demoText("给 SessionListModel 的分组和预览节流补测试", "Add tests for SessionListModel grouping and preview throttling"),
            "",
            "• Explored",
            "  └ Read SessionListModel.swift, ListModelTests.swift",
            "",
            "• Edited Tests/RunodeFeaturesTests/ListModelTests.swift (+48 -2)",
            "",
            "\u001b[1m•\u001b[0m Working \u001b[2m(12s • esc to interrupt)\u001b[0m",
        ).joinToString("\r\n")

        /** 普通 shell 最近一条命令（`eza --icons`）的输出，不含提示符。 */
        val shellLastOutput = listOf(
            "\uF07B apps  \uE7A8 Cargo.toml  \uF48A README.md  \uF07B crates",
            "\uF07B docs  \uF15C LICENSE  \uE615 Makefile  \uF07B vendor",
        ).joinToString("\n")

        val shellScreen = listOf(
            "\u001b[1;32methan@mbp\u001b[0m:\u001b[1;34m~/dev/runode\u001b[0m$ git status --short",
            " \u001b[31mM\u001b[0m crates/protocol/src/remote.rs",
            "\u001b[32m??\u001b[0m apps/ios/",
            "\u001b[7m 反显 \u001b[0m \u001b[1m粗体\u001b[0m \u001b[3m斜体\u001b[0m \u001b[4m下划线\u001b[0m 中文宽字符 ✅",
            "\u001b[34m\uF07B apps\u001b[0m  \uE7A8 Cargo.toml  \uF48A README.md  \uE0A0 main",
            "\u001b[38;2;23;22;24;48;2;45;154;255m \uF179 \uF07C ~/dev/runode " +
                "\u001b[38;2;45;154;255;48;2;42;217;71m\uE0B0" +
                "\u001b[38;2;23;22;24m \uE0A0 main \uF113 " +
                "\u001b[0m\u001b[38;2;42;217;71m\uE0B0\u001b[0m ",
        ).joinToString("\r\n")
    }
}
