package dev.runode.mobile.features.model

import android.content.Context
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import dev.runode.mobile.connection.DeviceKeyStore
import dev.runode.mobile.connection.HostEvent
import dev.runode.mobile.connection.HostLink
import dev.runode.mobile.connection.LinkFailure
import dev.runode.mobile.connection.LinkState
import dev.runode.mobile.connection.MachineRecord
import dev.runode.mobile.connection.MachineStore
import dev.runode.mobile.connection.Pairing
import dev.runode.mobile.connection.PushRelay
import dev.runode.mobile.connection.UnlockPasswordStore
import dev.runode.mobile.features.R
import dev.runode.mobile.protocol.ClientMsg
import dev.runode.mobile.protocol.GateRejection
import dev.runode.mobile.protocol.HostMsg
import dev.runode.mobile.protocol.SessionId
import dev.runode.mobile.protocol.SessionLink
import dev.runode.mobile.protocol.TermSettings
import java.io.File
import java.net.URI
import java.util.UUID
import kotlin.time.Duration
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/** 导航栈里的一页。 */
sealed class Route {
    /** 这一页属于哪台电脑。 */
    abstract val machine: UUID

    /** 一台电脑的会话列表。 */
    data class Machine(override val machine: UUID) : Route()

    /** 一个会话的终端页。 */
    data class Terminal(override val machine: UUID, val session: SessionId) : Route()

    /** 一个会话所在仓库的 Git 页。 */
    data class Git(override val machine: UUID, val session: SessionId) : Route()

    /** 一台电脑的远程桌面。 */
    data class Screen(override val machine: UUID) : Route()
}

/**
 * App 用到的外部依赖，由 App 入口组装，测试换成假的。[recents]、[preferences]、[themes] 不给时都只记在内存里（测试、
 * 演示模式）。
 */
class AppDependencies(
    val store: MachineStore,
    val keyStore: DeviceKeyStore,
    val pairing: Pairing,
    /** 给一台电脑建连接。 */
    val makeLink: (MachineRecord) -> HostLink,
    /** 系统给的设备名；用户在设置里起了名字时报那个。 */
    val deviceName: String,
    /** 上次打开的终端记在哪里。 */
    val recents: DefaultsStore<RecentTerminal> = DefaultsStore("recentTerminal"),
    /** 设置存在哪里。 */
    val preferences: DefaultsStore<AppPreferences> = DefaultsStore("preferences"),
    /** 上次用的主题记在哪里，App 刚启动、电脑还没连上时先用它，用浅色主题的不会先闪一下默认的深色。 */
    val themes: DefaultsStore<AppTheme> = DefaultsStore("theme"),
) {
    /** 通知的系统接口；为空时不登记推送、不清通知（测试、演示模式，以及没带 Firebase 配置的构建）。 */
    var notifications: PushNotifications? = null

    /** 登记推送时报给电脑的 App 身份，App 入口按 applicationId 定（[PushIdentity.of]）；为空时不登记推送。 */
    var pushIdentity: PushIdentity? = null

    /** 官方 App 换推送句柄的中转服务；为空时直接把推送的 token 交给电脑。 */
    var pushRelay: PushRelay? = null

    /** 连上一台电脑的远程桌面；为空时没有远程桌面的入口（测试、演示模式）。 */
    var openScreen: ScreenOpener? = null

    /** 远程桌面里解锁电脑用的登录密码存在哪里；为空时每次都要用户输。 */
    var unlockPasswords: UnlockPasswordStore? = null

    /** 取、存登录密码前请用户验证身份，由 Activity 装上、离开时摘掉；为空时验证不了，存着的密码取不出来。 */
    var authenticate: UnlockAuthenticator? = null

    /** 连上一台电脑的识别；为空时语音输入只在手机上识别（测试、演示模式）。 */
    var openSpeech: SpeechOpener? = null

    /**
     * 手机上的语音模型下载到哪里；不给时放在临时目录里（测试、演示模式）。模型有几百 MB，放在 `noBackupFilesDir` 下，不进
     * 系统的备份。
     */
    var speechModels: File? = null

    /** Application 的 Context，语音输入用它录麦克风、查麦克风权限；为空时语音输入报没有能用的麦克风（测试、演示模式）。 */
    var appContext: Context? = null
}

/**
 * 整个 App 的状态：导航栈、各页的视图模型、每台电脑一条连接。配对过的每台电脑在 App 处于前台时都连着，首页据此显示
 * 各台的状态、等回答的会话和统计；App 进后台时全部断开，回到前台再连上、各页重新 `Attach`。在主线程上用。
 *
 * agent 等回答时由电脑经中转、FCM 推通知，手机不用连着：每台电脑连上时由 [PushRegistration] 登记推送。已经不等回答的
 * 会话的通知由 [clearAnsweredNotifications] 清掉。
 */
class AppModel(private val dependencies: AppDependencies) {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    private var pathValue by mutableStateOf<List<Route>>(emptyList())
    var path: List<Route>
        get() = pathValue
        set(value) {
            pathValue = value
            pathChanged()
        }

    private var pairingValue by mutableStateOf<PairingModel?>(null)

    /** 配对页开着时的视图模型。换掉、关掉时撤掉它正在跑的配对。 */
    var pairing: PairingModel?
        get() = pairingValue
        set(value) {
            val old = pairingValue
            pairingValue = value
            if (old !== value) old?.cancel()
        }

    /** 设置页开着。 */
    var showingSettings by mutableStateOf(false)

    /** 在设置页里点了「配对新电脑」：设置页关掉以后再打开配对页，两个页面不叠着弹。 */
    var pairsAfterSettings by mutableStateOf(false)

    val machineList = MachineListModel(dependencies.store, dependencies.keyStore)
    val settings = SettingsModel(dependencies.preferences, dependencies.deviceName)

    /** 向各台电脑登记推送，设置页按它显示哪台电脑不支持。 */
    val push = PushRegistration(
        dependencies.notifications,
        dependencies.pushIdentity,
        dependencies.pushRelay,
        enabled = settings.preferences.alertsBlockedAgents,
    )
    val dictation = DictationModel(
        dependencies.speechModels ?: File(System.getProperty("java.io.tmpdir"), "speech-models"),
        dependencies.openSpeech,
        dependencies.appContext,
    )

    /** 上次打开的终端，首页的「上次打开」用它。 */
    var recent by mutableStateOf(dependencies.recents.load())
        private set

    /** 上次用的主题，电脑还没连上时用它。 */
    private var savedTheme by mutableStateOf(dependencies.themes.load())

    private var sessionLists by mutableStateOf<Map<UUID, SessionListModel>>(emptyMap())

    /** 发出去、在等电脑回话的解除配对（`ClientMsg.Unpair`），按电脑。 */
    private val unpairing = mutableMapOf<UUID, Pair<UInt, CompletableDeferred<Boolean>>>()
    private val terminals = mutableMapOf<Route, TerminalModel>()
    private val gits = mutableMapOf<Route, GitModel>()
    private val screens = mutableMapOf<Route, ScreenModel>()

    /** 终端页上切过去的分屏：键是栈里的终端页，值是它现在显示的会话。切分屏不改导航栈。 */
    private var panes by mutableStateOf<Map<Route, SessionId>>(emptyMap())

    /** 终端页上显示过的会话，最近的在最后；分屏结束时按它切回上次看的那个。 */
    private val shownHistory = mutableListOf<SessionId>()
    private var active = true

    /** 系统交来、要等电脑列表读进来才能打开的会话链接（冷启动时）。 */
    private var pendingLink: SessionLink? = null

    /** 正在查通知栏的电脑，查完之前列表又变了不再查一次：查完时按那时的列表筛。 */
    private val clearingNotifications = mutableSetOf<UUID>()

    init {
        machineList.willDelete = { forget(it) }
        machineList.didLoad = { syncConnections() }
        settings.deviceNameDidChange = { deviceNameChanged(it) }
        settings.alertsDidChange = { push.enabled = it }
    }

    /** 打开一个终端页：在这台电脑的会话列表上时压在它上面，别处（首页、别的电脑）打开时连同它的会话列表一起换上，返回时先回到列表。 */
    fun openTerminal(machine: UUID, session: SessionId) {
        val route = Route.Terminal(machine, session)
        // 栈顶已经是这个终端页、只是切到了别的分屏时，回到它本身。
        panes = panes - route
        push(route, machine, ontoStack = path.lastOrNull() == Route.Machine(machine))
    }

    /** 打开这台电脑的页 [route]：[ontoStack] 为真时压在当前页上面，否则连同这台电脑的会话列表一起换上，返回时先回到列表。 */
    private fun push(route: Route, machine: UUID, ontoStack: Boolean) {
        path = if (ontoStack) path + route else listOf(Route.Machine(machine), route)
    }

    /**
     * 终端页上切到电脑上同一个标签里的另一个分屏：栈顶的终端页原地改显示这个会话，导航栈不动，返回时照旧回到列表。旧
     * 终端关掉；终端视图接着用，键盘开着就还开着。
     */
    fun switchTerminal(machine: UUID, session: SessionId) {
        val last = path.lastOrNull() as? Route.Terminal ?: return
        if (last.machine != machine) return
        val current = shown(last)
        if (current == Route.Terminal(machine, session)) return
        val keyboard = terminals[current]?.keyboardVisible ?: false
        panes = if (session == last.session) panes - last else panes + (last to session)
        pathChanged()
        // 先记上，底部的按键栏这一帧就不出来。
        terminal(machine, session)?.keyboardVisible = keyboard
    }

    /**
     * 终端页上显示的分屏 [session] 结束了：切回 [tabMates]（它所在标签里的分屏，结束前最后一次知道的）里上次看过、还
     * 没结束的那个，都没看过时切到第一个还没结束的；一个都没有时留在这页，盖着印章。
     */
    fun leaveEndedPane(machine: UUID, session: SessionId, tabMates: List<SessionId>) {
        val list = sessionList(machine) ?: return
        val alive = tabMates.filter { it != session && list.session(it)?.exited == false }
        val target = shownHistory.lastOrNull { it in alive } ?: alive.firstOrNull() ?: return
        switchTerminal(machine, target)
    }

    /** 栈里这个终端页现在显示的会话：切过分屏的是切到的那个。 */
    fun shownSession(machine: UUID, session: SessionId): SessionId = panes[Route.Terminal(machine, session)] ?: session

    private fun shown(route: Route): Route {
        if (route !is Route.Terminal) return route
        return Route.Terminal(route.machine, shownSession(route.machine, route.session))
    }

    /** 打开一个会话所在仓库的 Git 页，压在当前页上面：从终端页打开时返回回到终端，从会话列表打开时回到列表；别的电脑上的页面先换成这台电脑的会话列表。 */
    fun openGit(machine: UUID, session: SessionId) {
        val route = Route.Git(machine, session)
        if (path.lastOrNull() == route) return
        push(route, machine, ontoStack = path.firstOrNull() == Route.Machine(machine))
    }

    fun git(machine: UUID, session: SessionId): GitModel? {
        val route = Route.Git(machine, session)
        gits[route]?.let { return it }
        val list = sessionList(machine) ?: return null
        val model = GitModel(session, list.link)
        gits[route] = model
        model.open()
        return model
    }

    /** 有没有远程桌面可开。 */
    val canOpenScreen: Boolean get() = dependencies.openScreen != null

    /** 打开一台电脑的远程桌面，压在它的会话列表上面。 */
    fun openScreen(machine: UUID) {
        val route = Route.Screen(machine)
        if (path.lastOrNull() == route) return
        push(route, machine, ontoStack = path.firstOrNull() == Route.Machine(machine))
    }

    fun screen(machine: UUID): ScreenModel? {
        val route = Route.Screen(machine)
        screens[route]?.let { return it }
        val open = dependencies.openScreen ?: return null
        val record = machineList.machine(machine) ?: return null
        val model = ScreenModel(record, open, dependencies.unlockPasswords)
        // 验证要 Activity，远程桌面页开着时 Activity 可能换过，每次验证时再取。
        model.authenticate = { cipher, reason -> dependencies.authenticate?.invoke(cipher, reason) }
        screens[route] = model
        return model
    }

    /**
     * 系统交来的 `runode://` 链接：`runode://pair?…` 打开配对页，`runode://session?…` 打开那台电脑上那个会话的终端页。
     * 别的不认。
     */
    fun open(url: URI) {
        if (url.scheme?.lowercase() != "runode") return
        when (url.host?.lowercase()) {
            "pair" -> startPairing(url.toString())
            SessionLink.host -> SessionLink(url)?.let(::open)
        }
    }

    /** 打开那台电脑上那个会话的终端页：点开了等回答的通知，或者系统交来了会话链接。 */
    fun open(link: SessionLink) {
        pendingLink = link
        openPendingLink()
    }

    /**
     * 打开记下的会话链接。电脑列表还没读进来时先留着，读进来以后（[syncConnections]）再开；不认识的电脑、写法不对的会话
     * 编号就算了。
     */
    private fun openPendingLink() {
        if (!machineList.loaded) return
        val link = pendingLink ?: return
        pendingLink = null
        if (machineList.machine(link.machine) == null) return
        val session = SessionId(link.session) ?: return
        showingSettings = false
        pairsAfterSettings = false
        // 配对页盖在整个导航栈上，不关掉就看不见打开的终端。
        pairing = null
        openTerminal(link.machine, session)
    }

    /**
     * 打开配对页；[link] 是从别处（比如系统打开的 `runode://pair` 链接）带来的配对链接，只填进输入框，由用户看过要配的
     * 电脑后点「配对」：任何网页、短信都能发这种链接，自动配对会让手机连上别人的主机。
     */
    fun startPairing(link: String? = null) {
        val model = PairingModel(dependencies.pairing, settings.deviceName) { machineList.add(it) }
        if (link != null) model.linkText = link
        pairing = model
    }

    fun sessionList(machineId: UUID): SessionListModel? {
        sessionLists[machineId]?.let { return it }
        val machine = machineList.machine(machineId) ?: machineList.leaving.firstOrNull { it.id == machineId }
            ?: return null
        val model = SessionListModel(machine, dependencies.makeLink(machine))
        model.onSpawned = { openTerminal(machineId, it) }
        model.onThemeChanged = { saveTheme() }
        model.onSessionsChanged = { clearAnsweredNotifications(model) }
        model.onLinkEvent = { linkEvent(it, model) }
        sessionLists = sessionLists + (machineId to model)
        model.link.setFocused(path.any { it.machine == machineId })
        if (active) model.start()
        return model
    }

    fun terminal(machine: UUID, session: SessionId): TerminalModel? {
        val route = Route.Terminal(machine, session)
        terminals[route]?.let { return it }
        val list = sessionList(machine) ?: return null
        val info = list.session(session)
        // 列表里知道这个会话有没有前端在决定尺寸（电脑上的窗口在显示它）；不在列表里时连上再看。
        val hint = when {
            info == null -> SizeOwnerHint.Unknown
            info.sizeOwner == null -> SizeOwnerHint.None
            else -> SizeOwnerHint.Someone
        }
        val model = TerminalModel(
            sessionId = session,
            title = info?.meta?.displayTitle?.let(UiText::Raw) ?: UiText.res(R.string.terminal_default_title),
            agent = info?.meta?.agent,
            link = list.link,
            linkState = list.linkState,
            ownerHint = hint,
            sizePreference = settings.preferences.defaultSize,
            onOpen = list::screenOpened,
            // 终端页回答行的选项取自 SessionListModel.prompts，屏幕变了要它重读。
            onAnswerScreenChange = list::requestPreview,
            onAnswered = list::answered,
            onClose = list::screenClosed,
        )
        terminals[route] = model
        if (active) model.open()
        return model
    }

    /**
     * 终端页以外的界面用的主题：上次打开的终端所在的那台电脑的，它还没报主题时用配对列表里第一台报了的，都没有时用上次
     * 记下的；从没连上过电脑时用 runode 默认的深色主题，和终端页没设主题时一样。几台电脑主题不同时，进出会话列表也不换
     * 主题，免得翻页时整个界面变色。
     */
    val theme: AppTheme get() = reportedTheme ?: savedTheme ?: AppTheme(TermSettings.default)

    /** 电脑报来的主题，按 [theme] 说的先后挑；还没有电脑报过时为空。 */
    private val reportedTheme: AppTheme?
        get() {
            val order = listOfNotNull(recent?.machine) + machineList.machines.map { it.id }
            return order.firstNotNullOfOrNull { sessionLists[it]?.theme }
        }

    private fun saveTheme() {
        val theme = reportedTheme ?: return
        if (theme == savedTheme) return
        savedTheme = theme
        dependencies.themes.save(theme)
    }

    /** App 回到前台或进了后台。后台里连接迟早被系统断掉，主动断开干净；回来时重连。 */
    fun setActive(isActive: Boolean) {
        if (isActive == active) return
        active = isActive
        for (list in sessionLists.values) {
            if (isActive) list.start() else list.stop()
        }
        // 远程桌面开着时电脑一直在采集，退到后台就断开，回来再连。
        for (model in screens.values) {
            if (isActive) model.start() else model.close()
        }
        // 在后台时打开的终端页（点通知、系统交来链接比回到前台先到）只建了模型、没订阅连接。
        if (isActive) {
            for (model in terminals.values) model.open()
        }
    }

    /** 一台电脑的连接事件：连上时登记推送，断开时不再等它的回话，消息里有登记的回话。 */
    private fun linkEvent(event: HostEvent, list: SessionListModel) {
        val id = list.machine.id
        when (event) {
            is HostEvent.Ready -> {
                push.connected(list.machine, list.link)
                if (list.machine.deletedAt != null) scope.launch { finishDeleting(id) }
            }
            is HostEvent.State -> {
                val state = event.state
                val failure = (state as? LinkState.Failed)?.failure
                if (failure == LinkFailure.Rejected(GateRejection.UnknownDevice) && list.machine.deletedAt != null) {
                    // 电脑那边撤销了这部手机，撤销时连推送登记一起删了，用不着再注销；不再连了，留着只剩一把私钥。
                    push.disconnected(id)
                    scope.launch { machineList.purge(id) }
                } else if (!state.isConnected) {
                    push.disconnected(id)
                    unpairing[id]?.let { answerUnpair(id, it.first, agreed = false) }
                }
            }
            is HostEvent.Message -> {
                val message = event.message
                push.handle(message, id)
                when (message) {
                    is HostMsg.Done -> answerUnpair(id, message.req, agreed = true)
                    is HostMsg.Error -> message.req?.let { answerUnpair(id, it, agreed = false) }
                    else -> {}
                }
            }
            is HostEvent.Frame -> {}
        }
    }

    /**
     * 前台、连着这台电脑、这次连上以后收到过列表时，从通知栏清掉这台电脑上已经不等回答（或者没了）的会话的通知：电脑撤
     * 不回发出去的通知。
     */
    private fun clearAnsweredNotifications(list: SessionListModel) {
        val machine = list.machine.id
        val system = dependencies.notifications ?: return
        if (!current(list) || machine in clearingNotifications) return
        clearingNotifications += machine
        scope.launch {
            val delivered = try {
                system.delivered()
            } finally {
                clearingNotifications -= machine
            }
            if (!current(list)) return@launch
            val answered = delivered.filter { notification ->
                notification.link.machine == machine &&
                    SessionId(notification.link.session)?.let(list::session)?.let { SessionGroup.of(it) } !=
                    SessionGroup.Waiting
            }
            if (answered.isNotEmpty()) system.removeDelivered(answered.map { it.id })
        }
    }

    /** 能拿这台电脑的列表判断哪些会话还在等：App 在前台、连着、这次连上以后收到过列表。 */
    private fun current(list: SessionListModel): Boolean = active && list.linkState.isConnected && list.listCurrent

    /**
     * 导航栈变了或切了分屏：退出去的终端页、Git 页关掉，新打开的终端记作「上次打开」。会话列表的连接不跟导航栈走，只是
     * 栈里有页面的电脑算在看（`HostLink.setFocused`），连不上时重试得勤一些。
     */
    private fun pathChanged() {
        val path = path
        panes = panes.filterKeys { it in path }
        val live = path.map(::shown).toSet()
        closeAbsent(terminals, live) { it.close() }
        closeAbsent(gits, live) { it.close() }
        closeAbsent(screens, live) { it.close() }
        val top = path.lastOrNull()?.let(::shown)
        if (top is Route.Terminal) {
            remember(top.machine, top.session)
            shownHistory.remove(top.session)
            shownHistory += top.session
            if (shownHistory.size > shownHistoryLimit) shownHistory.removeAt(0)
        }
        val viewed = path.map { it.machine }.toSet()
        for ((id, list) in sessionLists) list.link.setFocused(id in viewed)
    }

    /** 关掉、拿走 [models] 里不在 [live] 中的页。 */
    private fun <Model> closeAbsent(models: MutableMap<Route, Model>, live: Set<Route>, close: (Model) -> Unit) {
        val absent = models.keys.filterNot { it in live }
        for (route in absent) models.remove(route)?.let(close)
    }

    /** 配对过的电脑变了（配对、改名、删除之后重新读了列表）：每台都有一条连接，删掉的断开，改了名字的换上新记录。 */
    private fun syncConnections() {
        // 删掉了、还没注销推送的也连着，连上了好注销。
        val machines = machineList.machines + machineList.leaving
        val present = machines.map { it.id }.toSet()
        for ((id, list) in sessionLists) {
            if (id in present) continue
            list.stop()
            sessionLists = sessionLists - id
            push.forget(id)
        }
        for (machine in machines) {
            val list = sessionLists[machine.id]
            if (list != null) list.machine = machine else sessionList(machine.id)
            push.renamed(machine)
        }
        openPendingLink()
    }

    private fun remember(machine: UUID, session: SessionId) {
        val info = sessionLists[machine]?.session(session)
        val recent = recent
        val previous = recent?.takeIf { it.machine == machine && it.session == session }
        val entry = RecentTerminal(
            machine = machine,
            session = session,
            title = if (info != null) info.meta.displayTitle else previous?.title,
            directory = info?.meta?.cwd ?: previous?.directory,
        )
        if (entry == recent) return
        this.recent = entry
        dependencies.recents.save(entry)
        saveTheme()
    }

    /** 设置页关掉了：在里面点过「配对新电脑」的，这时打开配对页。 */
    fun settingsDismissed() {
        if (!pairsAfterSettings) return
        pairsAfterSettings = false
        startPairing()
    }

    /** 设备名改了：各台电脑的连接下次连上时报新名字。新建的连接由 [AppDependencies.makeLink] 按存着的设置取名字。 */
    private fun deviceNameChanged(name: String) {
        for (list in sessionLists.values) {
            val link = list.link
            scope.launch { link.setDeviceName(name) }
        }
    }

    /** 要删掉的电脑：先退出它的页面；连着的话注销推送，限时等电脑回话再断开连接。电脑那边注销好了时为真。 */
    private suspend fun forget(id: UUID): Boolean {
        path = path.filterNot { it.machine == id }
        val unregistered = leave(id, timeout = 2.seconds)
        push.forget(id)
        sessionLists[id]?.stop()
        sessionLists = sessionLists - id
        return unregistered
    }

    /** 请电脑忘掉这部手机：先请它解除配对（推送登记一起删），电脑不认得这条请求、没回话时退回只注销推送。电脑那边办好了（或者本来就没登记推送）时为真。 */
    private suspend fun leave(id: UUID, timeout: Duration): Boolean {
        if (unpair(id, timeout)) return true
        return push.unregister(id, timeout)
    }

    /** 连着这台电脑时请它解除配对，等回话最多 [timeout]；回了 `Done` 时为真。没连着时不发、不等。 */
    private suspend fun unpair(id: UUID, timeout: Duration): Boolean {
        val list = sessionLists[id] ?: return false
        if (!list.linkState.isConnected) return false
        val req = list.link.nextRequestId()
        val reply = CompletableDeferred<Boolean>()
        unpairing[id]?.let { answerUnpair(id, it.first, agreed = false) }
        unpairing[id] = req to reply
        list.link.send(ClientMsg.Unpair(req))
        val timer = scope.launch {
            delay(timeout)
            answerUnpair(id, req, agreed = false)
        }
        return try {
            reply.await()
        } finally {
            timer.cancel()
        }
    }

    /** 解除配对 [req] 有了结果：电脑回了话、断开了或者等不及了。 */
    private fun answerUnpair(id: UUID, req: UInt, agreed: Boolean) {
        val pending = unpairing[id] ?: return
        if (pending.first != req) return
        unpairing.remove(id)
        pending.second.complete(agreed)
    }

    /** 删掉时没注销成推送的电脑（`MachineRecord.deletedAt`）又连上了：注销好了才真删掉。 */
    private suspend fun finishDeleting(id: UUID) {
        if (!leave(id, timeout = 5.seconds)) return
        machineList.purge(id)
    }

    /**
     * 推来的这条通知出不出：删掉了的、不认识的电脑的不出，电脑列表还没读进来时照出；App 在前台、终端页正显示着这个会话
     * 时也不出。
     */
    fun showsNotification(link: SessionLink): Boolean {
        if (machineList.loaded && machineList.machine(link.machine) == null) return false
        val top = path.lastOrNull() as? Route.Terminal ?: return true
        val shown = panes[top] ?: top.session
        return !(active && top.machine == link.machine && shown == SessionId(link.session))
    }

    private companion object {
        /** [shownHistory] 最多记几个会话。 */
        const val shownHistoryLimit = 32
    }
}
