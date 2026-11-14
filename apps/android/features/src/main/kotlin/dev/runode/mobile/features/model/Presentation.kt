package dev.runode.mobile.features.model

import androidx.annotation.DrawableRes
import dev.runode.mobile.connection.LinkFailure
import dev.runode.mobile.connection.LinkState
import dev.runode.mobile.features.R
import dev.runode.mobile.protocol.Agent
import dev.runode.mobile.protocol.AgentKind
import dev.runode.mobile.protocol.AgentState
import dev.runode.mobile.protocol.GateRejection
import dev.runode.mobile.protocol.GoodbyeReason
import dev.runode.mobile.protocol.GridSize
import dev.runode.mobile.protocol.HostMsg
import dev.runode.mobile.protocol.PairingInvitationError
import dev.runode.mobile.protocol.SessionInfo
import dev.runode.mobile.protocol.TaskSource
import java.time.Duration
import java.time.Instant
import kotlin.math.ceil

/** 首页电脑卡片上的一行：开头 [emphasis] 一段上 [tone] 的状态色，后面接中性色的 [text]。 */
data class MachineLine(
    val tone: Tone,
    /** 上状态色的一段；[Tone.Neutral] 时为空。 */
    val emphasis: UiText? = null,
    val text: UiText,
) {
    enum class Tone {
        Waiting,
        Working,
        Failure,
        Neutral,
    }
}

/** 一家 agent 的 logo：[colored] 为真时 [drawable] 按原色画，否则它是单色的，界面按前景色染色。 */
data class AgentLogo(@param:DrawableRes val drawable: Int, val colored: Boolean)

/** agent 状态旁的时长按 [STEP_MS] 往下取整，不满一步不写；从 agent 进入这个状态的时刻起每满一步重画一次。 */
object AgentClock {
    const val STEP_MS = 60_000L

    /** [nowMs] 之后的下一个整步；[nowMs] 早于 [sinceMs] 时是 [sinceMs] 起的第一步。都是 Unix 毫秒。 */
    fun following(nowMs: Long, sinceMs: Long): Long = sinceMs + ((nowMs - sinceMs).coerceAtLeast(0) / STEP_MS + 1) * STEP_MS

    /** 在等或在干活、知道从什么时刻起时，时长会变，要按整步重画。 */
    fun ticks(agent: Agent?, sinceMs: Long?): Boolean =
        sinceMs != null && (agent?.state == AgentState.Blocked || agent?.state == AgentState.Working)
}

/** 列表、状态条上给人看的文字。和界面框架无关，单元测试能直接测。 */
object Presentation {
    /**
     * agent 的状态：在等和在干活时，[agentElapsed] 写得出时长就说等了、干了多久，写不出时是名字加「等你回答」「干活中」；
     * 空闲时是名字加「空闲」。没有 agent 或者状态不认识时为空。[sinceMs] 是 [dev.runode.mobile.protocol.SessionMeta.agentSinceMs]。
     */
    fun agentStatus(agent: Agent?, sinceMs: Long?, now: Instant): Pair<UiText, AgentState>? {
        agent ?: return null
        val name = agent.kind.displayName
        val elapsed = agentElapsed(sinceMs, now)
        val state = when (agent.state) {
            AgentState.Working -> {
                if (elapsed != null) return UiText.res(R.string.session_worked, name, elapsed) to agent.state
                R.string.agent_working
            }
            AgentState.Idle -> R.string.idle
            AgentState.Blocked -> {
                if (elapsed != null) return UiText.res(R.string.session_waited, name, elapsed) to agent.state
                R.string.agent_waiting
            }
            is AgentState.Unknown -> return null
        }
        return UiText.res(R.string.detail_separator, name, UiText.res(state)) to agent.state
    }

    /**
     * 从 [sinceMs]（Unix 毫秒）到 [now] 多久，以小时或分钟只写最大的一个单位、往下取整。没有 [sinceMs]，或者不满一个
     * [AgentClock.STEP_MS]（包括手机和电脑的钟有偏差、[sinceMs] 比 [now] 晚）时为空。
     */
    fun agentElapsed(sinceMs: Long?, now: Instant): UiText? {
        sinceMs ?: return null
        val elapsed = Duration.ofMillis(now.toEpochMilli() - sinceMs)
        if (elapsed.toMillis() < AgentClock.STEP_MS) return null
        val hours = elapsed.toHours().toInt()
        if (hours > 0) return UiText.plural(R.plurals.elapsed_hours, hours, hours)
        val minutes = elapsed.toMinutes().toInt()
        return UiText.plural(R.plurals.elapsed_minutes, minutes, minutes)
    }

    /** 目录：家目录下的写成 `~/…`。 */
    fun directory(path: String?): String? {
        if (path.isNullOrEmpty()) return null
        val parts = path.split('/')
        // /Users/<名字>/... 或 /home/<名字>/...
        if (parts.size >= 3 && parts[0].isEmpty() && (parts[1] == "Users" || parts[1] == "home")) {
            val rest = parts.drop(3).joinToString("/")
            return if (rest.isEmpty()) "~" else "~/$rest"
        }
        return path
    }

    /** 配对时列出的电脑地址：IPv4 在前，只写前两个，其余的只说还有几个；电脑的网卡多时整串地址又长又难认。 */
    internal fun pairingAddresses(addresses: List<String>, port: UShort): UiText {
        val ordered = addresses.filterNot { ':' in it } + addresses.filter { ':' in it }
        val shown = ordered.take(2).joinToString(", ") { if (':' in it) "[$it]:$port" else "$it:$port" }
        return if (ordered.size > 2) {
            UiText.plural(R.plurals.pairing_addresses_more, ordered.size, ordered.size, shown)
        } else {
            UiText.Raw(shown)
        }
    }

    /** 会话卡片上的目录：和所在工作区的目录一样时不写（节头已经写了），在它下面时写成 `./…`，别处的照 [directory] 写。 */
    fun sessionDirectory(cwd: String?, workspaceDir: String?): String? {
        if (cwd.isNullOrEmpty()) return null
        if (workspaceDir.isNullOrEmpty()) return directory(cwd)
        val base = workspaceDir.removeSuffix("/")
        if (cwd == base || cwd == "$base/") return null
        if (cwd.startsWith("$base/")) return "./" + cwd.substring(base.length + 1)
        return directory(cwd)
    }

    fun gridSize(size: GridSize): String = "${size.cols}×${size.rows}"

    /** 会话列表里尺寸由谁控制；[mine] 为真时是这部手机。 */
    fun sizeOwner(owner: String?, mine: Boolean = false): UiText = when {
        mine -> UiText.res(R.string.size_owner_named, UiText.res(R.string.this_phone))
        owner.isNullOrEmpty() -> UiText.res(R.string.size_owner_none)
        else -> UiText.res(R.string.size_owner_named, owner)
    }

    /** 终端页里尺寸由谁控制。 */
    fun sizeOwnership(ownership: SizeOwnership): UiText = when (ownership) {
        SizeOwnership.Unknown -> UiText.res(R.string.size_owner_computer)
        SizeOwnership.Mine -> UiText.res(R.string.size_owner_mine)
        is SizeOwnership.Other -> ownership.name?.let { UiText.res(R.string.size_owner_named, it) }
            ?: UiText.res(R.string.size_owner_other)
    }

    /** 连接状态的完整说法。[shownName] 是旁边已经写着的电脑名字：连上的那台报的名字和它一样时不再写一遍。 */
    fun linkState(state: LinkState, shownName: String? = null): UiText = when (state) {
        LinkState.Idle -> UiText.res(R.string.link_idle)
        LinkState.Connecting -> UiText.res(R.string.link_connecting)
        is LinkState.Connected -> if (state.hostName == shownName) {
            UiText.res(R.string.link_connected)
        } else {
            UiText.res(R.string.link_connected_to, state.hostName)
        }
        is LinkState.Waiting -> UiText.res(
            if (state.offline) R.string.link_waiting_offline else R.string.link_waiting_retry,
            linkFailure(state.reason),
        )
        is LinkState.Failed -> linkFailure(state.failure)
    }

    /** 标题下面一行短的连接状态：重连时带倒计时。 */
    fun linkStatus(state: LinkState, now: Instant = Instant.now()): UiText = when (state) {
        LinkState.Idle -> UiText.res(R.string.link_idle)
        LinkState.Connecting -> UiText.res(R.string.link_status_connecting)
        is LinkState.Connected -> UiText.res(R.string.link_connected)
        is LinkState.Waiting -> if (state.offline) {
            UiText.res(R.string.link_status_offline)
        } else {
            val seconds = ceil(Duration.between(now, state.retryAt).toNanos() / 1e9).toInt()
            if (seconds > 0) {
                UiText.plural(R.plurals.link_status_retry_in, seconds, seconds)
            } else {
                UiText.res(R.string.link_status_retrying)
            }
        }
        is LinkState.Failed -> UiText.res(R.string.link_failed)
    }

    /** 连接、配对失败的说法。 */
    fun linkFailure(failure: LinkFailure): UiText = when (failure) {
        is LinkFailure.Rejected -> when (val reason = failure.reason) {
            GateRejection.UnknownDevice -> UiText.res(R.string.failure_unknown_device)
            GateRejection.BadSignature -> UiText.res(R.string.failure_bad_signature)
            GateRejection.PairingInvalid -> UiText.res(R.string.failure_pairing_invalid)
            GateRejection.RateLimited -> UiText.res(R.string.failure_rate_limited)
            GateRejection.Disabled -> UiText.res(R.string.failure_disabled)
            is GateRejection.Unknown -> UiText.res(R.string.failure_rejected, reason.kind)
        }
        is LinkFailure.GateVersion -> UiText.res(R.string.failure_gate_version, failure.version.toInt())
        is LinkFailure.Incompatible -> UiText.res(R.string.failure_incompatible, failure.reason)
        LinkFailure.MissingKey -> UiText.res(R.string.failure_missing_key)
        LinkFailure.FingerprintMismatch -> UiText.res(R.string.failure_fingerprint_mismatch)
        LinkFailure.Timeout -> UiText.res(R.string.failure_timeout)
        LinkFailure.NoAddress -> UiText.res(R.string.failure_no_address)
        LinkFailure.InvitationExpired -> UiText.res(R.string.failure_invitation_expired)
        is LinkFailure.ProtocolViolation -> UiText.res(R.string.failure_protocol_violation, failure.detail)
        is LinkFailure.ConnectionFailed -> UiText.res(R.string.failure_connection_failed, failure.detail)
        is LinkFailure.Closed -> UiText.res(R.string.failure_closed, failure.detail)
        LinkFailure.HostClosed -> UiText.res(R.string.failure_host_closed)
        is LinkFailure.Goodbye -> when (val reason = failure.reason) {
            GoodbyeReason.Handoff -> UiText.res(R.string.failure_goodbye_handoff)
            GoodbyeReason.Shutdown -> UiText.res(R.string.failure_goodbye_shutdown)
            is GoodbyeReason.Error -> UiText.res(R.string.failure_goodbye_error, reason.message)
            is GoodbyeReason.Unknown -> UiText.res(R.string.failure_host_closed)
        }
        is LinkFailure.KeyStore -> UiText.res(R.string.failure_key_store, failure.detail)
    }

    /** 配对链接读不了的说法。 */
    fun pairingInvitationError(error: PairingInvitationError): UiText = when (error) {
        PairingInvitationError.NotAnInvitation -> UiText.res(R.string.pairing_link_invalid)
        is PairingInvitationError.UnsupportedVersion -> UiText.res(R.string.pairing_link_version, error.version)
        is PairingInvitationError.Missing -> UiText.res(R.string.pairing_link_missing, error.field)
        is PairingInvitationError.Invalid -> UiText.res(R.string.pairing_link_bad_field, error.field)
    }

    /**
     * 宿主回的错误：标了认得的原因（`HostErrorReason`）又带着路径时换成界面语言的说法，别的照旧显示宿主写的原文。
     */
    fun hostError(error: HostMsg.Error): UiText {
        val path = error.reason?.path ?: return UiText.Raw(error.message)
        val id = when (error.reason?.kind) {
            "not_found" -> R.string.host_error_not_found
            "not_a_directory" -> R.string.host_error_not_a_directory
            "permission_denied" -> R.string.host_error_permission_denied
            "not_absolute" -> R.string.host_error_not_absolute
            else -> return UiText.Raw(error.message)
        }
        return UiText.res(id, path)
    }

    /** 首页电脑卡片上的一行：连着时是会话数，有等你回答或在干活的先说它们；连不上时是「连不上」，别的没连着的状态是连接状态。 */
    fun machineLine(state: LinkState, sessions: List<SessionInfo>, loaded: Boolean, now: Instant = Instant.now()): MachineLine {
        when (state) {
            is LinkState.Connected -> {}
            is LinkState.Failed -> return MachineLine(
                MachineLine.Tone.Failure,
                UiText.res(R.string.machine_unreachable),
                UiText.Raw(""),
            )
            else -> return MachineLine(MachineLine.Tone.Neutral, text = linkStatus(state, now))
        }
        if (!loaded) return MachineLine(MachineLine.Tone.Neutral, text = UiText.res(R.string.link_connected))
        val live = sessions.filterNot { it.exited }
        if (live.isEmpty()) return MachineLine(MachineLine.Tone.Neutral, text = UiText.res(R.string.no_terminals))
        val total = UiText.plural(R.plurals.machine_sessions_total, live.size, live.size)
        val waiting = live.count { SessionGroup.of(it) == SessionGroup.Waiting }
        if (waiting > 0) {
            return MachineLine(MachineLine.Tone.Waiting, UiText.plural(R.plurals.machine_waiting, waiting, waiting), total)
        }
        val working = live.count { SessionGroup.of(it) == SessionGroup.Working }
        if (working > 0) {
            return MachineLine(MachineLine.Tone.Working, UiText.plural(R.plurals.machine_working, working, working), total)
        }
        return MachineLine(MachineLine.Tone.Neutral, text = UiText.plural(R.plurals.machine_sessions, live.size, live.size))
    }

    /** 首页的大标题：有等你回答的说等你的个数，否则有在干活的说干活的个数。 */
    fun homeTitle(summary: HomeSummary): UiText = when {
        summary.waiting > 0 -> UiText.plural(R.plurals.home_waiting, summary.waiting, summary.waiting)
        summary.working > 0 -> UiText.plural(R.plurals.home_working, summary.working, summary.working)
        summary.connected > 0 -> UiText.res(R.string.home_nothing_waiting)
        else -> UiText.res(R.string.home_no_computer)
    }

    /** 首页大标题下面那行，标题已经说全了时为空。 */
    fun homeDetail(summary: HomeSummary): UiText? = when {
        summary.waiting > 0 ->
            if (summary.working > 0) UiText.plural(R.plurals.home_more_working, summary.working, summary.working) else null
        summary.working > 0 -> null
        summary.sessions > 0 -> UiText.plural(R.plurals.home_sessions_total, summary.sessions, summary.sessions)
        summary.connected > 0 -> UiText.res(R.string.no_terminals)
        else -> null
    }

    /**
     * 会话卡片上说状态的一句。给了 [machine]（会话所在电脑的名字）时，等你回答的那句带上它。在等和在干活时，[agentElapsed]
     * 写得出时长就说等了、干了多久。[namesAgent] 为假时说 agent 的几句不带它的名字，留给前面画着 agent logo 的地方用。
     */
    fun sessionSubtitle(session: SessionInfo, machine: String? = null, now: Instant, namesAgent: Boolean = true): UiText {
        val agent = session.meta.agent?.kind?.displayName ?: ""
        val elapsed = agentElapsed(session.meta.agentSinceMs, now)
        return when (val status = SessionStatus.of(session)) {
            SessionStatus.Waiting -> when {
                machine != null && elapsed != null ->
                    if (namesAgent) UiText.res(R.string.session_waited_on, agent, machine, elapsed)
                    else UiText.res(R.string.session_waited_on_unnamed, machine, elapsed)
                machine != null ->
                    if (namesAgent) UiText.res(R.string.session_waiting_on, agent, machine)
                    else UiText.res(R.string.session_waiting_on_unnamed, machine)
                elapsed != null ->
                    if (namesAgent) UiText.res(R.string.session_waited, agent, elapsed) else UiText.res(R.string.session_waited_unnamed, elapsed)
                else -> if (namesAgent) UiText.res(R.string.session_waiting, agent) else UiText.res(R.string.session_waiting_unnamed)
            }
            SessionStatus.Working -> when {
                elapsed != null ->
                    if (namesAgent) UiText.res(R.string.session_worked, agent, elapsed) else UiText.res(R.string.session_worked_unnamed, elapsed)
                else -> if (namesAgent) UiText.res(R.string.session_working, agent) else UiText.res(R.string.session_working_unnamed)
            }
            SessionStatus.AgentIdle -> if (namesAgent) UiText.res(R.string.session_agent_idle, agent) else UiText.res(R.string.idle)
            is SessionStatus.Running -> UiText.res(R.string.session_running, status.program)
            SessionStatus.Shell -> UiText.res(R.string.idle)
            SessionStatus.Exited -> UiText.res(R.string.session_exited)
        }
    }

    /**
     * 工作区标题行上画 logo 的会话：跑着收了 logo 的 agent、shell 还没退出的，按 [SessionSection.sessions] 的先后；后台那一节
     * 为空。
     */
    fun sectionAgents(section: SessionSection): List<SessionInfo> {
        if (section.id !is SessionSection.Id.Workspace) return emptyList()
        return section.sessions.filter { session ->
            val agent = session.meta.agent ?: return@filter false
            !session.exited && agentLogo(agent.kind) != null
        }
    }

    /**
     * [agents]（[sectionAgents] 挑出来的会话）各自在干什么，带着 agent 的名字、不带时长，用「，」连成一句，给只画了 logo 的
     * 标题行读屏用；没有会话时为空。
     */
    fun sectionAgentsSummary(agents: List<SessionInfo>): UiText? = agents
        .map { sessionSubtitle(it.copy(meta = it.meta.copy(agentSinceMs = null)), now = Instant.now()) }
        .reduceOrNull { summary, phrase -> UiText.res(R.string.list_pair, summary, phrase) }

    /** 会话列表标题下面一行：连着时说经哪条路连的，没连着时是连接状态。没有地址的连接走的是跨网络的隧道。 */
    fun linkRoute(state: LinkState, now: Instant = Instant.now()): UiText {
        if (state !is LinkState.Connected) return linkStatus(state, now)
        val address = state.address ?: return UiText.res(R.string.link_route_tunnel)
        return UiText.res(if (isLocalAddress(address)) R.string.link_route_local else R.string.link_route_direct)
    }

    /**
     * 是不是局域网里的地址：IPv4 私网和链路本地、IPv6 链路本地和唯一本地，以及 mDNS 的 `.local` 名字。地址后面的网卡
     * 作用域（`%en0`）不影响判断。
     */
    internal fun isLocalAddress(address: String): Boolean {
        var bare = address.substringBefore('%').lowercase().removeSuffix(".")
        if (bare.endsWith(".local")) return true
        // IPv4 映射的 IPv6 地址按里面的 IPv4 判断。
        if (bare.startsWith("::ffff:") && '.' in bare) bare = bare.removePrefix("::ffff:")
        if (':' in bare) {
            val head = bare.split(':').first().toIntOrNull(16)?.takeIf { it in 0..0xFFFF } ?: return false
            return head and 0xffc0 == 0xfe80 || head and 0xfe00 == 0xfc00
        }
        val octets = bare.split('.').map { part -> part.toIntOrNull()?.takeIf { it in 0..255 && part.all(Char::isDigit) } }
        if (octets.size != 4 || octets.any { it == null }) return false
        val a = octets[0]!!
        val b = octets[1]!!
        return a == 10 || (a == 172 && b in 16..31) || (a == 192 && b == 168) || (a == 169 && b == 254)
    }

    /**
     * 屏幕文字的最后几行：去掉行尾空白、空行和只有制表符（分隔线、边框）的行。[atPrompt] 为真（shell 在提示符上等输入）
     * 时再去掉最后一个有字的行，那就是光标所在的提示符，信息量很低。
     */
    fun previewLines(text: String, limit: Int, atPrompt: Boolean = false): List<String> {
        val meaningful = text.split('\n').map { it.trimEnd() }
            .filter { line -> line.any { !it.isWhitespace() && it.code !in 0x2500..0x257F } }
        return (if (atPrompt) meaningful.dropLast(1) else meaningful).takeLast(limit)
    }

    /** 会话列表里一节的标题：工作区的名字，没有名字时是目录的最后一段；不在任何窗口里的是「后台」。 */
    fun sectionTitle(section: SessionSection): UiText = when (val id = section.id) {
        SessionSection.Id.Background -> UiText.res(R.string.section_background)
        is SessionSection.Id.Workspace -> when {
            !section.name.isNullOrEmpty() -> UiText.Raw(section.name)
            !section.dir.isNullOrEmpty() -> UiText.Raw(lastPathComponent(section.dir))
            else -> UiText.res(R.string.section_workspace, id.index.toInt())
        }
    }

    /** 一节标题前的图标名（`moon.zzz`、`folder`），界面换成自己的图标。 */
    fun sectionSymbol(section: SessionSection): String = when (section.id) {
        SessionSection.Id.Background -> "moon.zzz"
        is SessionSection.Id.Workspace -> "folder"
    }

    /** 一节标题下面那行小字：工作区的目录，开着不止一个窗口时带上第几个窗口；后台那一节说明它是什么。 */
    fun sectionDetail(section: SessionSection): UiText? = when (section.id) {
        SessionSection.Id.Background -> UiText.res(R.string.section_background_detail)
        is SessionSection.Id.Workspace -> {
            val window = sectionWindow(section)
            val directory = sectionDirectory(section)?.let(UiText::Raw)
            when {
                window != null && directory != null -> UiText.res(R.string.detail_separator, window, directory)
                else -> window ?: directory
            }
        }
    }

    /** 工作区的目录；后台那一节，或者目录写出来和标题一样时（家目录的工作区叫 `~`）为空。 */
    fun sectionDirectory(section: SessionSection): String? {
        if (section.id !is SessionSection.Id.Workspace) return null
        val directory = directory(section.dir) ?: return null
        return directory.takeUnless { UiText.Raw(it) == sectionTitle(section) }
    }

    /** 工作区在第几个窗口；只开着一个窗口时，或者后台那一节为空。 */
    fun sectionWindow(section: SessionSection): UiText? {
        if (section.id !is SessionSection.Id.Workspace) return null
        return section.window?.let { UiText.res(R.string.section_window, it.toInt()) }
    }

    /** 终端页标题下面那行：shell 退出或会话没了时说这个，没连上时是连接状态，否则是电脑的名字。 */
    fun terminalSubtitle(machine: String?, link: LinkState, phase: TerminalModel.Phase, now: Instant = Instant.now()): UiText =
        when (phase) {
            is TerminalModel.Phase.Exited -> UiText.res(R.string.terminal_shell_exited)
            is TerminalModel.Phase.Gone -> UiText.res(R.string.terminal_session_ended)
            else -> if (link.isConnected && machine != null) UiText.Raw(machine) else linkStatus(link, now)
        }

    fun sizePreference(preference: SizePreference): UiText = when (preference) {
        SizePreference.Automatic -> UiText.res(R.string.size_automatic)
        SizePreference.FitPhone -> UiText.res(R.string.size_fit_phone)
        SizePreference.FollowMachine -> UiText.res(R.string.size_follow_machine)
    }

    /**
     * agent 的 logo，由桌面上 `agent_logo` 用的那批 SVG 转成 VectorDrawable；靠滤镜晕色的（antigravity）VectorDrawable 画
     * 不出，用从 SVG 栅格化的各密度 PNG，SVG 改了要重出。[coloredLogos] 里的是彩色 logo，按原色画。没收 logo 的为空。
     */
    fun agentLogo(kind: AgentKind): AgentLogo? =
        agentLogos[kind.label]?.let { AgentLogo(it, colored = kind.label in coloredLogos) }

    private val coloredLogos = setOf(
        "claude", "gemini", "devin", "antigravity", "kimi", "kiro", "amp", "qwen", "junie", "trae", "code_buddy", "mistral_vibe",
        "muse",
    )

    private val agentLogos = mapOf(
        "pi" to R.drawable.agent_logo_pi, "claude" to R.drawable.agent_logo_claude, "codex" to R.drawable.agent_logo_codex,
        "gemini" to R.drawable.agent_logo_gemini, "cursor" to R.drawable.agent_logo_cursor, "devin" to R.drawable.agent_logo_devin,
        "antigravity" to R.drawable.agent_logo_antigravity, "cline" to R.drawable.agent_logo_cline,
        "mastracode" to R.drawable.agent_logo_mastracode, "open_code" to R.drawable.agent_logo_opencode,
        "github_copilot" to R.drawable.agent_logo_github_copilot, "kimi" to R.drawable.agent_logo_kimi,
        "kiro" to R.drawable.agent_logo_kiro, "amp" to R.drawable.agent_logo_amp, "grok" to R.drawable.agent_logo_grok,
        "hermes" to R.drawable.agent_logo_hermes, "kilo" to R.drawable.agent_logo_kilo, "qodercli" to R.drawable.agent_logo_qoder,
        "qwen" to R.drawable.agent_logo_qwen, "goose" to R.drawable.agent_logo_goose, "junie" to R.drawable.agent_logo_junie,
        "open_hands" to R.drawable.agent_logo_openhands, "trae" to R.drawable.agent_logo_trae,
        "code_buddy" to R.drawable.agent_logo_codebuddy, "mistral_vibe" to R.drawable.agent_logo_mistral_vibe,
        "jules" to R.drawable.agent_logo_jules, "omp" to R.drawable.agent_logo_omp, "muse" to R.drawable.agent_logo_muse,
    )

    fun sessionTitle(session: SessionInfo): UiText =
        session.meta.displayTitle?.let(UiText::Raw) ?: UiText.res(R.string.terminal_default_title)

    /**
     * 会话卡片菜单里一组命令的名字：文件名，不在会话目录里时带上它在哪一级（`Makefile · ../..`）。自己加的命令按这个项目的
     * 和通用的分。
     */
    fun taskSourceTitle(source: TaskSource, cwd: String?): UiText {
        when (source.kind) {
            TaskSource.Kind.Custom -> return UiText.res(R.string.tasks_custom)
            TaskSource.Kind.Global -> return UiText.res(R.string.tasks_global)
            else -> {}
        }
        val file = lastPathComponent(source.file)
        val dir = deletingLastPathComponent(source.file)
        if (cwd == null || cwd == dir || !cwd.startsWith(if (dir.endsWith("/")) dir else "$dir/")) return UiText.Raw(file)
        val levels = cwd.substring(dir.length).split('/').count { it.isNotEmpty() }
        return UiText.Raw("$file · ${List(levels) { ".." }.joinToString("/")}")
    }

    /** 一组命令前的图标名，界面换成自己的图标。 */
    fun taskSourceSymbol(source: TaskSource): String = when (source.kind) {
        TaskSource.Kind.Custom -> "star"
        TaskSource.Kind.Global -> "globe"
        TaskSource.Kind.Makefile -> "hammer"
        TaskSource.Kind.PackageJson -> "shippingbox"
        TaskSource.Kind.Unknown -> "terminal"
    }

    /** 界面上写的程序名：Windows 上的进程名带着 `.exe`，写成 `powershell` 而不是 `powershell.exe`。 */
    internal fun programLabel(name: String): String =
        if (name.length > 4 && name.lowercase().endsWith(".exe")) name.dropLast(4) else name

    /** 菜单里命令那一节的标题：终端结束了时说明为什么点不了，前台在跑别的程序时说明命令会在新终端里跑。 */
    fun tasksHeader(session: SessionInfo): UiText {
        if (session.exited) return UiText.res(R.string.tasks_run_ended)
        if (session.meta.foregroundIsShell) return UiText.res(R.string.tasks_run)
        val foreground = session.meta.foreground
        if (!foreground.isNullOrEmpty()) return UiText.res(R.string.tasks_run_busy, programLabel(foreground))
        return UiText.res(R.string.tasks_run_new)
    }

    /** 路径的最后一段，末尾的 `/` 不算；根目录是 `/`。 */
    private fun lastPathComponent(path: String): String {
        val trimmed = path.trimEnd('/')
        if (trimmed.isEmpty()) return if (path.isEmpty()) "" else "/"
        return trimmed.substringAfterLast('/')
    }

    /** 去掉最后一段的路径，末尾的 `/` 不算；只剩根目录时是 `/`，没有 `/` 时为空。 */
    private fun deletingLastPathComponent(path: String): String {
        val trimmed = path.trimEnd('/')
        val slash = trimmed.lastIndexOf('/')
        return when {
            slash < 0 -> ""
            slash == 0 -> "/"
            else -> trimmed.substring(0, slash)
        }
    }
}
