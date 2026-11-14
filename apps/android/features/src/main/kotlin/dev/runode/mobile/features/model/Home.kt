package dev.runode.mobile.features.model

import dev.runode.mobile.connection.LinkState
import dev.runode.mobile.connection.MachineRecord
import dev.runode.mobile.features.R
import dev.runode.mobile.protocol.Agent
import dev.runode.mobile.protocol.SessionId
import dev.runode.mobile.protocol.SessionInfo
import java.util.UUID
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.launch
import kotlinx.serialization.Serializable

/**
 * 上次打开的终端：首页「上次打开」那一块，点一下回到它。标题和目录是打开时的样子，连上以后以会话列表里实时的为准。
 */
@Serializable
data class RecentTerminal(
    @Serializable(with = UuidSerializer::class) val machine: UUID,
    val session: SessionId,
    /** 会话的标题；会话没报标题时为空，显示成默认的「终端」。 */
    val title: String?,
    val directory: String?,
) {
    val displayTitle: UiText get() = title?.let(UiText::Raw) ?: UiText.res(R.string.terminal_default_title)
}

/** 首页顶上的几个数：连着的电脑上加起来。 */
data class HomeSummary(
    val waiting: Int = 0,
    val working: Int = 0,
    /** 没退出的会话。 */
    val sessions: Int = 0,
    /** 连着的电脑。 */
    val connected: Int = 0,
)

/** 首页「上次打开」那一块要显示的东西。 */
data class ResumeItem(
    val machine: MachineRecord,
    val session: SessionId,
    val title: UiText,
    val directory: String?,
    /** 连上以后会话里实时的 agent；还没连上时为空。 */
    val agent: Agent?,
)

/** 首页「等你回答」里的一个会话，带着它所在的电脑和会话列表（快速回复经那条连接发）。 */
class WaitingSession internal constructor(val list: SessionListModel, val session: SessionInfo) {
    /** 哪台电脑上的哪个会话。 */
    val id: String = "${list.machine.id}/${session.id.rawValue}"
}

/** 首页「等你回答」一次只摊开一张卡，这里定摊开哪张、怎么翻。参数里的 `ids` 是 [WaitingSession.id]，按 [AppModel.waitingSessions] 的顺序。 */
internal object HomePager {
    /** 摊开的那张：用户翻到的 [focused] 还在列表里就是它，否则（没翻过，或者它已经答完不在了）是第一张。 */
    fun current(focused: String?, ids: List<String>): String? =
        if (focused != null && focused in ids) focused else ids.firstOrNull()

    /** 从 [current] 往后翻一张，翻过最后一张回到第一张。 */
    fun next(current: String?, ids: List<String>): String? {
        val index = ids.indexOf(current ?: return ids.firstOrNull())
        if (index < 0) return ids.firstOrNull()
        return ids[(index + 1) % ids.size]
    }

    /** 从 [current] 往前翻一张，翻过第一张回到最后一张。 */
    fun previous(current: String?, ids: List<String>): String? {
        val index = ids.indexOf(current ?: return ids.firstOrNull())
        if (index < 0) return ids.firstOrNull()
        return ids[(index + ids.size - 1) % ids.size]
    }
}

/** 配对过的每台电脑的会话列表，按配对的先后。 */
val AppModel.machineLists: List<SessionListModel>
    get() = machineList.machines.mapNotNull { sessionList(it.id) }

/** 连着的电脑上的统计；断开的电脑上留着的旧列表不算。 */
val AppModel.summary: HomeSummary
    get() {
        var waiting = 0
        var working = 0
        var sessions = 0
        var connected = 0
        for (list in machineLists) {
            if (!list.linkState.isConnected) continue
            connected += 1
            for (session in list.sessions) {
                if (session.exited) continue
                sessions += 1
                when (SessionGroup.of(session)) {
                    SessionGroup.Waiting -> waiting += 1
                    SessionGroup.Working -> working += 1
                    SessionGroup.Other -> {}
                }
            }
        }
        return HomeSummary(waiting, working, sessions, connected)
    }

/**
 * 连着的电脑上等用户回答的会话，按电脑的先后、再按宿主给的先后。卡片上能直接回答，所以只认这次连上以后收到的列表：
 * 重连上、新列表还没到时，断开前在等的会话可能早答完了。
 */
val AppModel.waitingSessions: List<WaitingSession>
    get() = machineLists.filter { it.linkState.isConnected && it.listCurrent }.flatMap { list ->
        list.sessions.filter { SessionGroup.of(it) == SessionGroup.Waiting }.map { WaitingSession(list, it) }
    }

/** 上次打开的终端；那台电脑删掉了，或者连上后列表里已经没有这个会话（结束了）时为空。 */
val AppModel.resume: ResumeItem?
    get() {
        val recent = recent ?: return null
        val machine = machineList.machine(recent.machine) ?: return null
        val list = sessionList(recent.machine)
        val live = list?.session(recent.session)
        if (list != null && list.loaded && (live == null || live.exited)) return null
        return ResumeItem(
            machine = machine,
            session = recent.session,
            title = live?.let(Presentation::sessionTitle) ?: recent.displayTitle,
            directory = live?.meta?.cwd ?: recent.directory,
            agent = live?.meta?.agent,
        )
    }

/** 能新开会话的电脑：连着的。 */
val AppModel.spawnableLists: List<SessionListModel>
    get() = machineLists.filter { it.linkState.isConnected }

/** 打开上次的终端。 */
fun AppModel.resumeRecent() {
    val resume = resume ?: return
    openTerminal(resume.machine.id, resume.session)
}

/** 首页在看着的时候：每台电脑都定时刷新列表和在干活、等回答的会话的预览。 */
suspend fun AppModel.keepHomeRefreshing() {
    coroutineScope {
        for (list in machineLists) launch { list.keepRefreshing() }
    }
}

/** 下拉刷新：每台都重新要列表、刷新全部预览，断开的立刻重连。 */
fun AppModel.refreshHome() {
    for (list in machineLists) {
        when (list.linkState) {
            is LinkState.Connected -> {
                list.refresh()
                list.refreshPreviews()
            }
            is LinkState.Waiting, is LinkState.Failed -> list.reconnect()
            LinkState.Idle, LinkState.Connecting -> {}
        }
    }
}
