package dev.runode.mobile.features.ui.terminal

import android.content.Intent
import android.net.Uri
import android.provider.Settings
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.scaleIn
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
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
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.windowInsetsBottomHeight
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.onClick
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.runode.mobile.connection.LinkState
import dev.runode.mobile.connection.MachineRecord
import dev.runode.mobile.features.R
import dev.runode.mobile.features.model.AgentPrompt
import dev.runode.mobile.features.model.AppPreferences
import dev.runode.mobile.features.model.DictationModel
import dev.runode.mobile.features.model.DictationRemedy
import dev.runode.mobile.features.model.Presentation
import dev.runode.mobile.features.model.QuickKey
import dev.runode.mobile.features.model.QuickReplyModel
import dev.runode.mobile.features.model.SessionListModel
import dev.runode.mobile.features.model.SizePreference
import dev.runode.mobile.features.model.SplitPane
import dev.runode.mobile.features.model.TerminalModel
import dev.runode.mobile.features.ui.AppIcons
import dev.runode.mobile.features.ui.CircleIconButton
import dev.runode.mobile.features.ui.ConfirmDialog
import dev.runode.mobile.features.ui.HapticOnChange
import dev.runode.mobile.features.ui.TopBar
import dev.runode.mobile.features.ui.agentClock
import dev.runode.mobile.features.ui.linkClock
import dev.runode.mobile.features.ui.sessions.TaskListItems
import dev.runode.mobile.features.ui.sessions.TaskSourceItems
import dev.runode.mobile.features.ui.theme.ActionKind
import dev.runode.mobile.features.ui.theme.CornerRadius
import dev.runode.mobile.features.ui.theme.RunodeTheme
import dev.runode.mobile.features.ui.theme.Shapes
import dev.runode.mobile.features.ui.theme.StatusGlyph
import dev.runode.mobile.features.ui.theme.StatusMark
import dev.runode.mobile.features.ui.theme.SurfaceButton
import dev.runode.mobile.features.ui.theme.Type
import dev.runode.mobile.features.ui.theme.resolve
import dev.runode.mobile.features.ui.theme.themeColors
import dev.runode.mobile.features.ui.theme.toColor
import dev.runode.mobile.protocol.GridSize
import dev.runode.mobile.protocol.SessionId
import dev.runode.mobile.protocol.TaskSource
import dev.runode.mobile.terminal.DictationPress
import dev.runode.mobile.terminal.KeyBarPalette
import dev.runode.mobile.terminal.KeyboardAccessoryBar
import dev.runode.mobile.terminal.Terminal
import dev.runode.mobile.terminal.TerminalInput
import dev.runode.mobile.terminal.TerminalView
import dev.runode.mobile.terminal.TerminalViewDelegate
import kotlinx.coroutines.delay

/**
 * 终端页：顶栏是会话名和连接状态，分了屏时下面一排分屏标签；中间是终端，断线时顶上叠一条横幅，结束了斜盖一枚印章；
 * 底下是等你回答时的回答行和按键栏（软键盘弹出时它跟在键盘上面）。整页按这个会话的主题（[TerminalModel.chromeTheme]）上色。
 *
 * @param sessions 这台电脑的会话列表：分屏、认出来的提问从它取
 * @param dictation 语音输入；为空时麦克风键不起作用
 * @param machine 这个终端所在的电脑，交给电脑识别时连它
 * @param onPaneEnded 显示的分屏结束了，带着它所在标签里的分屏
 * @param onOpenSettings 打开 app 的设置页：语音输入缺模型时从弹框过去下载
 */
@Composable
fun TerminalScreen(
    model: TerminalModel,
    preferences: AppPreferences,
    sessions: SessionListModel?,
    dictation: DictationModel?,
    machine: MachineRecord?,
    onBack: () -> Unit,
    onOpenGit: () -> Unit,
    onSelectPane: (SessionId) -> Unit,
    onPaneEnded: (List<SessionId>) -> Unit,
    onOpenSettings: () -> Unit,
) {
    RunodeTheme(model.chromeTheme) {
        TerminalPage(model, preferences, sessions, dictation, machine, onBack, onOpenGit, onSelectPane, onPaneEnded, onOpenSettings)
    }
}

@Composable
private fun TerminalPage(
    model: TerminalModel,
    preferences: AppPreferences,
    sessions: SessionListModel?,
    dictation: DictationModel?,
    machine: MachineRecord?,
    onBack: () -> Unit,
    onOpenGit: () -> Unit,
    onSelectPane: (SessionId) -> Unit,
    onPaneEnded: (List<SessionId>) -> Unit,
    onOpenSettings: () -> Unit,
) {
    val colors = themeColors
    val density = LocalDensity.current
    var terminalView by remember { mutableStateOf<TerminalView?>(null) }
    var bannerHeight by remember { mutableStateOf(0.dp) }
    // 连了一会儿还没连上才出「正在连接…」横幅，切分屏、回到前台时一闪而过的连接不出。
    var connectingLong by remember { mutableStateOf(false) }
    // 最后一次知道的、这个会话所在标签里的分屏：会话结束后电脑关掉它的分屏，布局里就找不到它了。
    var tabMates by remember(model) { mutableStateOf<List<SessionId>>(emptyList()) }
    val panes = sessions?.panes(sharingTabWith = model.sessionId) ?: emptyList()
    val paneIds = panes.map { it.id }

    val connecting = model.phase == TerminalModel.Phase.Connecting
    LaunchedEffect(model, connecting) {
        connectingLong = false
        if (!connecting) return@LaunchedEffect
        delay(CONNECTING_BANNER_DELAY_MS)
        connectingLong = true
    }
    // 之后电脑在布局变了时推 `LayoutChanged`，列表收到就重新要。
    LaunchedEffect(sessions) { sessions?.refreshLayout() }
    val cwd = sessions?.session(model.sessionId)?.meta?.cwd
    LaunchedEffect(sessions, cwd) { if (cwd != null) sessions?.loadTasks(cwd) }
    LaunchedEffect(paneIds) { if (paneIds.isNotEmpty()) tabMates = paneIds }
    LaunchedEffect(model, model.isEnded) {
        if (model.isEnded && model.sessionId in tabMates) onPaneEnded(tabMates)
    }
    DisposableEffect(dictation) { onDispose { dictation?.cancel() } }
    val recording = dictation?.phase == DictationModel.Phase.Recording
    val haptics = LocalHapticFeedback.current
    LaunchedEffect(recording) { if (recording) haptics.performHapticFeedback(HapticFeedbackType.LongPress) }

    val onDictation by rememberUpdatedState<(DictationPress) -> Unit> { press ->
        dictation?.press(press, machine, preferences.dictationSource) { text -> model.send(TerminalInput.Paste(text)) }
    }
    val delegate = remember(model) { Delegate(model) { onDictation(it) } }
    val banner = bannerMessage(model, connectingLong)

    Column(Modifier.fillMaxSize().background(colors.page)) {
        TopBar(onBack = onBack, actions = { MoreMenu(model, sessions, onOpenGit) }) {
            TitleView(model, panes, sessions, machine)
        }
        if (panes.isNotEmpty()) {
            PaneTabBar(panes, model.sessionId, onSelectPane)
            HorizontalDivider(thickness = 1.dp, color = colors.hairline)
        }
        Box(Modifier.weight(1f).fillMaxWidth().background(model.background.toColor())) {
            // 换了分屏时整个终端视图重建，新的视图按自己的大小报「适配手机」的网格。
            androidx.compose.runtime.key(model) {
                Terminal(
                    delegate = delegate,
                    modifier = Modifier.fillMaxSize(),
                    topObstruction = if (banner == null) 0.dp else bannerHeight,
                    fontSizeOverride = preferences.fontSize?.toFloat(),
                    bellHaptics = preferences.bellHaptics,
                    acceptsInput = !model.isEnded,
                    dictating = recording,
                    onMake = { view ->
                        terminalView = view
                        model.attachDisplay(view)
                    },
                    onRelease = { view ->
                        model.detachDisplay(view)
                        if (terminalView === view) terminalView = null
                    },
                )
            }
            if (banner != null) {
                Banner(
                    banner,
                    model,
                    Modifier
                        .align(Alignment.TopCenter)
                        .onSizeChanged { bannerHeight = with(density) { it.height.toDp() } + 8.dp },
                )
            }
            androidx.compose.animation.AnimatedVisibility(
                visible = model.isEnded,
                enter = scaleIn(initialScale = 1.4f) + fadeIn(),
                exit = fadeOut(),
                modifier = Modifier.align(Alignment.Center),
            ) { EndedStamp(model.phase) }
            dictation?.phase?.takeIf { it != DictationModel.Phase.Idle }?.let { phase ->
                DictationBadge(phase, Modifier.align(Alignment.BottomCenter).padding(bottom = 8.dp))
            }
            if (model.scrolledBack) {
                SurfaceButton(
                    ActionKind.Primary,
                    onClick = { model.scrollToBottom() },
                    height = 48.dp,
                    cornerRadius = 24.dp,
                    contentPadding = 18.dp,
                    modifier = Modifier.align(Alignment.BottomEnd).padding(16.dp),
                ) {
                    Icon(AppIcons.jumpToLatest, contentDescription = null, modifier = Modifier.size(18.dp))
                    Spacer(Modifier.width(8.dp))
                    Text(stringResource(R.string.terminal_jump_to_latest), style = Type.subheadline)
                }
            }
        }
        BottomBars(model, sessions?.prompts?.get(model.sessionId), terminalView)
    }

    DictationErrorDialog(dictation, onOpenSettings)
    if (model.isConfirmingKill) {
        ConfirmDialog(
            title = stringResource(R.string.end_terminal_title),
            message = stringResource(R.string.end_terminal_detail),
            confirm = stringResource(R.string.end_session),
            onConfirm = { model.kill() },
            onDismiss = { model.isConfirmingKill = false },
        )
    }
}

private const val CONNECTING_BANNER_DELAY_MS = 600L

/** 把终端视图报上来的事交给视图模型；语音输入的按键交给 [onDictation]。 */
private class Delegate(private val model: TerminalModel, private val onDictation: (DictationPress) -> Unit) : TerminalViewDelegate {
    override fun didInput(view: TerminalView, input: TerminalInput) = model.send(input)

    override fun fitSizeDidChange(view: TerminalView, size: GridSize) = model.updateFitSize(size)

    override fun didScrollBack(view: TerminalView, scrolledBack: Boolean) {
        model.scrolledBack = scrolledBack
    }

    override fun keyboardVisible(view: TerminalView, visible: Boolean) {
        model.keyboardVisible = visible
    }

    override fun dictation(view: TerminalView, press: DictationPress) = onDictation(press)
}

/** 分屏时标题是工作区的名字，当前会话由下面的分屏标签标出；没分屏时是会话名，前面带它的状态。下面一行是电脑和连接状态。 */
@Composable
private fun androidx.compose.foundation.layout.RowScope.TitleView(
    model: TerminalModel,
    panes: List<SplitPane>,
    sessions: SessionListModel?,
    machine: MachineRecord?,
) {
    val colors = themeColors
    val now = linkClock(model.linkState)
    val title = model.title.resolve()
    // 左右的返回键和「⋯」一样宽，标题在中间的一栏里居中就是在屏幕上居中。
    Column(
        Modifier.weight(1f).semantics(mergeDescendants = true) { heading() },
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            if (panes.isEmpty()) {
                StatusGlyph(model.agent?.let { StatusMark.of(it.state) } ?: StatusMark.Idle, 15.dp)
                Text(title, style = Type.headline, maxLines = 1, overflow = TextOverflow.Ellipsis)
            } else {
                val section = sessions?.sections?.firstOrNull { section -> section.sessions.any { it.id == model.sessionId } }
                Text(
                    section?.let { Presentation.sectionTitle(it).resolve() } ?: title,
                    style = Type.headline,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        Text(
            Presentation.terminalSubtitle(machine?.name, model.linkState, model.phase, now).resolve(),
            style = Type.caption,
            color = colors.secondary,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/**
 * 顶栏右边的「⋯」菜单：键盘、回到最新、终端尺寸、Git、会话目录里的命令、结束会话。会话结束了只剩回到最新和尺寸说明。
 * 命令在这个会话里跑了时回到最新。
 */
@Composable
private fun MoreMenu(model: TerminalModel, sessions: SessionListModel?, onOpenGit: () -> Unit) {
    val colors = themeColors
    var expanded by remember { mutableStateOf(false) }
    var taskSource by remember { mutableStateOf<TaskSource?>(null) }
    val session = sessions?.session(model.sessionId)
    Box {
        CircleIconButton(AppIcons.more, stringResource(R.string.terminal_options), { expanded = true })
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false; taskSource = null }) {
            val source = taskSource
            if (source != null && sessions != null && session != null) {
                TaskListItems(sessions, session, source, onBack = { taskSource = null }) { ranHere ->
                    expanded = false
                    taskSource = null
                    if (ranHere) model.scrollToBottom()
                }
                return@DropdownMenu
            }
            val ended = model.isEnded
            if (!ended) {
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.terminal_keyboard)) },
                    leadingIcon = { Icon(AppIcons.keyboard, null, Modifier.size(18.dp)) },
                    onClick = { expanded = false; model.showKeyboard() },
                )
            }
            DropdownMenuItem(
                text = { Text(stringResource(R.string.terminal_jump_to_latest)) },
                leadingIcon = { Icon(AppIcons.jumpToLatest, null, Modifier.size(18.dp)) },
                onClick = { expanded = false; model.scrollToBottom() },
            )
            HorizontalDivider(thickness = 1.dp, color = colors.hairline)
            Text(
                stringResource(R.string.terminal_size),
                style = Type.footnote.copy(fontWeight = FontWeight.SemiBold),
                color = colors.secondary,
                modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp).semantics { heading() },
            )
            // 会话结束了，尺寸改不了了。
            for (preference in SizePreference.entries) {
                val chosen = model.sizePreference == preference
                DropdownMenuItem(
                    text = { Text(Presentation.sizePreference(preference).resolve()) },
                    trailingIcon = if (chosen) ({ Icon(AppIcons.check, null, Modifier.size(18.dp)) }) else null,
                    enabled = !ended,
                    onClick = { expanded = false; model.sizePreference = preference },
                    modifier = Modifier.semantics { selected = chosen },
                )
            }
            Column(Modifier.padding(horizontal = 12.dp, vertical = 6.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(Presentation.sizeOwnership(model.sizeOwnership).resolve(), style = Type.footnote, color = colors.secondary)
                model.gridSize?.let { size ->
                    Text(stringResource(R.string.terminal_grid, Presentation.gridSize(size)), style = Type.footnote, color = colors.secondary)
                }
            }
            // 会话结束了宿主那边就没有它了，Git 请求只会回「no session」。
            if (!ended) {
                HorizontalDivider(thickness = 1.dp, color = colors.hairline)
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.git)) },
                    leadingIcon = { Icon(AppIcons.branch, null, Modifier.size(18.dp)) },
                    onClick = { expanded = false; onOpenGit() },
                )
                if (sessions != null && session != null) TaskSourceItems(sessions, session) { taskSource = it }
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.end_session), color = colors.failure) },
                    leadingIcon = { Icon(AppIcons.close, null, Modifier.size(18.dp), tint = colors.failure) },
                    onClick = { expanded = false; model.isConfirmingKill = true },
                )
            }
        }
    }
}

/** 横幅上的内容：断线时的连接状态，或者连了一会儿还没连上。 */
private sealed class BannerMessage {
    data class Link(val state: LinkState) : BannerMessage()
    data object Connecting : BannerMessage()
}

private fun bannerMessage(model: TerminalModel, connectingLong: Boolean): BannerMessage? = when (model.phase) {
    is TerminalModel.Phase.Disconnected -> BannerMessage.Link(model.linkState)
    TerminalModel.Phase.Connecting -> if (connectingLong) BannerMessage.Connecting else null
    else -> null
}

/** 叠在终端顶上的横幅：图标、文字，断线时右边有「重试」。 */
@Composable
private fun Banner(message: BannerMessage, model: TerminalModel, modifier: Modifier = Modifier) {
    val colors = themeColors
    val text = when (message) {
        BannerMessage.Connecting -> stringResource(R.string.terminal_connecting)
        is BannerMessage.Link -> {
            val now = linkClock(message.state)
            when (val state = message.state) {
                is LinkState.Failed -> Presentation.linkFailure(state.failure).resolve()
                else -> Presentation.linkStatus(state, now).resolve()
            }
        }
    }
    Row(
        modifier
            .padding(horizontal = 12.dp)
            .padding(top = 8.dp)
            .fillMaxWidth()
            .defaultMinSize(minHeight = 44.dp)
            .background(colors.card, Shapes.card)
            .border(1.dp, colors.hairline, Shapes.card)
            .padding(horizontal = 14.dp, vertical = 6.dp)
            .semantics { liveRegion = LiveRegionMode.Polite },
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        when (message) {
            BannerMessage.Connecting -> CircularProgressIndicator(Modifier.size(16.dp), color = colors.secondary, strokeWidth = 2.dp)
            is BannerMessage.Link -> Icon(AppIcons.offline, null, tint = colors.waiting, modifier = Modifier.size(18.dp))
        }
        Text(text, style = Type.subheadline, modifier = Modifier.weight(1f))
        if (message is BannerMessage.Link) {
            SurfaceButton(ActionKind.Raised, onClick = { model.reconnect() }, contentPadding = 14.dp) {
                Text(stringResource(R.string.action_retry), style = Type.subheadline)
            }
        }
    }
}

/** 会话结束了时斜盖在终端上的印章：大字「已结束」，下面一行小字说怎么结束的。点不到它，终端照样能滚动。 */
@Composable
private fun EndedStamp(phase: TerminalModel.Phase) {
    val color = themeColors.failure.copy(alpha = 0.75f)
    val detail = when (phase) {
        is TerminalModel.Phase.Exited -> phase.status?.let { stringResource(R.string.terminal_exited_status, it) }
            ?: stringResource(R.string.terminal_exited)
        is TerminalModel.Phase.Gone -> phase.reason.resolve()
        else -> ""
    }
    val ended = stringResource(R.string.terminal_ended)
    Column(
        Modifier
            .widthIn(max = 280.dp)
            .rotate(-12f)
            .border(3.dp, color, RoundedCornerShape(10.dp))
            .padding(horizontal = 20.dp, vertical = 12.dp)
            .clearAndSetSemantics { contentDescription = "$ended, $detail" },
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Text(ended, style = Type.largeTitle.copy(fontWeight = FontWeight.Black), color = color)
        if (detail.isNotEmpty()) {
            Text(
                detail,
                style = Type.footnote.copy(fontWeight = FontWeight.SemiBold),
                color = color,
                textAlign = TextAlign.Center,
                maxLines = 2,
            )
        }
    }
}

/** 终端底部正中的提示：正在听、正在识别。 */
@Composable
private fun DictationBadge(phase: DictationModel.Phase, modifier: Modifier = Modifier) {
    val colors = themeColors
    Row(
        modifier
            .background(colors.card, CircleShape)
            .border(1.dp, colors.hairline, CircleShape)
            .padding(horizontal = 12.dp, vertical = 6.dp)
            .semantics(mergeDescendants = true) { liveRegion = LiveRegionMode.Polite },
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        if (phase == DictationModel.Phase.Recording) {
            Box(Modifier.size(8.dp).background(colors.failure, CircleShape))
        } else {
            CircularProgressIndicator(Modifier.size(10.dp), color = colors.secondary, strokeWidth = 1.5.dp)
        }
        Text(
            stringResource(if (phase == DictationModel.Phase.Recording) R.string.terminal_listening else R.string.terminal_recognizing),
            style = Type.footnote,
        )
    }
}

/**
 * 底部的回答行和按键栏。按键栏铺满宽度，软键盘弹出时跟在键盘上面；会话结束了不出，这时只给导航栏让出地方。
 */
@Composable
private fun BottomBars(model: TerminalModel, prompt: AgentPrompt?, view: TerminalView?) {
    val colors = themeColors
    val bar = view?.takeIf { !model.isEnded }
    val showsBar = bar != null
    Column(Modifier.fillMaxWidth().background(colors.page)) {
        if (model.isAwaitingAnswer || showsBar) HorizontalDivider(thickness = 1.dp, color = colors.hairline)
        if (model.isAwaitingAnswer) {
            AnswerRow(
                model.quickReply,
                prompt,
                model.agent?.kind?.displayName,
                Modifier.padding(horizontal = 14.dp).padding(top = 8.dp, bottom = if (showsBar) 0.dp else 8.dp),
            )
        }
        if (bar != null) {
            KeyboardAccessoryBar(
                bar,
                palette = KeyBarPalette(
                    key = colors.card,
                    border = colors.hairline,
                    label = colors.primary,
                    selectedKey = colors.primary,
                    selectedLabel = colors.page,
                ),
            )
        } else {
            Spacer(Modifier.windowInsetsBottomHeight(WindowInsets.navigationBars))
        }
    }
}

/**
 * 终端页底部回答 agent 的那一行：琥珀标记加「等你回答」，右边几个选项编号键；一行放不下时键折到下一行，再放不下就横着滚。
 * 只有键，不带输入框：终端本身就能打字。
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun AnswerRow(model: QuickReplyModel, prompt: AgentPrompt?, agentName: String?, modifier: Modifier = Modifier) {
    val colors = themeColors
    HapticOnChange(model.deliveredCount, HapticFeedbackType.Confirm)
    HapticOnChange(model.failedCount, HapticFeedbackType.Reject)
    val label = stringResource(R.string.reply_to, agentName ?: "Agent")
    Column(modifier.semantics { contentDescription = label }, verticalArrangement = Arrangement.spacedBy(6.dp)) {
        FlowRow(
            Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Row(
                Modifier.height(44.dp).padding(start = 4.dp, end = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(7.dp),
            ) {
                StatusGlyph(StatusMark.Waiting, 15.dp)
                Text(
                    stringResource(R.string.terminal_waiting_for_you),
                    style = Type.footnote.copy(fontWeight = FontWeight.SemiBold),
                    color = colors.waiting,
                )
            }
            Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                if (prompt != null) {
                    for (option in prompt.options) {
                        AnswerKey(model, option.key, if (option.selected) ActionKind.Waiting else ActionKind.Secondary, option.selected)
                    }
                } else {
                    for (key in QuickKey.options) AnswerKey(model, key, ActionKind.Secondary, null)
                }
            }
        }
        model.errorMessage?.let { error ->
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
                Icon(AppIcons.warning, contentDescription = null, tint = colors.failure, modifier = Modifier.size(14.dp))
                Text(error.resolve(), style = Type.footnote, color = colors.failure)
            }
        }
    }
}

@Composable
private fun AnswerKey(model: QuickReplyModel, key: QuickKey, kind: ActionKind, selected: Boolean?) {
    val description = stringResource(R.string.send_key, key.accessibilityLabel.resolve())
    SurfaceButton(
        kind,
        onClick = { model.press(key) },
        enabled = model.isConnected,
        cornerRadius = CornerRadius.key,
        selected = selected,
        modifier = Modifier.width(56.dp).semantics { contentDescription = description },
    ) {
        Text(key.label, style = Type.subheadline.copy(fontWeight = FontWeight.Bold), fontFamily = FontFamily.Monospace)
    }
}

/** 终端页顶上的一排分屏标签，一个分屏一个：状态标记加会话名。点了切过去；当前的那个滚进视野。 */
@Composable
private fun PaneTabBar(panes: List<SplitPane>, current: SessionId, onSelect: (SessionId) -> Unit) {
    val colors = themeColors
    val state = rememberLazyListState()
    val haptics = LocalHapticFeedback.current
    LaunchedEffect(current, panes.size) {
        val index = panes.indexOfFirst { it.id == current }
        if (index >= 0) state.animateScrollToItem(index)
    }
    val switchHint = stringResource(R.string.terminal_switch_pane)
    LazyRow(
        state = state,
        contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 10.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        modifier = Modifier.fillMaxWidth().background(colors.page),
    ) {
        items(panes, key = { it.id.toString() }) { pane ->
            val chosen = pane.id == current
            val title = Presentation.sessionTitle(pane.session).resolve()
            val meta = pane.session.meta
            val status = Presentation.agentStatus(meta.agent, meta.agentSinceMs, agentClock(meta.agent, meta.agentSinceMs))
            val statusText = status?.first?.resolve()
            val shape = RoundedCornerShape(13.dp)
            Row(
                Modifier
                    .widthIn(max = 220.dp)
                    .defaultMinSize(minHeight = 44.dp)
                    .background(if (chosen) colors.raised else androidx.compose.ui.graphics.Color.Transparent, shape)
                    .clickable(role = Role.Tab, onClickLabel = if (chosen) null else switchHint) {
                        if (!chosen) {
                            haptics.performHapticFeedback(HapticFeedbackType.SegmentTick)
                            onSelect(pane.id)
                        }
                    }
                    .semantics(mergeDescendants = true) {
                        selected = chosen
                        if (statusText != null) stateDescription = statusText
                    }
                    .alpha(if (pane.session.exited) 0.5f else 1f)
                    .padding(horizontal = 14.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                StatusGlyph(status?.let { StatusMark.of(it.second) } ?: StatusMark.Idle, 15.dp)
                Text(
                    title,
                    style = Type.subheadline.copy(fontWeight = if (chosen) FontWeight.SemiBold else FontWeight.Normal),
                    color = if (chosen) colors.primary else colors.secondary,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
    }
}

/** 语音输入出错的弹框；缺模型时多一个去设置页下载，没有麦克风权限时多一个去系统设置。 */
@Composable
private fun DictationErrorDialog(dictation: DictationModel?, onOpenSettings: () -> Unit) {
    val message = dictation?.errorMessage ?: return
    val context = LocalContext.current
    val dismiss = { dictation.errorMessage = null }
    AlertDialog(
        onDismissRequest = dismiss,
        title = { Text(stringResource(R.string.dictation_failed)) },
        text = { Text(message.resolve()) },
        confirmButton = {
            when (dictation.remedy) {
                DictationRemedy.DownloadModel -> TextButton(onClick = { dismiss(); onOpenSettings() }) {
                    Text(stringResource(R.string.dictation_download))
                }
                DictationRemedy.MicrophonePermission -> TextButton(onClick = {
                    dismiss()
                    context.startActivity(
                        Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", context.packageName, null))
                            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                    )
                }) { Text(stringResource(R.string.open_system_settings)) }
                null -> TextButton(onClick = dismiss) { Text(stringResource(R.string.action_ok)) }
            }
        },
        dismissButton = if (dictation.remedy == null) {
            null
        } else {
            { TextButton(onClick = dismiss) { Text(stringResource(R.string.action_cancel)) } }
        },
    )
}
