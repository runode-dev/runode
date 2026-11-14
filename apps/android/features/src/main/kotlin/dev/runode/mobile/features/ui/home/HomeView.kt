package dev.runode.mobile.features.ui.home

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.layout.layout
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.onClick
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.zIndex
import dev.runode.mobile.connection.LinkState
import dev.runode.mobile.connection.MachineRecord
import dev.runode.mobile.features.R
import dev.runode.mobile.features.model.AppModel
import dev.runode.mobile.features.model.HomePager
import dev.runode.mobile.features.model.HomeSummary
import dev.runode.mobile.features.model.InstalledAgent
import dev.runode.mobile.features.model.MachineLine
import dev.runode.mobile.features.model.Presentation
import dev.runode.mobile.features.model.ResumeItem
import dev.runode.mobile.features.model.Route
import dev.runode.mobile.features.model.SessionListModel
import dev.runode.mobile.features.model.SessionSection
import dev.runode.mobile.features.model.SessionStatus
import dev.runode.mobile.features.model.WaitingSession
import dev.runode.mobile.features.model.keepHomeRefreshing
import dev.runode.mobile.features.model.refreshHome
import dev.runode.mobile.features.model.resume
import dev.runode.mobile.features.model.resumeRecent
import dev.runode.mobile.features.model.spawnableLists
import dev.runode.mobile.features.model.summary
import dev.runode.mobile.features.model.waitingSessions
import dev.runode.mobile.features.ui.AppIcons
import dev.runode.mobile.features.ui.CircleIconButton
import dev.runode.mobile.features.ui.ConfirmDialog
import dev.runode.mobile.features.ui.DisclosureChevron
import dev.runode.mobile.features.ui.ErrorDialog
import dev.runode.mobile.features.ui.LeadingTitle
import dev.runode.mobile.features.ui.ListSectionHeader
import dev.runode.mobile.features.ui.PreviewTone
import dev.runode.mobile.features.ui.QuickReplyBar
import dev.runode.mobile.features.ui.ScreenPadding
import dev.runode.mobile.features.ui.ScreenPreview
import dev.runode.mobile.features.ui.SpawnKindItems
import dev.runode.mobile.features.ui.TopBar
import dev.runode.mobile.features.ui.agentClock
import dev.runode.mobile.features.ui.linkClock
import dev.runode.mobile.features.ui.theme.ActionKind
import dev.runode.mobile.features.ui.theme.BlinkingCursor
import dev.runode.mobile.features.ui.theme.CornerRadius
import dev.runode.mobile.features.ui.theme.Shapes
import dev.runode.mobile.features.ui.theme.StatusGlyph
import dev.runode.mobile.features.ui.theme.StatusMark
import dev.runode.mobile.features.ui.theme.SurfaceButton
import dev.runode.mobile.features.ui.theme.Type
import dev.runode.mobile.features.ui.theme.cardBackground
import dev.runode.mobile.features.ui.theme.inlineCode
import dev.runode.mobile.features.ui.theme.resolve
import dev.runode.mobile.features.ui.theme.themeColors
import dev.runode.mobile.terminal.TerminalView
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

/**
 * 首页：顶上一句话说有几个会话在等你，接着是等你回答的会话（一次摊开一张，带快速回复）、配对过的电脑和上次打开的终端，
 * 底下钉着配对和新开终端两个按钮。看着首页时每台电脑都定时刷新（[keepHomeRefreshing]）。
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HomeView(app: AppModel) {
    val machines = app.machineList
    val colors = themeColors
    val context = LocalContext.current
    var pageSize by remember { mutableStateOf(IntSize.Zero) }
    var focusedWaiting by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(machines.machines.map { it.id }) { app.keepHomeRefreshing() }

    Column(Modifier.fillMaxSize().background(colors.page).onSizeChanged { pageSize = it }) {
        TopBar(
            actions = {
                CircleIconButton(AppIcons.settings, stringResource(R.string.settings), { app.showingSettings = true })
            },
        ) { LeadingTitle("Runode", showsMark = true) }
        if (machines.machines.isEmpty()) {
            if (machines.loaded) Onboarding(Modifier.weight(1f)) { app.startPairing() }
        } else {
            PullToRefreshBox(isRefreshing = false, onRefresh = { app.refreshHome() }, modifier = Modifier.weight(1f)) {
                LazyColumn(
                    Modifier.fillMaxSize(),
                    contentPadding = PaddingValues(horizontal = ScreenPadding, vertical = 4.dp),
                    verticalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    item(key = "headline") { HomeHeadline(app.summary, Modifier.padding(bottom = 8.dp)) }
                    val waiting = app.waitingSessions
                    val ids = waiting.map { it.id }
                    val current = HomePager.current(focusedWaiting, ids)
                    val index = ids.indexOf(current)
                    if (current != null && index >= 0) {
                        item(key = "waiting") {
                            val item = waiting[index]
                            WaitingStack(
                                item = item,
                                showsMachine = machines.machines.size > 1,
                                position = index + 1,
                                count = waiting.size,
                                onOpen = { app.openTerminal(item.list.machine.id, item.session.id) },
                                onNext = { focusedWaiting = HomePager.next(current, ids) },
                                onPrevious = { focusedWaiting = HomePager.previous(current, ids) },
                            )
                        }
                    }
                    item(key = "machines-header") { ListSectionHeader(stringResource(R.string.home_computers)) }
                    item(key = "machines") { MachineGroup(app) }
                    app.resume?.let { resume ->
                        item(key = "resume-header") { ListSectionHeader(stringResource(R.string.home_last_opened)) }
                        item(key = "resume") {
                            val live = app.sessionList(resume.machine.id)?.session(resume.session)
                            ResumeCard(
                                item = resume,
                                mark = live?.let { StatusMark.of(SessionStatus.of(it)) } ?: StatusMark.Idle,
                                status = live?.let {
                                    Presentation.sessionSubtitle(it, now = agentClock(it.meta.agent, it.meta.agentSinceMs)).resolve()
                                },
                                onOpen = { app.resumeRecent() },
                            )
                        }
                    }
                }
            }
            ActionBar(app) { list ->
                if (pageSize != IntSize.Zero) {
                    list.spawnSize = TerminalView.gridSize(
                        context, pageSize.width, pageSize.height, app.settings.preferences.fontSize?.toFloat(),
                    )
                }
            }
        }
    }

    // 对话框关掉时就离开了组合，改名、删除要在首页的作用域里跑完。
    val scope = rememberCoroutineScope()
    RenameDialog(app, scope)
    DeleteDialog(app, scope)
    val spawnErrorList = if (app.path.isEmpty()) app.machineLists().firstOrNull { it.errorMessage != null } else null
    val machineError = if (app.showingSettings) null else machines.errorMessage
    ErrorDialog((machineError ?: spawnErrorList?.errorMessage)?.resolve()) {
        if (machineError != null) machines.errorMessage = null else spawnErrorList?.errorMessage = null
    }
}

private fun AppModel.machineLists(): List<SessionListModel> = machineList.machines.mapNotNull { sessionList(it.id) }

/** 等你回答的那张卡，不止一个在等时底下露出一截后一张的卡边。 */
@Composable
private fun WaitingStack(
    item: WaitingSession,
    showsMachine: Boolean,
    position: Int,
    count: Int,
    onOpen: () -> Unit,
    onNext: () -> Unit,
    onPrevious: () -> Unit,
) {
    val colors = themeColors
    Column {
        WaitingCard(item, showsMachine, position, count, onOpen, onNext, onPrevious, Modifier.zIndex(1f))
        if (count > 1) {
            val shape = RoundedCornerShape(bottomStart = CornerRadius.group, bottomEnd = CornerRadius.group)
            // 上沿压在卡片底下 10dp，只露出左右和下面三条边。
            Box(
                Modifier
                    .layout { measurable, constraints ->
                        val placeable = measurable.measure(constraints)
                        val overlap = 10.dp.roundToPx()
                        layout(placeable.width, placeable.height - overlap) { placeable.place(0, -overlap) }
                    }
                    .padding(horizontal = 16.dp)
                    .fillMaxWidth()
                    .height(20.dp)
                    .background(colors.stackEdge, shape)
                    .border(1.dp, colors.hairline, shape)
                    .clearAndSetSemantics {},
            )
        }
    }
}

/**
 * 等你回答的一个会话：头部是标题和状态，不止一个在等时右边的「1 / 2」翻到下一张；中间是屏幕的摘录，点头部或摘录打开终端；
 * 下面直接快速回复。认出了提问时选项单独列在下面，摘录只放问题。
 */
@Composable
private fun WaitingCard(
    item: WaitingSession,
    showsMachine: Boolean,
    position: Int,
    count: Int,
    onOpen: () -> Unit,
    onNext: () -> Unit,
    onPrevious: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = themeColors
    val list = item.list
    val session = item.session
    val title = Presentation.sessionTitle(session).resolve()
    val openHint = stringResource(R.string.open_terminal_hint)
    val positionText = stringResource(R.string.home_page_position, position, count)
    val nextLabel = stringResource(R.string.home_next_waiting)
    val previousLabel = stringResource(R.string.home_previous_waiting)
    Column(modifier.fillMaxWidth().cardBackground(colors, padding = 14.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Row(Modifier.defaultMinSize(minHeight = 44.dp), verticalAlignment = Alignment.CenterVertically) {
            Column(
                Modifier
                    .weight(1f)
                    .clickable(onClickLabel = openHint, role = Role.Button, onClick = onOpen)
                    .semantics(mergeDescendants = true) {
                        if (count > 1) {
                            stateDescription = positionText
                            customActions = listOf(
                                CustomAccessibilityAction(nextLabel) { onNext(); true },
                                CustomAccessibilityAction(previousLabel) { onPrevious(); true },
                            )
                        }
                    },
                verticalArrangement = Arrangement.spacedBy(2.dp),
            ) {
                Text(title, style = Type.headline, maxLines = 2, overflow = TextOverflow.Ellipsis)
                Text(
                    Presentation.sessionSubtitle(
                        session,
                        if (showsMachine) list.machine.name else null,
                        agentClock(session.meta.agent, session.meta.agentSinceMs),
                    ).resolve(),
                    style = Type.footnote,
                    color = colors.secondary,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            if (count > 1) {
                Box(
                    Modifier
                        .size(width = 64.dp, height = 44.dp)
                        .clickable(role = Role.Button, onClick = onNext)
                        .semantics {
                            contentDescription = nextLabel
                            stateDescription = positionText
                        },
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        "$position / $count",
                        style = Type.caption,
                        color = colors.secondary,
                        modifier = Modifier
                            .background(colors.raised, RoundedCornerShape(50))
                            .padding(horizontal = 10.dp, vertical = 5.dp),
                    )
                }
            }
        }
        val prompt = list.prompts[session.id]
        val preview = prompt?.question ?: list.previews[session.id] ?: emptyList()
        if (preview.isNotEmpty()) {
            Row(
                Modifier
                    .background(colors.well, Shapes.inner)
                    .clickable(onClickLabel = openHint, role = Role.Button, onClick = onOpen),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                ScreenPreview(preview, Modifier.weight(1f), tone = PreviewTone.Primary)
                DisclosureChevron(Modifier.padding(end = 10.dp))
            }
        }
        val replyLabel = stringResource(R.string.reply_to, title)
        QuickReplyBar(
            list.quickReply(session.id),
            prompt,
            Modifier.semantics { contentDescription = replyLabel },
        )
    }
}

/** 首页顶上的一句话，有会话在等你时后面闪着光标；下面一行补充，说的是在干活的会话时前面带盲文格。 */
@Composable
private fun HomeHeadline(summary: HomeSummary, modifier: Modifier = Modifier) {
    Column(
        modifier
            .fillMaxWidth()
            .padding(horizontal = 2.dp)
            .semantics(mergeDescendants = true) { heading() },
        verticalArrangement = Arrangement.spacedBy(5.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(7.dp)) {
            Text(Presentation.homeTitle(summary).resolve(), style = Type.title, modifier = Modifier.weight(1f, fill = false))
            if (summary.waiting > 0) BlinkingCursor()
        }
        Presentation.homeDetail(summary)?.let { detail ->
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(7.dp)) {
                if (summary.working > 0) StatusGlyph(StatusMark.Working, 15.dp)
                Text(detail.resolve(), style = Type.subheadline, color = themeColors.secondary)
            }
        }
    }
}

/** 配对过的电脑并成一张卡，一台一行。 */
@Composable
private fun MachineGroup(app: AppModel) {
    val colors = themeColors
    val machines = app.machineList.machines
    Column(
        Modifier
            .fillMaxWidth()
            .background(colors.card, Shapes.group)
            .border(1.dp, colors.hairline, Shapes.group),
    ) {
        machines.forEachIndexed { index, machine ->
            if (index > 0) HorizontalDivider(thickness = 1.dp, color = colors.hairline)
            MachineRow(
                machine = machine,
                list = app.sessionList(machine.id),
                onOpen = { app.path = listOf(Route.Machine(machine.id)) },
                onRename = { app.machineList.beginRename(machine.id) },
                onDelete = { app.machineList.deleteTarget = machine.id },
            )
        }
    }
}

/**
 * 电脑组里的一台电脑：名字，下面是连接状态和会话数，点了打开它的会话列表；长按有打开、改名、删除，读屏经自定义操作。
 * 断开了在等重连或者连不上时，右边是立刻重连的「重试」。
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun MachineRow(
    machine: MachineRecord,
    list: SessionListModel?,
    onOpen: () -> Unit,
    onRename: () -> Unit,
    onDelete: () -> Unit,
) {
    val colors = themeColors
    val state = list?.linkState ?: LinkState.Idle
    val retries = state is LinkState.Waiting || state is LinkState.Failed
    val now = linkClock(state)
    var menu by remember { mutableStateOf(false) }
    val renameLabel = stringResource(R.string.action_rename)
    val deleteLabel = stringResource(R.string.action_delete)
    val hint = stringResource(R.string.home_open_machine_hint)
    Row(verticalAlignment = Alignment.CenterVertically) {
        Box(Modifier.weight(1f)) {
            Row(
                Modifier
                    .fillMaxWidth()
                    .defaultMinSize(minHeight = 56.dp)
                    .combinedClickable(onClickLabel = hint, role = Role.Button, onLongClick = { menu = true }, onClick = onOpen)
                    .semantics(mergeDescendants = true) {
                        customActions = listOf(
                            CustomAccessibilityAction(renameLabel) { onRename(); true },
                            CustomAccessibilityAction(deleteLabel) { onDelete(); true },
                        )
                    }
                    .padding(start = 14.dp, end = if (retries) 12.dp else 14.dp, top = 8.dp, bottom = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Icon(
                    AppIcons.laptop,
                    contentDescription = null,
                    tint = if (state.isConnected) colors.primary else colors.tertiary,
                    modifier = Modifier.size(20.dp),
                )
                Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(1.dp)) {
                    Text(
                        machine.name,
                        style = Type.subheadline.copy(fontWeight = FontWeight.SemiBold),
                        color = if (state.isConnected) colors.primary else colors.secondary,
                        maxLines = 2,
                    )
                    val line = Presentation.machineLine(state, list?.sessions ?: emptyList(), list?.loaded ?: false, now)
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        MachineLineMark(line.tone, state.isConnected)
                        Text(machineLineText(line), style = Type.footnote, color = colors.secondary, maxLines = 2)
                    }
                }
                if (!retries) DisclosureChevron()
            }
            DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                DropdownMenuItem(text = { Text(stringResource(R.string.action_open)) }, onClick = { menu = false; onOpen() })
                DropdownMenuItem(text = { Text(renameLabel) }, onClick = { menu = false; onRename() })
                DropdownMenuItem(
                    text = { Text(deleteLabel, color = colors.failure) },
                    onClick = { menu = false; onDelete() },
                )
            }
        }
        if (retries) {
            val reconnect = stringResource(R.string.home_reconnect, machine.name)
            SurfaceButton(
                ActionKind.Raised,
                onClick = { list?.reconnect() },
                contentPadding = 16.dp,
                modifier = Modifier.padding(end = 6.dp).semantics { onClick(label = reconnect) { list?.reconnect(); true } },
            ) {
                Text(stringResource(R.string.action_retry), style = Type.subheadline.copy(fontWeight = FontWeight.Medium))
            }
        }
    }
}

@Composable
private fun machineLineText(line: MachineLine) = buildAnnotatedString {
    val colors = themeColors
    line.emphasis?.let { emphasis ->
        val color = when (line.tone) {
            MachineLine.Tone.Waiting -> colors.waiting
            MachineLine.Tone.Working -> colors.working
            MachineLine.Tone.Failure -> colors.failure
            MachineLine.Tone.Neutral -> colors.secondary
        }
        withStyle(SpanStyle(color = color)) { append(emphasis.resolve()) }
    }
    append(line.text.resolve())
}

/** 电脑那一行前面的标记：等你回答、在干活是对应的状态标记，连不上是叉，连着而只有普通会话时是提示符，没连上时不画。 */
@Composable
private fun MachineLineMark(tone: MachineLine.Tone, connected: Boolean) {
    val size = 12.dp
    when (tone) {
        MachineLine.Tone.Waiting -> StatusGlyph(StatusMark.Waiting, size)
        MachineLine.Tone.Working -> StatusGlyph(StatusMark.Working, size)
        MachineLine.Tone.Failure -> Icon(AppIcons.close, null, tint = themeColors.failure, modifier = Modifier.size(size))
        MachineLine.Tone.Neutral -> if (connected) StatusGlyph(StatusMark.Idle, size)
    }
}

/** 上次打开的终端，样子和电脑组里的一行一样：前面是会话状态的标记，下面是电脑名和会话在干什么。 */
@Composable
private fun ResumeCard(item: ResumeItem, mark: StatusMark, status: String?, onOpen: () -> Unit) {
    val colors = themeColors
    val subtitle = status?.let { stringResource(R.string.home_resume_detail, item.machine.name, it) } ?: item.machine.name
    Row(
        Modifier
            .fillMaxWidth()
            .defaultMinSize(minHeight = 56.dp)
            .background(colors.card, Shapes.group)
            .border(1.dp, colors.hairline, Shapes.group)
            .clickable(onClickLabel = stringResource(R.string.open_terminal_hint), role = Role.Button, onClick = onOpen)
            .semantics(mergeDescendants = true) {}
            .padding(horizontal = 14.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Box(Modifier.width(20.dp), contentAlignment = Alignment.Center) { StatusGlyph(mark, 16.dp) }
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(1.dp)) {
            Text(item.title.resolve(), style = Type.subheadline.copy(fontWeight = FontWeight.SemiBold), maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text(subtitle, style = Type.footnote, color = colors.secondary, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        DisclosureChevron()
    }
}

/** 新开会话能开在哪一处：电脑上的某个工作区，或者一台电脑当前的工作区（[section] 为空）。 */
private class SpawnTarget(val id: String, val title: String, val list: SessionListModel, val section: SessionSection?)

/**
 * 有 `spawnChoices` 的电脑一个工作区一项，让用户挑；别的电脑一台一项。连着的电脑不止一台时每项带上电脑的名字，电脑开着几个
 * 窗口时带上窗口，同名的工作区才分得清。
 */
@Composable
private fun spawnTargets(app: AppModel): List<SpawnTarget> {
    val lists = app.spawnableLists
    return lists.flatMap { list ->
        val machine = list.machine.name
        val choices = list.spawnChoices
        if (choices.isEmpty()) return@flatMap listOf(SpawnTarget(list.machine.id.toString(), machine, list, null))
        choices.map { section ->
            var name = Presentation.sectionTitle(section).resolve()
            section.window?.let { name = stringResource(R.string.home_spawn_window, name, it.toInt()) }
            SpawnTarget(
                "${list.machine.id}/${section.id}",
                if (lists.size > 1) stringResource(R.string.home_spawn_target, machine, name) else name,
                list,
                section,
            )
        }
    }
}

private fun spawn(target: SpawnTarget, agent: InstalledAgent?, prepare: (SessionListModel) -> Unit) {
    prepare(target.list)
    val section = target.section
    if (section != null) target.list.spawn(section, agent) else target.list.spawn(agent = agent)
}

/** 钉在页面底下的配对和新开终端两个按钮。[prepare] 在新开之前按页面大小算好网格。 */
@Composable
private fun ActionBar(app: AppModel, prepare: (SessionListModel) -> Unit) {
    val colors = themeColors
    val spawnable = app.spawnableLists
    val targets = spawnTargets(app)
    val busy = spawnable.any { it.isSpawning }
    var menu by remember { mutableStateOf(false) }
    var menuTarget by remember { mutableStateOf<SpawnTarget?>(null) }
    Row(
        Modifier
            .fillMaxWidth()
            .background(colors.page)
            .windowInsetsPadding(WindowInsets.navigationBars)
            .padding(horizontal = ScreenPadding, vertical = 8.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        SurfaceButton(
            ActionKind.Secondary,
            onClick = { app.startPairing() },
            height = 50.dp,
            cornerRadius = CornerRadius.action,
            modifier = Modifier.weight(1f),
        ) {
            Icon(AppIcons.scan, null, modifier = Modifier.size(18.dp))
            Spacer(Modifier.width(8.dp))
            Text(stringResource(R.string.home_pair), style = Type.subheadline.copy(fontWeight = ActionKind.Secondary.weight))
        }
        val single = targets.singleOrNull()?.takeIf { it.list.agents.isEmpty() }
        Box(Modifier.weight(1f)) {
            SurfaceButton(
                ActionKind.Primary,
                onClick = {
                    if (single != null) {
                        spawn(single, null, prepare)
                    } else {
                        menuTarget = null
                        menu = true
                    }
                },
                enabled = !busy && spawnable.isNotEmpty(),
                height = 50.dp,
                cornerRadius = CornerRadius.action,
                modifier = Modifier.fillMaxWidth(),
            ) {
                if (busy) {
                    CircularProgressIndicator(Modifier.size(16.dp), color = colors.page, strokeWidth = 2.dp)
                } else {
                    Icon(AppIcons.plus, null, modifier = Modifier.size(18.dp))
                }
                Spacer(Modifier.width(8.dp))
                Text(stringResource(R.string.new_terminal), style = Type.subheadline.copy(fontWeight = ActionKind.Primary.weight))
            }
            DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                val chosen = menuTarget ?: targets.singleOrNull()
                if (chosen != null) {
                    SpawnKindItems(chosen.list.agents) { agent ->
                        menu = false
                        spawn(chosen, agent, prepare)
                    }
                } else {
                    val header = if (targets.any { it.section != null }) R.string.home_spawn_where else R.string.home_spawn_which_machine
                    Text(
                        stringResource(header),
                        style = Type.footnote,
                        color = colors.secondary,
                        modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp).semantics { heading() },
                    )
                    for (target in targets) {
                        DropdownMenuItem(
                            text = { Text(target.title) },
                            trailingIcon = if (target.list.agents.isEmpty()) null else ({ DisclosureChevron() }),
                            onClick = {
                                if (target.list.agents.isEmpty()) {
                                    menu = false
                                    spawn(target, null, prepare)
                                } else {
                                    menuTarget = target
                                }
                            },
                        )
                    }
                }
            }
        }
    }
}

/** 还没配对电脑时的首页：中间一句话说清这是干什么的和配对按钮，下面两步说明怎么配。 */
@Composable
private fun Onboarding(modifier: Modifier = Modifier, onPair: () -> Unit) {
    val colors = themeColors
    Column(
        modifier
            .fillMaxWidth()
            .verticalScroll(rememberScrollState())
            .windowInsetsPadding(WindowInsets.navigationBars)
            .padding(bottom = 16.dp),
    ) {
        Spacer(Modifier.height(64.dp))
        Column(
            Modifier.fillMaxWidth().padding(horizontal = 32.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(stringResource(R.string.onboarding_title), style = Type.title2, textAlign = TextAlign.Center)
            Text(
                stringResource(R.string.onboarding_detail),
                style = Type.subheadline,
                color = colors.secondary,
                textAlign = TextAlign.Center,
            )
            SurfaceButton(ActionKind.Primary, onClick = onPair, height = 48.dp, contentPadding = 24.dp, modifier = Modifier.padding(top = 12.dp)) {
                Icon(AppIcons.scan, null, modifier = Modifier.size(18.dp))
                Spacer(Modifier.width(8.dp))
                Text(stringResource(R.string.home_pair), style = Type.headline)
            }
        }
        Spacer(Modifier.height(64.dp))
        Column(Modifier.padding(horizontal = 24.dp)) {
            Text(
                stringResource(R.string.onboarding_how),
                style = Type.footnote.copy(fontWeight = FontWeight.SemiBold),
                color = colors.secondary,
                modifier = Modifier.padding(bottom = 10.dp).semantics { heading() },
            )
            OnboardingStep(1, stringResource(R.string.onboarding_step_command), stringResource(R.string.onboarding_step_command_detail))
            HorizontalDivider(Modifier.padding(start = 40.dp), thickness = 1.dp, color = colors.hairline)
            OnboardingStep(2, stringResource(R.string.onboarding_step_scan), stringResource(R.string.onboarding_step_scan_detail))
        }
    }
}

/** 配对说明里的一步：左边编号，右边标题和一行说明（说明里反引号括起来的是命令）。 */
@Composable
private fun OnboardingStep(number: Int, title: String, detail: String) {
    val colors = themeColors
    Row(
        Modifier.padding(vertical = 12.dp).semantics(mergeDescendants = true) {},
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Box(Modifier.size(28.dp).background(colors.fill, Shapes.inner), contentAlignment = Alignment.Center) {
            Text(number.toString(), style = Type.footnote.copy(fontWeight = FontWeight.SemiBold), color = colors.secondary)
        }
        Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(title, style = Type.subheadline.copy(fontWeight = FontWeight.SemiBold))
            Text(inlineCode(detail), style = Type.footnote, color = colors.secondary)
        }
    }
}

/** 给电脑改名的输入框。 */
@Composable
private fun RenameDialog(app: AppModel, scope: CoroutineScope) {
    val machines = app.machineList
    val target = machines.renameTarget ?: return
    if (!machines.isRenaming) return
    AlertDialog(
        onDismissRequest = { machines.isRenaming = false },
        title = { Text(stringResource(R.string.action_rename)) },
        text = {
            OutlinedTextField(
                value = machines.renameText,
                onValueChange = { machines.renameText = it },
                singleLine = true,
                label = { Text(stringResource(R.string.name_field)) },
            )
        },
        confirmButton = {
            TextButton(onClick = {
                val name = machines.renameText
                machines.isRenaming = false
                scope.launch { machines.rename(target, name) }
            }) { Text(stringResource(R.string.action_save)) }
        },
        dismissButton = { TextButton(onClick = { machines.isRenaming = false }) { Text(stringResource(R.string.action_cancel)) } },
    )
}

/** 删除电脑前的确认。连着时删除前先请电脑解除配对；没连着时说明要等它再连上。 */
@Composable
private fun DeleteDialog(app: AppModel, scope: CoroutineScope) {
    val machines = app.machineList
    val target = machines.deleteTarget ?: return
    val connected = app.sessionList(target)?.linkState?.isConnected == true
    ConfirmDialog(
        title = stringResource(R.string.home_delete_title),
        message = stringResource(if (connected) R.string.home_delete_connected else R.string.home_delete_offline),
        confirm = stringResource(R.string.action_delete),
        onConfirm = { scope.launch { machines.delete(target) } },
        onDismiss = { machines.deleteTarget = null },
    )
}
