package dev.runode.mobile.features.ui.sessions

import android.content.ClipData
import android.content.ClipboardManager
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
import androidx.compose.foundation.layout.windowInsetsBottomHeight
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
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
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import dev.runode.mobile.features.R
import dev.runode.mobile.features.model.InstalledAgent
import dev.runode.mobile.features.model.Presentation
import dev.runode.mobile.features.model.SessionFilter
import dev.runode.mobile.features.model.SessionListModel
import dev.runode.mobile.features.model.SessionSection
import dev.runode.mobile.features.model.SessionStatus
import dev.runode.mobile.features.ui.AgentLogoIcon
import dev.runode.mobile.features.ui.AppIcons
import dev.runode.mobile.features.ui.CircleIconButton
import dev.runode.mobile.features.ui.ConfirmDialog
import dev.runode.mobile.features.ui.ConnectionStatusRow
import dev.runode.mobile.features.ui.DisclosureChevron
import dev.runode.mobile.features.ui.EmptyState
import dev.runode.mobile.features.ui.ErrorDialog
import dev.runode.mobile.features.ui.LeadingTitle
import dev.runode.mobile.features.ui.LoadingState
import dev.runode.mobile.features.ui.PreviewTone
import dev.runode.mobile.features.ui.PromptOptionLayout
import dev.runode.mobile.features.ui.QuickReplyBar
import dev.runode.mobile.features.ui.ScreenPadding
import dev.runode.mobile.features.ui.ScreenPreview
import dev.runode.mobile.features.ui.SpawnKindItems
import dev.runode.mobile.features.ui.TopBar
import dev.runode.mobile.features.ui.agentClock
import dev.runode.mobile.features.ui.linkClock
import dev.runode.mobile.features.ui.theme.ActionKind
import dev.runode.mobile.features.ui.theme.Shapes
import dev.runode.mobile.features.ui.theme.StatusGlyph
import dev.runode.mobile.features.ui.theme.StatusMark
import dev.runode.mobile.features.ui.theme.SurfaceButton
import dev.runode.mobile.features.ui.theme.Type
import dev.runode.mobile.features.ui.theme.cardBackground
import dev.runode.mobile.features.ui.theme.resolve
import dev.runode.mobile.features.ui.theme.themeColors
import dev.runode.mobile.protocol.SessionId
import dev.runode.mobile.protocol.SessionInfo
import dev.runode.mobile.protocol.TaskSource
import dev.runode.mobile.terminal.TerminalView

/**
 * 一台电脑上的会话，按工作区分节（[SessionListModel.sections]），节头能新开终端、长按改名。顶上能筛出只看等你回答的、
 * 在干活的。一个会话一张卡片，样子按 [SessionStatus] 分：等你回答的带屏幕摘录和一排快速回复的按键，在干活的带屏幕最后
 * 一行，其余只有标题和状态；点开终端，长按有更多操作。右上角新开终端，打开远程桌面。
 *
 * @param onOpenScreen 打开这台电脑的远程桌面；为空时没有这个入口
 * @param fontSize 设置里的手动字号，为空时跟随系统字体大小；后台新开会话时按它算网格
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SessionListView(
    model: SessionListModel,
    onBack: () -> Unit,
    onOpen: (SessionId) -> Unit,
    onOpenGit: (SessionId) -> Unit,
    onOpenScreen: (() -> Unit)?,
    fontSize: Double?,
) {
    val colors = themeColors
    val context = LocalContext.current
    var filter by rememberSaveable { mutableStateOf(SessionFilter.All) }
    var renaming by remember { mutableStateOf<SessionSection?>(null) }
    var pageSize by remember { mutableStateOf(IntSize.Zero) }
    LaunchedEffect(model) { model.keepRefreshing() }
    // 和打开终端后适配的网格一样大，宿主起会话时就不用再改尺寸。
    LaunchedEffect(pageSize, fontSize) {
        if (pageSize != IntSize.Zero) model.spawnSize = TerminalView.gridSize(context, pageSize.width, pageSize.height, fontSize?.toFloat())
    }
    val sections = model.sections(matching = filter)
    val connected = model.linkState.isConnected
    val now = linkClock(model.linkState)

    val picker = model.directoryPicker
    Box(Modifier.fillMaxSize()) {
        // 选目录的页盖着列表时，读屏只读那一页。
        val covered = if (picker != null) Modifier.clearAndSetSemantics {} else Modifier
        Column(Modifier.fillMaxSize().background(colors.page).onSizeChanged { pageSize = it }.then(covered)) {
            TopBar(
                onBack = onBack,
                actions = {
                    if (onOpenScreen != null) {
                        CircleIconButton(AppIcons.display, stringResource(R.string.remote_desktop), onOpenScreen, enabled = connected)
                    }
                    NewMenu(model, enabled = connected && !model.isSpawning)
                },
            ) {
                LeadingTitle(model.machine.name, Presentation.linkRoute(model.linkState, now).resolve())
            }
            PullToRefreshBox(
                isRefreshing = false,
                onRefresh = {
                    model.refresh()
                    model.findAgents()
                    model.refreshPreviews()
                    model.refreshTasks()
                },
                modifier = Modifier.weight(1f).fillMaxWidth(),
            ) {
                LazyColumn(
                    Modifier.fillMaxSize(),
                    contentPadding = PaddingValues(horizontal = ScreenPadding, vertical = 4.dp),
                    verticalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    if (!connected) {
                        item(key = "link") {
                            ConnectionStatusRow(model.linkState, model::reconnect, Modifier.cardBackground(colors))
                        }
                    }
                    // 一个会话都没有时不出筛选条；筛着的时候会话没了也留着，才切得回全部。
                    if (model.sessions.isNotEmpty() || filter != SessionFilter.All) {
                        item(key = "filter") { FilterBar(filter, model::count) { filter = it } }
                    }
                    if (sections.isEmpty() && filter != SessionFilter.All) {
                        item(key = "filter-empty") { FilterEmpty(filter) { filter = SessionFilter.All } }
                    }
                    for (section in sections) {
                        item(key = "section-${section.id}") {
                            WorkspaceHeader(
                                section = section,
                                canSpawn = model.canSpawn(section),
                                isSpawning = model.isSpawning,
                                agents = model.agents,
                                onSpawn = { agent -> model.spawn(section, agent) },
                                onRename = { renaming = section },
                            )
                        }
                        items(section.sessions, key = { "session-${section.id}-${it.id}" }) { session ->
                            SessionCard(model, session, section, onOpen, onOpenGit, context)
                        }
                    }
                    item(key = "bottom") { Spacer(Modifier.windowInsetsBottomHeight(WindowInsets.navigationBars)) }
                }
                // 有空工作区时每一节自己写着「没有终端」，整页的空状态只在一节都没有时出。
                if (model.loaded && model.sections.isEmpty() && connected) {
                    EmptyState(
                        stringResource(R.string.sessions_empty_title),
                        stringResource(R.string.sessions_empty_detail),
                        Modifier.align(Alignment.Center),
                    ) { EmptySpawnButton(model) }
                } else if (!model.loaded && connected) {
                    LoadingState(stringResource(R.string.sessions_loading), Modifier.align(Alignment.Center))
                }
            }
        }
        if (picker != null) {
            DirectoryPickerView(picker, model.hasDesktopWindow, onCancel = model::cancelNewWorkspace) { dir, name ->
                model.createWorkspace(dir, name)
            }
        }
    }

    renaming?.let { section -> RenameWorkspaceDialog(section, onDismiss = { renaming = null }) { model.rename(section, it) } }
    model.killTarget?.let { id ->
        val title = model.session(id)?.let { Presentation.sessionTitle(it).resolve() } ?: ""
        ConfirmDialog(
            title = stringResource(R.string.end_terminal_title),
            message = stringResource(R.string.end_terminal_named, title),
            confirm = stringResource(R.string.end_session),
            onConfirm = { model.kill(id) },
            onDismiss = { if (model.killTarget == id) model.killTarget = null },
        )
    }
    ErrorDialog(model.errorMessage?.resolve()) { model.errorMessage = null }
}

/**
 * 顶栏右边的「+」：新开终端，电脑上装着 agent 时还能起哪一家，再是新建工作区（打开 [DirectoryPickerView]）。新开终端时
 * 能新开终端的工作区不止一个的话先挑工作区，否则直接开在电脑上当前的工作区里。
 */
@Composable
private fun NewMenu(model: SessionListModel, enabled: Boolean) {
    var expanded by remember { mutableStateOf(false) }
    var agentChosen by remember { mutableStateOf(false) }
    var agent by remember { mutableStateOf<InstalledAgent?>(null) }
    val choices = model.spawnChoices
    fun pick(chosen: InstalledAgent?) {
        if (choices.isEmpty()) {
            expanded = false
            model.spawn(agent = chosen)
        } else {
            agent = chosen
            agentChosen = true
        }
    }
    Box {
        CircleIconButton(
            AppIcons.plus,
            stringResource(R.string.sessions_new),
            {
                agentChosen = false
                agent = null
                expanded = true
            },
            prominent = true,
            enabled = enabled,
        )
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            if (!agentChosen) {
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.new_terminal)) },
                    leadingIcon = { Icon(AppIcons.terminal, null, Modifier.size(18.dp)) },
                    onClick = { pick(null) },
                )
                for (installed in model.agents) {
                    DropdownMenuItem(
                        text = { Text(stringResource(R.string.new_agent, installed.label)) },
                        leadingIcon = { AgentLogoIcon(installed.kind, Modifier.size(18.dp)) },
                        onClick = { pick(installed) },
                    )
                }
                HorizontalDivider(thickness = 1.dp, color = themeColors.hairline)
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.new_workspace)) },
                    leadingIcon = { Icon(AppIcons.folderPlus, null, Modifier.size(18.dp)) },
                    onClick = {
                        expanded = false
                        model.beginNewWorkspace()
                    },
                )
            } else {
                for (section in choices) {
                    DropdownMenuItem(
                        text = {
                            Column {
                                Text(Presentation.sectionTitle(section).resolve())
                                Presentation.sectionDetail(section)?.let {
                                    Text(it.resolve(), style = Type.caption, color = themeColors.secondary)
                                }
                            }
                        },
                        onClick = {
                            expanded = false
                            model.spawn(section, agent)
                        },
                    )
                }
            }
        }
    }
}

/** 空页上新开终端的按钮：电脑上装着 agent 时先挑开什么。 */
@Composable
private fun EmptySpawnButton(model: SessionListModel) {
    var expanded by remember { mutableStateOf(false) }
    Box(Modifier.padding(top = 8.dp)) {
        SurfaceButton(
            ActionKind.Primary,
            onClick = { if (model.agents.isEmpty()) model.spawn() else expanded = true },
            enabled = !model.isSpawning,
            height = 48.dp,
            contentPadding = 24.dp,
        ) {
            Text(stringResource(R.string.new_terminal), style = Type.headline)
        }
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            SpawnKindItems(model.agents) {
                expanded = false
                model.spawn(agent = it)
            }
        }
    }
}

/** 列表顶上的分段筛选：全部、等你、干活中，各带会话数。等你的有会话时那一段的字一律是琥珀色，没选中也看得出有人在等。 */
@Composable
private fun FilterBar(selection: SessionFilter, count: (SessionFilter) -> Int, onSelect: (SessionFilter) -> Unit) {
    val colors = themeColors
    Row(
        Modifier
            .fillMaxWidth()
            .background(colors.well, Shapes.inner)
            .padding(3.dp),
        horizontalArrangement = Arrangement.spacedBy(3.dp),
    ) {
        for (filter in SessionFilter.entries) {
            val chosen = filter == selection
            val n = count(filter)
            val name = stringResource(
                when (filter) {
                    SessionFilter.All -> R.string.filter_all
                    SessionFilter.Waiting -> R.string.filter_waiting
                    SessionFilter.Working -> R.string.filter_working
                },
            )
            val label = stringResource(R.string.filter_count, name, n)
            val tint = if (filter == SessionFilter.Waiting && n > 0) colors.waiting else if (chosen) colors.primary else colors.secondary
            Row(
                Modifier
                    .weight(1f)
                    .defaultMinSize(minHeight = 40.dp)
                    .background(if (chosen) colors.card else androidx.compose.ui.graphics.Color.Transparent, RoundedCornerShape(10.dp))
                    .then(if (chosen) Modifier.border(1.dp, colors.hairline, RoundedCornerShape(10.dp)) else Modifier)
                    .clickable(role = Role.Tab) { onSelect(filter) }
                    .semantics(mergeDescendants = true) {
                        contentDescription = label
                        selected = chosen
                    },
                horizontalArrangement = Arrangement.Center,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(name, style = Type.subheadline.copy(fontWeight = if (chosen) FontWeight.SemiBold else FontWeight.Medium), color = tint)
                Spacer(Modifier.width(6.dp))
                Text(n.toString(), style = Type.footnote, color = tint.copy(alpha = tint.alpha * 0.8f))
            }
        }
    }
}

/** 筛出来一个会话都没有时列表里的那一行，带一个回到全部的按钮。 */
@Composable
private fun FilterEmpty(filter: SessionFilter, onShowAll: () -> Unit) {
    Column(
        Modifier.fillMaxWidth().padding(vertical = 24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text(
            stringResource(if (filter == SessionFilter.Waiting) R.string.filter_empty_waiting else R.string.filter_empty_working),
            style = Type.subheadline,
            color = themeColors.secondary,
            textAlign = TextAlign.Center,
        )
        SurfaceButton(ActionKind.Raised, onClick = onShowAll, contentPadding = 16.dp) {
            Text(stringResource(R.string.filter_show_all))
        }
    }
}

/**
 * 一节的标题：工作区的名字和目录，右边一个在这个工作区里新开终端的按钮（电脑上装着 agent 时是挑开终端还是哪家 agent 的
 * 菜单），长按菜单能新开终端、改名；后台那一节只有标题。
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun WorkspaceHeader(
    section: SessionSection,
    canSpawn: Boolean,
    isSpawning: Boolean,
    agents: List<InstalledAgent>,
    onSpawn: (InstalledAgent?) -> Unit,
    onRename: () -> Unit,
) {
    val colors = themeColors
    val title = Presentation.sectionTitle(section).resolve()
    val isWorkspace = section.id is SessionSection.Id.Workspace
    var menu by remember { mutableStateOf(false) }
    var spawnMenu by remember { mutableStateOf(false) }
    val spawnLabel = stringResource(R.string.spawn_in_section, title)
    Box {
        Column(
            Modifier
                .fillMaxWidth()
                .then(
                    if (isWorkspace) Modifier.combinedClickable(onLongClick = { menu = true }, onClick = {}) else Modifier,
                )
                .padding(start = 4.dp, end = 4.dp, top = 8.dp),
        ) {
            Row(Modifier.defaultMinSize(minHeight = 32.dp), verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    if (isWorkspace) AppIcons.folder else AppIcons.moon,
                    contentDescription = null,
                    tint = colors.secondary,
                    modifier = Modifier.width(20.dp).height(18.dp),
                )
                Spacer(Modifier.width(8.dp))
                Text(
                    title,
                    style = Type.subheadline.copy(fontWeight = FontWeight.SemiBold),
                    maxLines = 1,
                    modifier = Modifier.semantics { heading() },
                )
                Presentation.sectionDirectory(section)?.let { directory ->
                    Spacer(Modifier.width(8.dp))
                    Text(
                        directory,
                        style = Type.caption,
                        fontFamily = FontFamily.Monospace,
                        color = colors.tertiary,
                        maxLines = 1,
                        overflow = TextOverflow.StartEllipsis,
                        modifier = Modifier.weight(1f, fill = false),
                    )
                }
                Presentation.sectionWindow(section)?.let { window ->
                    Spacer(Modifier.width(8.dp))
                    Text(window.resolve(), style = Type.caption, color = colors.tertiary, maxLines = 1)
                }
                Spacer(Modifier.weight(1f))
                if (canSpawn) {
                    Box {
                        Box(
                            Modifier
                                .size(width = 44.dp, height = 36.dp)
                                .clickable(enabled = !isSpawning, role = Role.Button) {
                                    if (agents.isEmpty()) onSpawn(null) else spawnMenu = true
                                }
                                .semantics { contentDescription = spawnLabel },
                            contentAlignment = Alignment.CenterEnd,
                        ) {
                            Icon(AppIcons.plus, contentDescription = null, tint = colors.secondary, modifier = Modifier.size(18.dp))
                        }
                        DropdownMenu(expanded = spawnMenu, onDismissRequest = { spawnMenu = false }) {
                            SpawnKindItems(agents) {
                                spawnMenu = false
                                onSpawn(it)
                            }
                        }
                    }
                }
            }
            // 后台那一节没有目录，说明它是什么的那句另起一行；空工作区不另起一张卡片，也在这里写一行。两行都和标题的字左对齐。
            if (!isWorkspace) {
                Presentation.sectionDetail(section)?.let {
                    Text(it.resolve(), style = Type.caption, color = colors.tertiary, modifier = Modifier.padding(start = 28.dp))
                }
            }
            if (section.sessions.isEmpty()) {
                Text(
                    stringResource(R.string.section_no_terminals),
                    style = Type.caption,
                    color = colors.tertiary,
                    modifier = Modifier.padding(start = 28.dp),
                )
            }
        }
        DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
            if (canSpawn) {
                if (agents.isEmpty()) {
                    DropdownMenuItem(
                        text = { Text(stringResource(R.string.spawn_here)) },
                        enabled = !isSpawning,
                        onClick = { menu = false; onSpawn(null) },
                    )
                } else {
                    SpawnKindItems(agents) {
                        menu = false
                        onSpawn(it)
                    }
                }
            }
            DropdownMenuItem(text = { Text(stringResource(R.string.action_rename)) }, onClick = { menu = false; onRename() })
        }
    }
}

/**
 * 一个会话一张卡片，样子按 [SessionStatus] 分。等你回答的卡片只有头部能点开终端，下面的按键是快速回复；别的卡片整张能
 * 点开。长按有打开、Git、复制目录、会话目录里的命令和结束会话；命令在这个会话里跑了时打开它的终端。
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun SessionCard(
    model: SessionListModel,
    session: SessionInfo,
    section: SessionSection,
    onOpen: (SessionId) -> Unit,
    onOpenGit: (SessionId) -> Unit,
    context: android.content.Context,
) {
    val colors = themeColors
    var menu by remember { mutableStateOf(false) }
    var taskSource by remember { mutableStateOf<TaskSource?>(null) }
    val hint = stringResource(R.string.open_terminal_hint)
    val open = { onOpen(session.id) }
    LaunchedEffect(session.meta.cwd) { session.meta.cwd?.let { model.loadTasks(it) } }
    val clickable = Modifier.combinedClickable(onClickLabel = hint, role = Role.Button, onLongClick = { menu = true }, onClick = open)
    val head = @Composable { modifier: Modifier ->
        SessionRow(session, section.dir, model.tabNumber(session.id), modifier)
    }
    val preview = model.previews[session.id] ?: emptyList()
    Box {
        when (val status = SessionStatus.of(session)) {
            SessionStatus.Waiting -> {
                // 认出了提问时选项单独成键，摘录只放问题。
                val prompt = model.prompts[session.id]
                val excerpt = prompt?.question ?: preview
                val replyLabel = stringResource(R.string.reply_to, Presentation.sessionTitle(session).resolve())
                Column(Modifier.fillMaxWidth().cardBackground(colors), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    head(clickable)
                    if (excerpt.isNotEmpty()) ScreenPreview(excerpt, tone = PreviewTone.Primary)
                    QuickReplyBar(
                        model.quickReply(session.id),
                        prompt,
                        Modifier.semantics { contentDescription = replyLabel },
                        layout = PromptOptionLayout.Compact,
                        showsDraft = false,
                    )
                }
            }
            SessionStatus.Working -> Column(
                Modifier
                    .fillMaxWidth()
                    .background(colors.card, Shapes.card)
                    .border(1.dp, colors.hairline, Shapes.card)
                    .then(clickable)
                    .padding(12.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                head(Modifier)
                if (preview.isNotEmpty()) ScreenPreview(preview.takeLast(1))
            }
            else -> Box(
                Modifier
                    .fillMaxWidth()
                    .alpha(if (status == SessionStatus.Exited) 0.6f else 1f)
                    .background(colors.card, Shapes.card)
                    .border(1.dp, colors.hairline, Shapes.card)
                    .then(clickable)
                    .padding(horizontal = 12.dp, vertical = 8.dp),
            ) { head(Modifier) }
        }
        DropdownMenu(expanded = menu, onDismissRequest = { menu = false; taskSource = null }) {
            taskSource?.let { source ->
                TaskListItems(model, session, source, onBack = { taskSource = null }) { ranHere ->
                    menu = false
                    taskSource = null
                    if (ranHere) open()
                }
                return@DropdownMenu
            }
            DropdownMenuItem(text = { Text(stringResource(R.string.action_open)) }, onClick = { menu = false; open() })
            session.meta.cwd?.let { cwd ->
                DropdownMenuItem(text = { Text(stringResource(R.string.git)) }, onClick = { menu = false; onOpenGit(session.id) })
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.sessions_copy_folder)) },
                    onClick = {
                        menu = false
                        context.getSystemService(ClipboardManager::class.java)?.setPrimaryClip(ClipData.newPlainText(cwd, cwd))
                    },
                )
            }
            TaskSourceItems(model, session) { taskSource = it }
            HorizontalDivider(thickness = 1.dp, color = colors.hairline)
            Text(
                "${Presentation.gridSize(session.size)} · ${Presentation.sizeOwner(session.sizeOwner).resolve()}",
                style = Type.caption,
                color = colors.secondary,
                modifier = Modifier.padding(horizontal = 12.dp, vertical = 6.dp),
            )
            DropdownMenuItem(
                text = { Text(stringResource(R.string.end_session), color = colors.failure) },
                onClick = { menu = false; model.killTarget = session.id },
            )
        }
    }
}

/**
 * 会话卡片的头部：状态标记，标题，下面一行说状态（[Presentation.sessionSubtitle]），末尾是标签页号；会话不在工作区的目录
 * 里时再起一行写它的目录。状态由那行字说，读屏不念标记。
 */
@Composable
private fun SessionRow(session: SessionInfo, workspaceDir: String?, tab: Int?, modifier: Modifier) {
    val colors = themeColors
    Row(
        modifier
            .fillMaxWidth()
            .defaultMinSize(minHeight = 42.dp)
            .semantics(mergeDescendants = true) {}
            .padding(horizontal = 2.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        StatusGlyph(StatusMark.of(SessionStatus.of(session)), 18.dp)
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(1.dp)) {
            Text(
                Presentation.sessionTitle(session).resolve(),
                style = Type.callout.copy(fontWeight = FontWeight.SemiBold),
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                Text(
                    Presentation.sessionSubtitle(session, now = agentClock(session.meta.agent, session.meta.agentSinceMs)).resolve(),
                    style = Type.footnote,
                    color = colors.secondary,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f, fill = false),
                )
                if (tab != null) {
                    Text(stringResource(R.string.session_tab, tab), style = Type.caption, color = colors.tertiary, maxLines = 1)
                }
            }
            Presentation.sessionDirectory(session.meta.cwd, workspaceDir)?.let { directory ->
                Text(
                    directory,
                    style = Type.caption,
                    fontFamily = FontFamily.Monospace,
                    color = colors.tertiary,
                    maxLines = 1,
                    overflow = TextOverflow.StartEllipsis,
                )
            }
        }
        DisclosureChevron()
    }
}

/** 给工作区改名的输入框，名字为空时存不了。 */
@Composable
private fun RenameWorkspaceDialog(section: SessionSection, onDismiss: () -> Unit, onSave: (String) -> Unit) {
    val initial = Presentation.sectionTitle(section).resolve()
    var text by remember(section) { mutableStateOf(initial) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.sessions_rename_workspace)) },
        text = {
            OutlinedTextField(
                value = text,
                onValueChange = { text = it },
                singleLine = true,
                label = { Text(stringResource(R.string.name_field)) },
            )
        },
        confirmButton = {
            TextButton(
                enabled = text.isNotBlank(),
                onClick = {
                    onDismiss()
                    onSave(text)
                },
            ) { Text(stringResource(R.string.action_save)) }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.action_cancel)) } },
    )
}
