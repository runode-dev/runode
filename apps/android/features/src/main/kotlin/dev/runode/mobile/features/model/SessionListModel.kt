package dev.runode.mobile.features.model

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import dev.runode.mobile.connection.HostEvent
import dev.runode.mobile.connection.HostLink
import dev.runode.mobile.connection.LinkState
import dev.runode.mobile.connection.MachineRecord
import dev.runode.mobile.features.R
import dev.runode.mobile.protocol.AgentState
import dev.runode.mobile.protocol.AttachMode
import dev.runode.mobile.protocol.ClientMsg
import dev.runode.mobile.protocol.GridSize
import dev.runode.mobile.protocol.HostMsg
import dev.runode.mobile.protocol.IntegrationMode
import dev.runode.mobile.protocol.PaneRect
import dev.runode.mobile.protocol.Placement
import dev.runode.mobile.protocol.SessionId
import dev.runode.mobile.protocol.SessionInfo
import dev.runode.mobile.protocol.ShellReport
import dev.runode.mobile.protocol.TaskEntry
import dev.runode.mobile.protocol.TaskSource
import dev.runode.mobile.protocol.WindowLayout
import kotlin.time.ComparableTimeMark
import kotlin.time.Duration
import kotlin.time.Duration.Companion.seconds
import kotlin.time.TimeSource
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/** 会话的 agent 状态分成几类：等用户回答的、在干活的、其他。首页按它汇总，卡片按它标徽标、带快速回复。 */
enum class SessionGroup {
    /** agent 停下来等回答（`AgentState::Blocked`）。 */
    Waiting,

    /** agent 在干活。 */
    Working,
    Other,
    ;

    companion object {
        fun of(session: SessionInfo): SessionGroup {
            val agent = session.meta.agent
            if (session.exited || agent == null) return Other
            return when (agent.state) {
                AgentState.Blocked -> Waiting
                AgentState.Working -> Working
                else -> Other
            }
        }
    }
}

/** 会话列表顶上的筛选：全部会话，或只看等你回答的、在干活的。 */
enum class SessionFilter {
    All,
    Waiting,
    Working,
    ;

    /** 只留这一类的会话；[All] 为空，什么都留。 */
    internal val group: SessionGroup?
        get() = when (this) {
            All -> null
            Waiting -> SessionGroup.Waiting
            Working -> SessionGroup.Working
        }
}

/** 一个会话在界面上标的状态，比 [SessionGroup] 细：没有 agent 的会话再分前台在跑别的程序和只有 shell。 */
sealed class SessionStatus {
    /** agent 停下来等回答。 */
    data object Waiting : SessionStatus()

    /** agent 在干活。 */
    data object Working : SessionStatus()

    /** 有 agent，但空闲或状态认不出。 */
    data object AgentIdle : SessionStatus()

    /** 前台在跑 shell 以外的程序，带着界面上写的程序名。 */
    data class Running(val program: String) : SessionStatus()

    data object Shell : SessionStatus()

    /** shell 已经退出，会话还留着。 */
    data object Exited : SessionStatus()

    companion object {
        fun of(session: SessionInfo): SessionStatus {
            if (session.exited) return Exited
            val agent = session.meta.agent
            if (agent != null) {
                return when (agent.state) {
                    AgentState.Blocked -> Waiting
                    AgentState.Working -> Working
                    AgentState.Idle, is AgentState.Unknown -> AgentIdle
                }
            }
            val foreground = session.meta.foreground
            if (!session.meta.foregroundIsShell && !foreground.isNullOrEmpty()) {
                return Running(Presentation.programLabel(foreground))
            }
            return Shell
        }
    }
}

/** 列表里的一节：电脑上的 app 里的一个工作区，或者不在任何窗口里的会话。 */
data class SessionSection(
    val id: Id,
    /** 工作区的名字；后台那一节为空。 */
    val name: String? = null,
    /** 工作区的目录。 */
    val dir: String? = null,
    /** 开着不止一个窗口时，工作区在第几个窗口。 */
    val window: UInt? = null,
    /** 工作区里按标签、分屏的先后，一个标签都没有的工作区为空；后台那一节按宿主给的先后。 */
    val sessions: List<SessionInfo>,
    /** 在这个工作区里新开终端时挨着的会话，见 `WorkspaceLayout.anchor`；后台那一节为空。 */
    val anchor: SessionId? = null,
) {
    sealed class Id {
        /** 第 [window] 个窗口里的第 [index] 个工作区，序号见 [WindowLayout]。 */
        data class Workspace(val window: UInt, val index: UInt) : Id()

        /** 没有窗口在显示的会话（电脑上的 app 没开着时是所有会话）。 */
        data object Background : Id()
    }
}

/** 电脑上一个分了屏的标签里的一个分屏。 */
data class SplitPane(
    val session: SessionInfo,
    /** 在标签里的位置，电脑没报时为空。 */
    val rect: PaneRect?,
) {
    val id: SessionId get() = session.id
}

/** 每个会话最多多久读一次屏幕做预览。 */
internal class PreviewThrottle(var interval: Duration) {
    sealed class Decision {
        /** 现在就读。 */
        data object Now : Decision()

        /** 过这么久再读（已经替它排好了）。 */
        data class Later(val delay: Duration) : Decision()

        /** 已经排着一次了，不用再排。 */
        data object Skip : Decision()
    }

    private val last = mutableMapOf<SessionId, ComparableTimeMark>()
    private val scheduled = mutableSetOf<SessionId>()

    fun request(id: SessionId, now: ComparableTimeMark): Decision {
        if (id in scheduled) return Decision.Skip
        val last = last[id]
        if (last != null && now - last < interval) {
            scheduled += id
            return Decision.Later(interval - (now - last))
        }
        this.last[id] = now
        return Decision.Now
    }

    /** 排着的那次到点了。 */
    fun fire(id: SessionId, now: ComparableTimeMark) {
        scheduled -= id
        last[id] = now
    }

    fun reset() {
        last.clear()
        scheduled.clear()
    }
}

/**
 * 一台电脑的会话列表：连上后 `ListSessions`，给每个会话发只看状态（`MetaOnly`）的 `Attach` 收实时的标题、目录和
 * agent 状态；用 `Layout` 问电脑上的 app 各个会话在哪个工作区，按工作区分节；用 `ReadScreen` 读屏幕底部几行做预览；
 * 等回答的会话能直接快速回复（`SendKeys`、`Paste`，不用连上会话）；能在某个工作区里新开终端、新建工作区
 * （[DirectoryPickerModel] 选目录）、给工作区改名、结束会话。
 *
 * 一条连接上一个会话只能有一个订阅，后来的 `Attach` 换掉先前的。所以终端页开着的会话（`screens`）列表不再给它发只看
 * 状态的 `Attach`，终端页关掉时再改回只看状态。
 */
class SessionListModel(
    machine: MachineRecord,
    val link: HostLink,
    previewInterval: Duration = 1.seconds,
    private val now: () -> ComparableTimeMark = { TimeSource.Monotonic.markNow() },
) {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    /** 改名以后 [AppModel] 换上新的记录。 */
    var machine by mutableStateOf(machine)
        internal set

    private var linkStateValue by mutableStateOf<LinkState>(LinkState.Idle)
    var linkState: LinkState
        get() = linkStateValue
        private set(value) {
            linkStateValue = value
            onSessionsChanged()
        }

    private var sessionsValue by mutableStateOf<List<SessionInfo>>(emptyList())
    var sessions: List<SessionInfo>
        get() = sessionsValue
        private set(value) {
            sessionsValue = value
            onSessionsChanged()
        }

    /** 电脑上的 app 的窗口，按窗口的序号；app 没开着窗口时为空。 */
    var windows by mutableStateOf<List<WindowLayout>>(emptyList())
        private set

    /** 每个会话屏幕底部的几行，去掉了空行。 */
    var previews by mutableStateOf<Map<SessionId, List<String>>>(emptyMap())
        private set

    /** 等你回答的会话屏幕上认出来的提问，和 [previews] 读自同一次读屏幕；认不出、不在等回答时没有。 */
    var prompts by mutableStateOf<Map<SessionId, AgentPrompt>>(emptyMap())
        private set

    private var loadedValue by mutableStateOf(false)

    /** 连上后收到过一次列表。 */
    var loaded: Boolean
        get() = loadedValue
        private set(value) {
            loadedValue = value
            onSessionsChanged()
        }

    /**
     * 这次连上以后收到过列表：[sessions] 是电脑上现在的样子。断开后、重连上还没收到新列表时为假，那时 [sessions] 还是
     * 断开前的。
     */
    internal var listCurrent by mutableStateOf(false)
        private set

    /** 这台电脑上终端的主题：只看状态的 `Attach` 回话里带着，终端页开着时它收到的 `ThemeApplied` 也经同一条连接到这里。没收到过时为空。 */
    var theme by mutableStateOf<AppTheme?>(null)
        private set

    var isSpawning by mutableStateOf(false)
        private set

    var errorMessage by mutableStateOf<UiText?>(null)

    /** 等用户确认结束的会话。 */
    var killTarget by mutableStateOf<SessionId?>(null)

    /** 各个会话目录里能跑的命令（Makefile 的目标、package.json 的 scripts），按目录；宿主回过话的目录才有，列不出来的目录是空的。 */
    var tasks by mutableStateOf<Map<String, List<TaskSource>>>(emptyMap())
        private set

    /** 这台电脑上装着的 agent，新开终端时能直接起它们。每次连上问一遍宿主，问到之前是上次的；宿主不认得这个请求时为空。 */
    var agents by mutableStateOf<List<InstalledAgent>>(emptyList())
        private set

    /** 新建工作区时选目录用的，选着的时候才有。 */
    var directoryPicker by mutableStateOf<DirectoryPickerModel?>(null)
        private set

    /** 尺寸归这部手机的会话：宿主报 `SizeOwner` 时记下，收到新列表时清掉（列表里的 `size_owner` 是手机报的设备名）。 */
    var phoneOwnsSize by mutableStateOf<Set<SessionId>>(emptySet())
        private set

    /** 新开会话用的网格尺寸，视图按手机屏幕算好后设进来。 */
    var spawnSize = GridSize(80u, 24u, 16u, 32u)

    /** 新开的会话开好了（宿主回了 `Opened` 或 `Spawned`），[AppModel] 据此打开它的终端页。 */
    var onSpawned: (SessionId) -> Unit = {}

    /** [theme] 变了，[AppModel] 据此记下 App 现在用的主题。 */
    internal var onThemeChanged: () -> Unit = {}

    /** 会话、连接状态或 [loaded] 变了时调。 */
    internal var onSessionsChanged: () -> Unit = {}

    /** 连接的事件（帧以外的），在列表自己处理之前转给 [AppModel]，由它交给 [PushRegistration]。 */
    internal var onLinkEvent: (HostEvent) -> Unit = {}

    private var task: Job? = null
    private var connected = false

    /** 这次连接上已经发过只看状态的 `Attach` 的会话。 */
    private val watching = mutableSetOf<SessionId>()

    /** 终端页开着的会话。 */
    private val screens = mutableSetOf<SessionId>()

    /** 在等回话的新开终端、新建工作区的请求。宿主对 `Open`、`OpenWorkspace`、`Spawn` 出错一定回带编号的 `Error`，不带编号的不认领。 */
    private var pendingSpawn: PendingSpawn? = null

    /** 在等回话的工作区改名的请求编号。 */
    private val pendingRenames = mutableSetOf<UInt>()

    /** 对连接的开、停按调用的先后做：后台、前台来回切得快时不会先开后停。 */
    private var linkControl: Job? = null
    private val throttle = PreviewThrottle(previewInterval)
    private val quickReplies = mutableMapOf<SessionId, QuickReplyModel>()

    /**
     * 每个会话在等的那次读屏幕是哪种，回话按会话对上：`ScreenText` 不带请求编号，宿主又是各自起线程回话，两次同时在路上时
     * 回话可能先后颠倒、对错读法，所以同一个会话同时只读一次（[sendPreviewRead]）。
     */
    private val pendingPreviews = mutableMapOf<SessionId, PreviewRead>()

    /** 读着的时候又要刷新的会话：这次（连同退回读屏幕底部的后半）有了回话再读一次。 */
    private val previewsAgain = mutableSetOf<SessionId>()

    /**
     * 答过提问、还没见它从屏幕上换掉的会话：所答的提问、答时 agent 进入等回答的时刻
     * （[dev.runode.mobile.protocol.SessionMeta.agentSinceMs]），和读到的还是它时还能再读几次。
     */
    private val answeredPrompts = mutableMapOf<SessionId, AnsweredPrompt>()

    /** 在等回话的列命令的请求，按请求编号记着列的目录。 */
    private val pendingTasks = mutableMapOf<UInt, String>()

    /** 正在列的目录：取请求编号之前就记上，同一个目录不会同时要两次。 */
    private val listingDirs = mutableSetOf<String>()

    /** 在等回话的找 agent 的请求编号。 */
    private var pendingAgentSearch: UInt? = null

    /** 上次找 agent 时交给宿主的 PATH；这次连上还没找过时为空。 */
    private var searchedPath: SearchedPath? = null

    /** 各个会话里 shell 报告的 PATH，从只看状态的 `Attach` 和之后的 `Meta` 里来。 */
    private val shellPaths = mutableMapOf<SessionId, String>()

    private class PendingSpawn(
        var req: UInt,
        /** 发着 `Open` 时预先取好的、退回 `Spawn` 用的编号；新建工作区、已经退回去了时为空。 */
        var fallback: UInt?,
        /** 没开成时提示的前半句。 */
        val failure: UiText,
        /** 退回 `Spawn` 时用的目录，为空时用家目录。 */
        val cwd: String? = null,
        /** 开好后在新终端里跑的命令行，带着预先取好的粘贴、回车的请求编号。 */
        val command: Launch? = null,
    )

    private class Launch(val text: String, val paste: UInt, val enter: UInt)

    private class AnsweredPrompt(val asked: AgentPrompt.Asked, val since: Long?, var rereads: Int)

    private class SearchedPath(val path: String?)

    /** 预览怎么读屏幕。 */
    internal sealed class PreviewRead {
        /** shell 在提示符上等输入：读最近一条命令的输出（`command: 1`），宿主按 shell 集成标出的提示符切，完全不含提示符。 */
        data object LastCommand : PreviewRead()

        /**
         * 读屏幕底部几行；[atPrompt] 为真时去掉最后一个有字的行（光标所在的提示符）。没有 shell 集成、上一条命令没有输出时
         * 的回退，也是 agent 在跑、前台不是 shell 时的读法。
         */
        data class BottomLines(val atPrompt: Boolean) : PreviewRead()
    }

    /**
     * 按电脑上的工作区分好节的会话：窗口按序号，工作区按侧栏里的先后，最后是不在任何窗口里的会话。没有会话的工作区也出
     * 一节（`sessions` 为空）：电脑上关掉了最后一个标签，或者标签都还没启动（恢复窗口时后台的标签要点开才起 shell，布局
     * 里没有它们的会话）。分屏里有会话、但列表里一个都还没有的工作区不出现（刚开的会话列表里还没有时，等下一次列表，免得
     * 闪一下空节）。
     */
    val sections: List<SessionSection>
        get() {
            val byId = sessions.reversed().associateBy { it.id }
            val placed = mutableSetOf<SessionId>()
            val sections = mutableListOf<SessionSection>()
            val windows = windows.sortedBy { it.index }
            for (window in windows) {
                for (workspace in window.workspaces) {
                    val members = workspace.sessions.mapNotNull { id ->
                        if (id in placed) return@mapNotNull null
                        byId[id]?.also { placed += id }
                    }
                    if (members.isEmpty() && workspace.sessions.isNotEmpty()) continue
                    sections += SessionSection(
                        id = SessionSection.Id.Workspace(window.index, workspace.index),
                        name = workspace.name,
                        dir = workspace.dir,
                        window = if (windows.size > 1) window.index else null,
                        sessions = members,
                        anchor = workspace.anchor,
                    )
                }
            }
            val rest = sessions.filterNot { it.id in placed }
            if (rest.isNotEmpty()) sections += SessionSection(SessionSection.Id.Background, sessions = rest)
            return sections
        }

    /** 按 [filter] 筛过的 [sections]：[SessionFilter.All] 原样返回，没有会话的工作区照留；否则每节只留这一类的会话，一个都不剩的节整节去掉。 */
    fun sections(matching: SessionFilter): List<SessionSection> {
        val group = matching.group ?: return sections
        return sections.mapNotNull { section ->
            section.copy(sessions = section.sessions.filter { SessionGroup.of(it) == group }).takeIf { it.sessions.isNotEmpty() }
        }
    }

    /** 合 [filter] 的会话有几个。 */
    fun count(filter: SessionFilter): Int {
        val group = filter.group ?: return sessions.size
        return sessions.count { SessionGroup.of(it) == group }
    }

    /** 订阅连接的事件并要求连接。 */
    fun start() {
        if (task != null) return
        val previous = linkControl
        val subscribed = CompletableDeferred<Flow<HostEvent>>()
        linkControl = scope.launch {
            previous?.join()
            val events = link.events()
            link.start()
            subscribed.complete(events)
        }
        task = scope.launch {
            subscribed.await().collect { handle(it) }
        }
    }

    /**
     * 离开这台电脑或者 App 进了后台：停止订阅，断开连接。订阅停了，连接断开的状态转不过来，这里自己当作断开处理，不然回到
     * 前台、还没连上时会照旧往没有的连接上发请求。
     */
    fun stop() {
        task?.cancel()
        task = null
        handle(HostEvent.State(LinkState.Idle))
        val previous = linkControl
        linkControl = scope.launch {
            previous?.join()
            link.stop()
        }
    }

    fun refresh() {
        if (!connected) return
        link.send(ClientMsg.ListSessions)
        link.send(ClientMsg.Layout(layoutRequest))
    }

    /**
     * 看着列表的时候每隔几秒重新要一次列表：别处新开的会话宿主不会主动告诉这条连接。在干活、在等回答的会话顺带刷新预览，
     * 它们的屏幕变得快，状态却不一定变。
     */
    suspend fun keepRefreshing(every: Duration = 5.seconds) {
        refreshPreviews()
        while (currentCoroutineContext().isActive) {
            delay(every)
            refresh()
            for (session in sessions) {
                if (SessionGroup.of(session) != SessionGroup.Other) requestPreview(session.id)
            }
        }
    }

    /** 进入列表时：所有会话的预览都刷新一次。 */
    fun refreshPreviews() {
        for (session in sessions) requestPreview(session.id)
    }

    fun reconnect() {
        scope.launch { link.reconnectNow() }
    }

    /**
     * 新开一个会话，开好后调 [onSpawned]；[agent] 不为空时开好后在它的 shell 里起这家 agent。先请电脑上的 app 开一个
     * 新标签（`Open`），电脑上也看得到、接着用：[near] 是挨着哪个会话（某个工作区的 `anchor`），为空时开在最前面那个窗口
     * 当前的工作区里。app 没开着窗口（宿主回 `Error`）时退回 `Spawn`，开一个只在后台跑的会话。
     */
    fun spawn(near: SessionId? = null, agent: InstalledAgent? = null) {
        if (!connected || isSpawning) return
        isSpawning = true
        val req = link.nextRequestId()
        val fallback = link.nextRequestId()
        pendingSpawn = PendingSpawn(req, fallback, UiText.res(R.string.spawn_failed), command = launch(agent))
        link.send(ClientMsg.Open(req, Placement.Tab, near, null, false))
    }

    /** 开好后起 [agent] 要打的命令，带着预先取好的粘贴、回车的请求编号；[agent] 为空时不打。 */
    private fun launch(agent: InstalledAgent?): Launch? =
        agent?.let { Launch(it.command, link.nextRequestId(), link.nextRequestId()) }

    /**
     * 问宿主这台电脑上装着哪些 agent，回话到了换掉 [agents]。按会话里 shell 报告的 PATH 找：宿主新起的 shell 拿到的 PATH
     * 还没经过用户的 shell 配置，常常没有装 agent 的目录；一个会话都没报告时只能按它找。
     */
    fun findAgents() {
        if (!connected) return
        val path = reportedPath
        val req = link.nextRequestId()
        pendingAgentSearch = req
        searchedPath = SearchedPath(path)
        link.send(ClientMsg.FindCommands(req, path, AgentLauncher.programs))
    }

    /** 列表里第一个报告了 PATH 的会话的 PATH。 */
    private val reportedPath: String?
        get() = sessions.firstNotNullOfOrNull { shellPaths[it.id] }

    /** 记下会话 [id] 的 shell 报告；[reportedPath] 因此变了时重新找 agent。 */
    private fun noteShell(shell: ShellReport?, id: SessionId) {
        shell ?: return
        val path = shell.path
        if (path == null) shellPaths -= id else shellPaths[id] = path
        findAgentsIfPathChanged()
    }

    private fun findAgentsIfPathChanged() {
        val searched = searchedPath
        if (searched != null && searched.path == reportedPath) return
        findAgents()
    }

    /** 开始新建工作区：从家目录开始选目录。 */
    fun beginNewWorkspace() {
        if (!connected) return
        val picker = DirectoryPickerModel(link)
        directoryPicker = picker
        picker.load(null)
    }

    fun cancelNewWorkspace() {
        directoryPicker = null
    }

    /**
     * 在电脑上的 app 里新建目录是 [dir] 的工作区（`OpenWorkspace`），开好后调 [onSpawned] 打开它的第一个终端。[name] 是
     * 工作区的名字，去掉首尾空白后为空时按目录取。电脑上不切过去，不打断用户手上的事。app 没开着窗口时没有工作区可建，在
     * [dir] 里开一个只在后台跑的会话（`Spawn`），名字用不上。开着窗口时建不成就提示，不退回 `Spawn`：多半是目录有问题，
     * 后台会话也开不成。
     */
    fun createWorkspace(dir: String, name: String? = null) {
        directoryPicker = null
        if (!connected || isSpawning) return
        isSpawning = true
        val req = link.nextRequestId()
        if (hasDesktopWindow) {
            pendingSpawn = PendingSpawn(req, null, UiText.res(R.string.create_workspace_failed))
            link.send(ClientMsg.OpenWorkspace(req, dir, false, name?.trim()?.takeIf { it.isNotEmpty() }))
        } else {
            pendingSpawn = PendingSpawn(req, null, UiText.res(R.string.spawn_failed))
            link.send(ClientMsg.Spawn(req, spawnSize, dir, IntegrationMode.Detect, true))
        }
    }

    /**
     * 在工作区这一节里新开终端，[agent] 同 [spawn]：挨着它的 `anchor` 开新标签。一个标签都没有的工作区没有 `anchor`，
     * 对它的目录发 `OpenWorkspace`：电脑上的 app 对已有这个目录的工作区在它里面新开标签。
     */
    fun spawn(section: SessionSection, agent: InstalledAgent? = null) {
        val anchor = section.anchor
        if (anchor != null) {
            spawn(near = anchor, agent = agent)
            return
        }
        val dir = section.dir
        if (section.id !is SessionSection.Id.Workspace || dir == null || !connected || isSpawning) return
        isSpawning = true
        val req = link.nextRequestId()
        pendingSpawn = PendingSpawn(req, null, UiText.res(R.string.spawn_failed), command = launch(agent))
        link.send(ClientMsg.OpenWorkspace(req, dir, false))
    }

    /** 能在这一节里新开终端：有 `anchor`，或者是知道目录的空工作区。 */
    fun canSpawn(section: SessionSection): Boolean =
        section.id is SessionSection.Id.Workspace && (section.anchor != null || section.dir != null)

    /** 新开终端时让用户挑的工作区：能新开终端的工作区不止一个时是它们，否则为空，直接开在电脑上当前的工作区里（[spawn]）。 */
    val spawnChoices: List<SessionSection>
        get() = sections.filter(::canSpawn).takeIf { it.size > 1 } ?: emptyList()

    /** 把工作区这一节改名为 [name]（`RenameWorkspace`），办好后重新要布局。名字去掉首尾空白后为空时不发；后台那一节不是工作区，没法改名。 */
    fun rename(section: SessionSection, name: String) {
        val trimmed = name.trim()
        val id = section.id
        if (!connected || trimmed.isEmpty() || id !is SessionSection.Id.Workspace) return
        val req = link.nextRequestId()
        pendingRenames += req
        link.send(ClientMsg.RenameWorkspace(req, id.window, id.index, trimmed))
    }

    /** 电脑上的 app 开着窗口，能在里面建工作区。 */
    val hasDesktopWindow: Boolean get() = windows.isNotEmpty()

    /** 结束会话（视图先让用户确认）。没连着时请求发不出去，电脑上的会话照旧在跑，列表不动，提示出错。 */
    fun kill(id: SessionId) {
        if (!connected) {
            errorMessage = UiText.res(R.string.kill_disconnected)
            return
        }
        link.send(ClientMsg.Kill(id))
        sessions = sessions.filterNot { it.id == id }
        watching -= id
        previews = previews - id
        prompts = prompts - id
        quickReplies -= id
    }

    /** 这个会话的快速回复，第一次要时建好。送到以后刷新它的预览。重连后收到新列表之前不能发：卡片还是断开前的，那个会话可能已经不在等这个回答了。 */
    fun quickReply(id: SessionId): QuickReplyModel = quickReplies.getOrPut(id) {
        QuickReplyModel(id, link, connected && listCurrent) { answered(it) }
    }

    /**
     * 回答送到了：认出来的提问作废，免得接着按到下一问上。按键刚写进去、agent 还没重画，这时读到的还是所答的那个提问
     * （光标也许挪了，状态栏也许变了），所以记下它（[AgentPrompt.asked]）；屏幕读两次，后一次隔一个 `previewInterval`。
     */
    internal fun answered(id: SessionId) {
        prompts[id]?.let { answeredPrompts[id] = AnsweredPrompt(it.asked, session(id)?.meta?.agentSinceMs, answeredRereads) }
        prompts = prompts - id
        requestPreview(id)
        requestPreview(id)
    }

    /** 终端页开始看这个会话。 */
    fun screenOpened(id: SessionId) {
        screens += id
        watching -= id
    }

    /** 终端页不看了：改回只看状态，不放手这个会话的状态更新，也让出尺寸。 */
    fun screenClosed(id: SessionId) {
        screens -= id
        if (!connected || sessions.none { it.id == id }) return
        watching += id
        link.send(ClientMsg.Attach(id, null, AttachMode.MetaOnly))
        requestPreview(id)
    }

    fun session(id: SessionId): SessionInfo? = sessions.firstOrNull { it.id == id }

    /** 电脑上和 [id] 在同一个标签里的分屏（含它自己），按分屏的先后；它在标签里是独自一个、或不在任何窗口里时为空。 */
    fun panes(sharingTabWith: SessionId): List<SplitPane> {
        val tabs = windows.flatMap { it.workspaces }.flatMap { it.tabs }
        val tab = tabs.firstOrNull { tab -> tab.panes.any { it.id == sharingTabWith } } ?: return emptyList()
        val panes = tab.panes.mapNotNull { pane -> session(pane.id)?.let { SplitPane(it, pane.rect) } }
        return if (panes.size > 1) panes else emptyList()
    }

    /**
     * 电脑上 [id] 在所在工作区里是第几个标签页（从 1 数）。只有工作区里不止一个标签页列得出会话时才有：列表里就它一个
     * 标签页的卡片时标了也分不出什么；还没启动的标签页没有会话，不算。
     */
    fun tabNumber(id: SessionId): Int? {
        for (workspace in windows.flatMap { it.workspaces }) {
            val i = workspace.tabs.indexOfFirst { tab -> tab.panes.any { it.id == id } }
            if (i < 0) continue
            val listed = workspace.tabs.filter { tab -> tab.panes.any { session(it.id) != null } }
            return if (listed.size > 1) i + 1 else null
        }
        return null
    }

    /** 只重新要一次布局：电脑推来 `LayoutChanged`、终端页打开时要，列表自己的刷新那时不一定在跑。 */
    fun refreshLayout() {
        if (!connected) return
        link.send(ClientMsg.Layout(layoutRequest))
    }

    // 命令

    /** 这个会话目录里能跑的命令，还没列过或者目录不知道时为空。 */
    fun tasks(session: SessionInfo): List<TaskSource> = session.meta.cwd?.let { tasks[it] } ?: emptyList()

    /** 能跑这个会话目录里的命令：连着、终端没结束。 */
    fun canRunTask(session: SessionInfo): Boolean = connected && !session.exited

    /**
     * 跑会话 [id] 目录里列出的一条命令，在 [project]（`TaskSource.project`，为空时是会话目录）里跑。shell 停在提示符上、
     * 要跑的目录就是会话目录时，在这个会话里粘贴再回车，返回真；否则（前台在跑别的程序、或目录不同）在它右边分屏开一个
     * 那个目录的新终端跑，开好后调 [onSpawned]，返回假。
     */
    fun runTask(task: TaskEntry, project: String? = null, id: SessionId): Boolean {
        val session = session(id) ?: return false
        if (!canRunTask(session)) return false
        val cwd = project ?: session.meta.cwd
        if (session.meta.foregroundIsShell && cwd == session.meta.cwd) {
            val paste = link.nextRequestId()
            val enter = link.nextRequestId()
            link.send(ClientMsg.Paste(paste, id, task.command))
            link.send(ClientMsg.SendKeys(enter, id, listOf("enter")))
            return true
        }
        if (isSpawning) return false
        isSpawning = true
        val req = link.nextRequestId()
        val fallback = link.nextRequestId()
        val paste = link.nextRequestId()
        val enter = link.nextRequestId()
        pendingSpawn = PendingSpawn(
            req, fallback, UiText.res(R.string.spawn_failed), cwd = cwd, command = Launch(task.command, paste, enter),
        )
        link.send(ClientMsg.Open(req, Placement.Right, id, cwd, false))
        return false
    }

    /** 列 [dir] 里的命令：列过的不再要，[refreshing] 为真时照要；同一个目录同时只等一次回话。 */
    fun loadTasks(dir: String, refreshing: Boolean = false) {
        if (!connected || dir in listingDirs || (!refreshing && dir in tasks)) return
        listingDirs += dir
        val req = link.nextRequestId()
        pendingTasks[req] = dir
        link.send(ClientMsg.ListTasks(req, dir))
    }

    /** 重新连上：列断线前已知会话里还没有结果的目录，有结果的不重发。 */
    private fun loadMissingTasks() {
        val dirs = sessions.mapNotNull { it.meta.cwd }.toSortedSet()
        if (dirs.isEmpty()) return
        scope.launch { for (dir in dirs) loadTasks(dir) }
    }

    /** 下拉刷新：重新列一遍各个会话目录里的命令。 */
    fun refreshTasks() {
        for (dir in sessions.mapNotNull { it.meta.cwd }.toSortedSet()) loadTasks(dir, refreshing = true)
    }

    /** 收到列命令的回话（或者它的 `Error`）：记下结果，列不出来的目录记成空的，不再要。 */
    private fun finishTasks(req: UInt, sources: List<TaskSource>) {
        val dir = pendingTasks.remove(req) ?: return
        listingDirs -= dir
        tasks = tasks + (dir to sources)
    }

    // 预览

    /** 刷新一个会话的预览：每个会话最多每 `previewInterval` 读一次屏幕，来得太勤的合并成到点时的一次。 */
    internal fun requestPreview(id: SessionId) {
        if (!connected || sessions.none { it.id == id }) return
        when (val decision = throttle.request(id, now())) {
            PreviewThrottle.Decision.Now -> sendPreviewRead(id)
            is PreviewThrottle.Decision.Later -> scope.launch {
                delay(decision.delay)
                throttle.fire(id, now())
                sendPreviewRead(id)
            }
            PreviewThrottle.Decision.Skip -> {}
        }
    }

    /** 按会话现在的样子选读法发出去：shell 空闲时读最近一条命令的输出，否则读屏幕底部。 */
    private fun sendPreviewRead(id: SessionId) {
        if (!connected) return
        val session = session(id) ?: return
        if (id in pendingPreviews) {
            previewsAgain += id
            return
        }
        if (session.meta.foregroundIsShell && !session.exited) {
            send(PreviewRead.LastCommand, id)
        } else {
            send(PreviewRead.BottomLines(atPrompt = false), id)
        }
    }

    private fun send(read: PreviewRead, id: SessionId) {
        pendingPreviews[id] = read
        when (read) {
            PreviewRead.LastCommand -> link.send(ClientMsg.ReadScreen(id, null, 1u))
            is PreviewRead.BottomLines -> {
                val waiting = session(id)?.let(SessionGroup::of) == SessionGroup.Waiting
                link.send(ClientMsg.ReadScreen(id, if (waiting) waitingReadLines else previewReadLines))
            }
        }
    }

    /** 会话不在等回答了（或者没了）：认出来的提问作废。 */
    private fun dropStalePrompt(id: SessionId) {
        if (session(id)?.let(SessionGroup::of) == SessionGroup.Waiting) return
        prompts = prompts - id
        answeredPrompts -= id
    }

    /**
     * 等回答的会话读回来的屏幕：认出提问就记下。刚答过、屏幕上还是所答的那个提问时不认，隔一会儿再读；agent 进入等回答的
     * 时刻和答时记下的不同，就是新的一次等待，提问一样也照认。
     */
    private fun notePrompt(id: SessionId, screen: String) {
        val prompt = AgentPrompt.parse(screen.split('\n'))
        val answered = answeredPrompts[id]
        if (prompt != null && answered != null && answered.rereads > 0 &&
            answered.since == session(id)?.meta?.agentSinceMs && answered.asked == prompt.asked
        ) {
            answered.rereads -= 1
            prompts = prompts - id
            requestPreview(id)
            return
        }
        answeredPrompts -= id
        prompts = if (prompt == null) prompts - id else prompts + (id to prompt)
    }

    /** 读最近一条命令的输出没成（没有 shell 集成、还没跑过命令、上一条命令没有输出）：马上改读屏幕底部，去掉提示符那一行。这是同一次刷新的后半，不另算节流。 */
    private fun fallBackToBottomLines(id: SessionId) {
        send(PreviewRead.BottomLines(atPrompt = true), id)
    }

    /** 一次读屏幕读完了（出错也算）：读着的时候又要过刷新的，现在再读；那次刷新已经过了节流。 */
    private fun finishPreview(id: SessionId) {
        if (previewsAgain.remove(id)) sendPreviewRead(id)
    }

    // 事件

    internal fun handle(event: HostEvent) {
        // 帧不转：AppModel 用不上，又来得最勤。
        if (event !is HostEvent.Frame) onLinkEvent(event)
        when (event) {
            is HostEvent.State -> {
                val state = event.state
                if (!state.isConnected) listCurrent = false
                linkState = state
                if (!state.isConnected) {
                    connected = false
                    watching.clear()
                    throttle.reset()
                    pendingPreviews.clear()
                    previewsAgain.clear()
                    pendingTasks.clear()
                    listingDirs.clear()
                    pendingAgentSearch = null
                    searchedPath = null
                    shellPaths.clear()
                    pendingRenames.clear()
                    for (reply in quickReplies.values) reply.connectionLost()
                    directoryPicker?.connectionLost()
                    // 回话随这条连接一起丢了，等下去按钮会一直灰着。
                    pendingSpawn = null
                    isSpawning = false
                }
            }
            is HostEvent.Ready -> {
                connected = true
                directoryPicker?.isConnected = true
                listCurrent = false
                watching.clear()
                refresh()
                loadMissingTasks()
                findAgents()
            }
            is HostEvent.Message -> handle(event.message)
            is HostEvent.Frame -> {}
        }
    }

    private fun updateTheme(newTheme: AppTheme) {
        if (theme == newTheme) return
        theme = newTheme
        onThemeChanged()
    }

    private fun handle(message: HostMsg) {
        if (quickReplies.values.any { it.handle(message) } || directoryPicker?.handle(message) == true) return
        when (message) {
            is HostMsg.SessionList -> {
                val list = message.sessions
                val known = sessions.map { it.id }.toSet()
                listCurrent = true
                sessions = list
                phoneOwnsSize = emptySet()
                for (reply in quickReplies.values) reply.isConnected = true
                loaded = true
                val present = list.map { it.id }.toSet()
                previews = previews.filterKeys { it in present }
                answeredPrompts.keys.retainAll(present)
                for (id in prompts.keys) dropStalePrompt(id)
                shellPaths.keys.retainAll(present)
                if (searchedPath != null) findAgentsIfPathChanged()
                for (session in list) {
                    if (session.id in screens || session.id in watching) continue
                    watching += session.id
                    link.send(ClientMsg.Attach(session.id, null, AttachMode.MetaOnly))
                }
                for (session in list) {
                    if (session.id !in known || session.id !in previews) requestPreview(session.id)
                }
            }
            is HostMsg.Attached -> {
                update(message.id) { it.copy(meta = message.meta) }
                dropStalePrompt(message.id)
                noteShell(message.shell, message.id)
                message.settings?.let { updateTheme(AppTheme(it)) }
            }
            is HostMsg.ThemeApplied -> updateTheme(AppTheme(message.settings))
            is HostMsg.Meta -> {
                update(message.id) { it.copy(meta = message.meta) }
                dropStalePrompt(message.id)
                noteShell(message.shell, message.id)
                requestPreview(message.id)
            }
            is HostMsg.Bell -> requestPreview(message.id)
            is HostMsg.ScreenText -> screenText(message)
            is HostMsg.Exited -> {
                update(message.id) { it.copy(exited = true) }
                dropStalePrompt(message.id)
            }
            is HostMsg.Resync -> {
                // 读得太慢被宿主摘掉了订阅：重新只看状态，不然这个会话的状态就停在旧的上了。终端页开着的会话归终端页
                // 自己重连，这里不发，不然会顶掉它要屏幕的订阅。
                if (!connected || message.id in screens || message.id !in watching) return
                link.send(ClientMsg.Attach(message.id, null, AttachMode.MetaOnly))
            }
            is HostMsg.SizeOwner -> {
                phoneOwnsSize = if (message.mine) phoneOwnsSize + message.id else phoneOwnsSize - message.id
                update(message.id) { it.copy(sizeOwner = message.owner) }
            }
            is HostMsg.Layout -> {
                if (message.req != layoutRequest) return
                windows = message.windows
                // 布局里有还不认识的会话（电脑上刚分屏、开标签）：终端页开着时列表自己的刷新停着，这里补要一次。
                val known = sessions.map { it.id }.toSet()
                val placed = message.windows.flatMap { it.workspaces }.flatMap { it.sessions }
                if (connected && placed.any { it !in known }) link.send(ClientMsg.ListSessions)
            }
            HostMsg.LayoutChanged -> refreshLayout()
            is HostMsg.Tasks -> finishTasks(message.req, message.sources)
            is HostMsg.Commands -> {
                if (message.req != pendingAgentSearch) return
                pendingAgentSearch = null
                agents = AgentLauncher.installed(message.found)
            }
            is HostMsg.Done -> {
                if (!pendingRenames.remove(message.req)) return
                refresh()
            }
            is HostMsg.Opened -> spawned(message.req, message.id)
            is HostMsg.Spawned -> spawned(message.req, message.id)
            is HostMsg.Error -> error(message)
            else -> {}
        }
    }

    private fun screenText(message: HostMsg.ScreenText) {
        val id = message.id
        val read = pendingPreviews.remove(id) ?: PreviewRead.BottomLines(atPrompt = false)
        val session = session(id) ?: return
        val waiting = SessionGroup.of(session) == SessionGroup.Waiting
        val limit = if (waiting) waitingPreviewLines else previewLines
        when (read) {
            PreviewRead.LastCommand -> {
                val lines = Presentation.previewLines(message.text, limit)
                if (lines.isEmpty()) {
                    fallBackToBottomLines(id)
                    return
                }
                previews = previews + (id to lines)
                prompts = prompts - id
            }
            is PreviewRead.BottomLines -> {
                previews = previews + (id to Presentation.previewLines(message.text, limit, read.atPrompt))
                if (waiting) notePrompt(id, message.text) else prompts = prompts - id
            }
        }
        finishPreview(id)
    }

    private fun spawned(req: UInt, id: SessionId) {
        val pending = pendingSpawn
        if (pending == null || req != pending.req) return
        val command = pending.command
        pendingSpawn = null
        isSpawning = false
        refresh()
        if (command != null) {
            // shortcut: 不等提示符就发，靠伪终端把输入攒着等 shell 读，shell 启动时清掉预输入的话命令会丢，那时改成等
            // 这个会话的 meta 报 shell 在提示符上再发
            link.send(ClientMsg.Paste(command.paste, id, command.text))
            link.send(ClientMsg.SendKeys(command.enter, id, listOf("enter")))
        }
        onSpawned(id)
    }

    private fun error(message: HostMsg.Error) {
        val req = message.req
        val id = message.id
        if (id != null && req == null && message.message.startsWith(HostMsg.backedUpPrefix)) {
            // 宿主积压时拒掉了重新只看状态的 Attach（Resync 之后、关掉终端页时），订阅没建成：放出来让下一次 sessionList
            // 补订。读屏幕被拒也是同样的回话、分不出来，多订一次无害；不 return，预览照旧在下面收尾。
            watching -= id
        }
        val pending = pendingSpawn
        when {
            req != null && pending != null && req == pending.req -> {
                val fallback = pending.fallback
                if (fallback != null) {
                    // 电脑上的 app 没开着窗口：开一个只在后台跑的会话。
                    pending.req = fallback
                    pending.fallback = null
                    link.send(ClientMsg.Spawn(fallback, spawnSize, pending.cwd, IntegrationMode.Detect, true))
                } else {
                    pendingSpawn = null
                    isSpawning = false
                    errorMessage = UiText.res(R.string.failure_detail, pending.failure, Presentation.hostError(message))
                }
            }
            req != null && pendingRenames.remove(req) ->
                errorMessage = UiText.res(R.string.rename_workspace_failed, Presentation.hostError(message))
            req != null && req in pendingTasks -> finishTasks(req, emptyList())
            req == layoutRequest -> {
                // 电脑上没有 app 的界面连着宿主：所有会话都在后台。
                windows = emptyList()
            }
            id != null && message.message.startsWith(HostMsg.noSessionPrefix) -> {
                // 宿主说没有这个会话：别处已经结束了它。读屏幕超时这类错误不算。
                watching -= id
                sessions = sessions.filterNot { it.id == id }
                previews = previews - id
                prompts = prompts - id
                pendingPreviews -= id
                previewsAgain -= id
            }
            id != null && req == null -> {
                val read = pendingPreviews.remove(id) ?: return
                if (read == PreviewRead.LastCommand) {
                    // 读最近一条命令的输出出错（多半是没有 shell 集成）：退回读屏幕底部。
                    fallBackToBottomLines(id)
                } else {
                    finishPreview(id)
                }
            }
        }
    }

    private fun update(id: SessionId, change: (SessionInfo) -> SessionInfo) {
        val index = sessions.indexOfFirst { it.id == id }
        if (index < 0) return
        sessions = sessions.toMutableList().also { it[index] = change(it[index]) }
    }

    companion object {
        /** 预览读多少行：读回来以后去掉空行再取最后几行。 */
        internal val previewReadLines: UInt = 12u

        /** 等你回答的会话多读几行：提问连同上面的问题、下面的提示要整个读进来才认得出（[AgentPrompt]）。 */
        internal val waitingReadLines: UInt = 16u

        /** 预览显示几行：两行够看出在干什么，卡片矮一些，一屏多放几张。 */
        const val previewLines = 2

        /** 等你回答的会话的预览多显示一行，问题和选项才看得全。 */
        const val waitingPreviewLines = 3

        /**
         * `Layout` 的请求编号固定用 0：[HostLink.nextRequestId] 不发 0，回话不会和别的请求混；几次的回话都是完整的布局，
         * 后到的盖过先到的，也就不用一个个对上。
         */
        internal val layoutRequest: UInt = 0u

        /** 答过以后读到的一直是所答的那个提问时最多再读这么多次；读完还是它，按键多半没起作用，提问照旧认出来。 */
        internal const val answeredRereads = 5
    }
}
