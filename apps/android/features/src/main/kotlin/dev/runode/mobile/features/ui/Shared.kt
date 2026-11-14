package dev.runode.mobile.features.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius as GeometryCornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import dev.runode.mobile.connection.LinkState
import dev.runode.mobile.features.R
import dev.runode.mobile.features.model.AgentClock
import dev.runode.mobile.features.model.AgentPrompt
import dev.runode.mobile.features.model.InstalledAgent
import dev.runode.mobile.features.model.Presentation
import dev.runode.mobile.features.model.QuickKey
import dev.runode.mobile.features.model.QuickReplyModel
import dev.runode.mobile.features.ui.theme.ActionKind
import dev.runode.mobile.features.ui.theme.Shapes
import dev.runode.mobile.features.ui.theme.SurfaceButton
import dev.runode.mobile.features.ui.theme.Type
import dev.runode.mobile.features.ui.theme.resolve
import dev.runode.mobile.features.ui.theme.themeColors
import dev.runode.mobile.protocol.Agent
import dev.runode.mobile.protocol.AgentKind
import java.time.Instant
import kotlinx.coroutines.delay

/** 顶栏上的圆键：44dp 的圆，卡片的底加细线；[prominent] 时是前景色的实心圆，图标用页面的底色。 */
@Composable
internal fun CircleIconButton(
    icon: ImageVector,
    label: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    prominent: Boolean = false,
    enabled: Boolean = true,
) {
    val colors = themeColors
    val background = if (prominent) colors.primary else colors.card
    val tint = if (prominent) colors.page else colors.primary
    Box(
        modifier = modifier
            .size(44.dp)
            .background(background, CircleShape)
            .then(if (prominent) Modifier else Modifier.border(1.dp, colors.hairline, CircleShape))
            .clickable(enabled = enabled, role = Role.Button, onClick = onClick)
            .semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) {
        Icon(icon, contentDescription = null, tint = if (enabled) tint else tint.copy(alpha = 0.4f), modifier = Modifier.size(18.dp))
    }
}

/** 页面顶上的一条：让开状态栏，左边可选的返回键，中间 [title]，右边 [actions]。 */
@Composable
internal fun TopBar(
    modifier: Modifier = Modifier,
    background: Color = themeColors.page,
    onBack: (() -> Unit)? = null,
    actions: @Composable RowScope.() -> Unit = {},
    title: @Composable RowScope.() -> Unit,
) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .background(background)
            .windowInsetsPadding(WindowInsets.statusBars)
            .height(60.dp)
            .padding(horizontal = 16.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        if (onBack != null) CircleIconButton(AppIcons.back, stringResource(R.string.action_back), onBack)
        Row(Modifier.weight(1f), verticalAlignment = Alignment.CenterVertically, content = title)
        actions()
    }
}

/** 顶栏左边的标题，下面可以再带一行小字，前面可以带应用的标志。 */
@Composable
internal fun RowScope.LeadingTitle(title: String, subtitle: String? = null, showsMark: Boolean = false) {
    if (showsMark) {
        RunodeMark(Modifier.size(28.dp))
        Spacer(Modifier.width(8.dp))
    }
    Column(Modifier.weight(1f).semantics(mergeDescendants = true) { heading() }) {
        Text(title, style = Type.headline, maxLines = 1, overflow = TextOverflow.Ellipsis)
        if (subtitle != null) {
            Text(subtitle, style = Type.caption, color = themeColors.secondary, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
    }
}

/** 连接状态的文字用的时刻：只有等着重连时带倒计时，要按秒刷新；别的状态下不随时间变。 */
@Composable
internal fun linkClock(state: LinkState): Instant {
    val now by produceState(Instant.now(), state) {
        value = Instant.now()
        if (state !is LinkState.Waiting) return@produceState
        while (true) {
            delay(1000 - System.currentTimeMillis() % 1000)
            value = Instant.now()
        }
    }
    return now
}

/** 卡片右边表示能点进去的小箭头。 */
@Composable
internal fun DisclosureChevron(modifier: Modifier = Modifier) {
    Icon(AppIcons.chevronRight, contentDescription = null, tint = themeColors.tertiary, modifier = modifier.size(16.dp))
}

/** 一列卡片上面的标题。 */
@Composable
internal fun ListSectionHeader(title: String, modifier: Modifier = Modifier) {
    Text(
        title,
        style = Type.footnote.copy(fontWeight = FontWeight.SemiBold),
        color = themeColors.secondary,
        modifier = modifier
            .padding(start = 4.dp, top = 12.dp)
            .semantics { heading() },
    )
}

/** 屏幕预览的字是哪种深浅：一般是说明色；等你回答的卡片上要读的是 agent 的提问，用正文色。 */
internal enum class PreviewTone { Primary, Secondary }

/** 屏幕最后几行的预览：等宽小字，凹下去的底槽，每行单独截断（长行折下去会把别的行挤掉）。 */
@Composable
internal fun ScreenPreview(lines: List<String>, modifier: Modifier = Modifier, tone: PreviewTone = PreviewTone.Secondary) {
    val colors = themeColors
    val description = stringResource(R.string.screen_preview, lines.joinToString(stringResource(R.string.list_separator)))
    Column(
        modifier = modifier
            .fillMaxWidth()
            .background(colors.well, Shapes.inner)
            .padding(horizontal = 12.dp, vertical = 9.dp)
            .clearAndSetSemantics { contentDescription = description },
        verticalArrangement = Arrangement.spacedBy(3.dp),
    ) {
        val color = if (tone == PreviewTone.Primary) colors.primary.copy(alpha = 0.85f) else colors.secondary
        for (line in lines) {
            Text(line, style = Type.caption, fontFamily = FontFamily.Monospace, color = color, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
    }
}

/** 提问的选项怎么排。 */
internal enum class PromptOptionLayout {
    /** 一项一行，编号块加原文：首页等你回答的卡片。 */
    Rows,

    /** [AgentPrompt.compactOptions] 并排成键，还有别的选项时全部收进「⋯」菜单：会话列表的卡片。 */
    Compact,
}

/**
 * 认出来的提问（[AgentPrompt]）的选项，按下去交给 [onPress] 发编号键。默认项是琥珀色，其余中性；原文在 [PromptOptionLayout.Rows]
 * 里至多折成两行、在 [PromptOptionLayout.Compact] 里只占一行，再长就截断。读屏念「发送 编号：原文」，默认项是选中的。
 */
@Composable
internal fun PromptOptions(prompt: AgentPrompt, layout: PromptOptionLayout, enabled: Boolean, onPress: (QuickKey) -> Unit) {
    val colors = themeColors
    when (layout) {
        PromptOptionLayout.Rows -> Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
            for (option in prompt.options) {
                val label = stringResource(R.string.send_key, option.key.accessibilityLabel.resolve())
                SurfaceButton(
                    kind = if (option.selected) ActionKind.Waiting else ActionKind.Raised,
                    onClick = { onPress(option.key) },
                    enabled = enabled,
                    height = 46.dp,
                    selected = option.selected,
                    modifier = Modifier.fillMaxWidth().semantics { contentDescription = label },
                ) {
                    Row(
                        Modifier.fillMaxWidth().padding(start = 10.dp, end = 14.dp, top = 6.dp, bottom = 6.dp),
                        horizontalArrangement = Arrangement.spacedBy(12.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Box(
                            Modifier
                                .size(24.dp)
                                .background(
                                    if (option.selected) colors.onWaitingFill.copy(alpha = 0.18f) else colors.well,
                                    RoundedCornerShape(7.dp),
                                ),
                            contentAlignment = Alignment.Center,
                        ) {
                            Text(
                                option.number.toString(),
                                style = Type.footnote.copy(fontWeight = FontWeight.Normal),
                                fontFamily = FontFamily.Monospace,
                            )
                        }
                        Text(
                            option.text,
                            style = Type.subheadline.copy(fontWeight = FontWeight.Normal),
                            maxLines = 2,
                            overflow = TextOverflow.Ellipsis,
                            modifier = Modifier.weight(1f),
                        )
                    }
                }
            }
        }
        PromptOptionLayout.Compact -> Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            val shown = prompt.compactOptions
            for (option in shown) {
                val label = stringResource(R.string.send_key, option.key.accessibilityLabel.resolve())
                SurfaceButton(
                    kind = if (option.selected) ActionKind.Waiting else ActionKind.Raised,
                    onClick = { onPress(option.key) },
                    enabled = enabled,
                    selected = option.selected,
                    contentPadding = 12.dp,
                    modifier = Modifier.weight(1f).semantics { contentDescription = label },
                ) {
                    Text(
                        option.number.toString(),
                        style = Type.callout.copy(fontWeight = FontWeight.Normal),
                        fontFamily = FontFamily.Monospace,
                        color = if (option.selected) colors.onWaitingFill.copy(alpha = 0.6f) else colors.secondary,
                    )
                    Spacer(Modifier.width(6.dp))
                    Text(option.text, style = Type.callout, maxLines = 1, overflow = TextOverflow.Ellipsis)
                }
            }
            if (prompt.options.size > shown.size) {
                var expanded by remember { mutableStateOf(false) }
                val label = pluralStringResource(R.plurals.show_all_options, prompt.options.size, prompt.options.size)
                Box {
                    SurfaceButton(
                        kind = ActionKind.Raised,
                        onClick = { expanded = true },
                        enabled = enabled,
                        modifier = Modifier.width(48.dp).semantics { contentDescription = label },
                    ) {
                        Icon(AppIcons.more, contentDescription = null, modifier = Modifier.size(18.dp))
                    }
                    DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
                        for (option in prompt.options) {
                            val description = stringResource(R.string.send_key, option.key.accessibilityLabel.resolve())
                            DropdownMenuItem(
                                text = { Text("${option.number}. ${option.text}") },
                                onClick = {
                                    expanded = false
                                    onPress(option.key)
                                },
                                modifier = Modifier.semantics { contentDescription = description },
                            )
                        }
                    }
                }
            }
        }
    }
}

/**
 * 写着 agent 等了、干了多久的地方用的现在：[AgentClock.ticks] 时从 [sinceMs]（Unix 毫秒）起每满一个
 * [AgentClock.STEP_MS] 换一次，页面回到前台时马上换一次，否则停在组合时。`delay` 的计时在手机深睡时不走，所以不能只靠
 * 它数到下一个整步。
 */
@Composable
internal fun agentClock(agent: Agent?, sinceMs: Long?): Instant {
    val ticks = AgentClock.ticks(agent, sinceMs)
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    var now by remember { mutableStateOf(Instant.now()) }
    LaunchedEffect(ticks, sinceMs, lifecycle) {
        now = Instant.now()
        if (!ticks || sinceMs == null) return@LaunchedEffect
        lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
            while (true) {
                now = Instant.now()
                delay(AgentClock.following(now.toEpochMilli(), sinceMs) - System.currentTimeMillis())
            }
        }
    }
    return now
}

/** [counter] 每加一次轻震一下；第一次组合时不震。 */
@Composable
internal fun HapticOnChange(counter: Int, type: HapticFeedbackType) {
    val haptics = LocalHapticFeedback.current
    var last by remember { mutableStateOf(counter) }
    LaunchedEffect(counter) {
        if (counter != last) {
            last = counter
            haptics.performHapticFeedback(type)
        }
    }
}

/**
 * 快速回复栏：认出了提问时是它的选项，没认出时是 [QuickKey.standard] 那一排常用按键；[showsDraft] 时再带一个单行文本框
 * （发送时粘贴进去再按回车）。首页和会话列表上等回答的会话用它。
 */
@Composable
internal fun QuickReplyBar(
    model: QuickReplyModel,
    prompt: AgentPrompt?,
    modifier: Modifier = Modifier,
    layout: PromptOptionLayout = PromptOptionLayout.Rows,
    showsDraft: Boolean = true,
) {
    val colors = themeColors
    HapticOnChange(model.deliveredCount, HapticFeedbackType.Confirm)
    HapticOnChange(model.failedCount, HapticFeedbackType.Reject)
    Column(modifier, verticalArrangement = Arrangement.spacedBy(10.dp)) {
        val enabled = model.isConnected
        if (prompt != null) {
            PromptOptions(prompt, layout, enabled) { model.press(it) }
        } else {
            Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                for (key in QuickKey.standard) {
                    val label = stringResource(R.string.send_key, key.accessibilityLabel.resolve())
                    SurfaceButton(
                        kind = ActionKind.Raised,
                        onClick = { model.press(key) },
                        enabled = enabled,
                        contentPadding = 10.dp,
                        modifier = Modifier.widthIn(min = 44.dp).clearAndSetSemantics {
                            contentDescription = label
                            role = Role.Button
                        },
                    ) {
                        Text(key.label, style = Type.callout.copy(fontWeight = FontWeight.Medium), fontFamily = FontFamily.Monospace)
                    }
                }
            }
        }
        if (showsDraft) DraftField(model, enabled)
        model.errorMessage?.let { error ->
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
                Icon(AppIcons.warning, contentDescription = null, tint = colors.failure, modifier = Modifier.size(14.dp))
                Text(error.resolve(), style = Type.footnote, color = colors.failure)
            }
        }
    }
}

/** 回复的输入框，发送键在框里的右端。 */
@Composable
private fun DraftField(model: QuickReplyModel, enabled: Boolean) {
    val colors = themeColors
    val sendLabel = stringResource(R.string.send_reply)
    val placeholder = stringResource(R.string.draft_placeholder)
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .defaultMinSize(minHeight = 46.dp)
            .background(colors.well, Shapes.inner)
            .border(1.dp, colors.hairline, Shapes.inner)
            .padding(start = 14.dp, end = 1.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.weight(1f), contentAlignment = Alignment.CenterStart) {
            if (model.draft.isEmpty()) {
                Text(placeholder, style = Type.body, color = colors.tertiary, maxLines = 1)
            }
            BasicTextField(
                value = model.draft,
                onValueChange = { model.draft = it },
                enabled = enabled,
                singleLine = true,
                textStyle = Type.body.copy(color = colors.primary),
                cursorBrush = SolidColor(colors.primary),
                keyboardOptions = KeyboardOptions(
                    capitalization = KeyboardCapitalization.None,
                    autoCorrectEnabled = false,
                    imeAction = ImeAction.Send,
                ),
                keyboardActions = KeyboardActions(onSend = { model.sendDraft() }),
                modifier = Modifier.fillMaxWidth().semantics { contentDescription = placeholder },
            )
        }
        Box(
            modifier = Modifier
                .size(44.dp)
                .clickable(enabled = enabled && model.canSendDraft, role = Role.Button) { model.sendDraft() }
                .semantics { contentDescription = sendLabel },
            contentAlignment = Alignment.Center,
        ) {
            Box(Modifier.size(34.dp).background(colors.strongHairline, CircleShape), contentAlignment = Alignment.Center) {
                Icon(
                    AppIcons.arrowUp,
                    contentDescription = null,
                    tint = if (model.canSendDraft) colors.primary else colors.tertiary,
                    modifier = Modifier.size(16.dp),
                )
            }
        }
    }
}

/** 连接状态：图标、文字（重连时带倒计时），断开时有「重试」。 */
@Composable
internal fun ConnectionStatusRow(state: LinkState, onRetry: () -> Unit, modifier: Modifier = Modifier) {
    val colors = themeColors
    val now = linkClock(state)
    Row(modifier, horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.CenterVertically) {
        when (state) {
            LinkState.Idle, LinkState.Connecting ->
                CircularProgressIndicator(Modifier.size(18.dp), color = colors.secondary, strokeWidth = 2.dp)
            is LinkState.Waiting -> Icon(AppIcons.offline, null, tint = colors.waiting, modifier = Modifier.size(18.dp))
            is LinkState.Failed -> Icon(AppIcons.warning, null, tint = colors.failure, modifier = Modifier.size(18.dp))
            is LinkState.Connected -> Icon(AppIcons.check, null, tint = colors.success, modifier = Modifier.size(18.dp))
        }
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(Presentation.linkStatus(state, now).resolve(), style = Type.subheadline.copy(fontWeight = FontWeight.SemiBold))
            val detail = when (state) {
                is LinkState.Failed -> Presentation.linkState(state)
                is LinkState.Waiting -> Presentation.linkFailure(state.reason)
                else -> null
            }
            if (detail != null) Text(detail.resolve(), style = Type.footnote, color = colors.secondary)
        }
        if (state is LinkState.Waiting || state is LinkState.Failed) {
            SurfaceButton(ActionKind.Raised, onClick = onRetry, contentPadding = 16.dp) {
                Text(stringResource(R.string.action_retry), style = Type.subheadline.copy(fontWeight = FontWeight.Medium))
            }
        }
    }
}

/**
 * 新开终端时开什么的菜单项：普通终端，和电脑上装着的各家 agent，前面是它的 logo，没收 logo 的用通用的图标。[onPick] 收到的
 * 为空时是普通终端。
 */
@Composable
internal fun SpawnKindItems(agents: List<InstalledAgent>, onPick: (InstalledAgent?) -> Unit) {
    DropdownMenuItem(
        text = { Text(stringResource(R.string.spawn_kind_terminal)) },
        leadingIcon = { Icon(AppIcons.terminal, null, Modifier.size(18.dp)) },
        onClick = { onPick(null) },
    )
    for (agent in agents) {
        DropdownMenuItem(
            text = { Text(agent.label) },
            leadingIcon = { AgentLogoIcon(agent.kind, Modifier.size(18.dp)) },
            onClick = { onPick(agent) },
        )
    }
}

/** 菜单里一家 agent 名字前的 logo，按前景色染色；没收 logo 的用通用的图标。 */
@Composable
internal fun AgentLogoIcon(kind: AgentKind, modifier: Modifier = Modifier) {
    val logo = Presentation.agentLogo(kind)
    if (logo != null) Icon(painterResource(logo), null, modifier) else Icon(AppIcons.sparkles, null, modifier)
}

/** 只有一个「好」的出错提示；[message] 为空时不出。 */
@Composable
internal fun ErrorDialog(message: String?, onDismiss: () -> Unit) {
    message ?: return
    AlertDialog(
        onDismissRequest = onDismiss,
        confirmButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.action_ok)) } },
        title = { Text(stringResource(R.string.error_title)) },
        text = { Text(message) },
    )
}

/** 要用户确认的危险操作：[confirm] 是那个按钮上的字，按下去才做 [onConfirm]。 */
@Composable
internal fun ConfirmDialog(title: String, message: String, confirm: String, onConfirm: () -> Unit, onDismiss: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        confirmButton = {
            TextButton(onClick = {
                onDismiss()
                onConfirm()
            }) { Text(confirm, color = themeColors.failure) }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.action_cancel)) } },
        title = { Text(title) },
        text = { Text(message) },
    )
}

/** 一行字居中的空状态。 */
@Composable
internal fun EmptyState(title: String, detail: String? = null, modifier: Modifier = Modifier, actions: @Composable () -> Unit = {}) {
    Column(
        modifier.padding(32.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(title, style = Type.title2, textAlign = TextAlign.Center)
        if (detail != null) Text(detail, style = Type.subheadline, color = themeColors.secondary, textAlign = TextAlign.Center)
        actions()
    }
}

/** 应用的标志：应用图标里的提示符 `>`、光标和指示灯，不带图标的底，能放在任何底色上。坐标照抄应用图标的几个图层。 */
@Composable
internal fun RunodeMark(modifier: Modifier = Modifier) {
    val prompt = themeColors.primary
    Canvas(modifier.aspectRatio(1f)) {
        val scale = size.minDimension / 824f
        fun point(x: Float, y: Float) = Offset((x - 100) * scale, (y - 100) * scale)
        val path = Path().apply {
            val a = point(310f, 374f)
            val b = point(472f, 512f)
            val c = point(310f, 650f)
            moveTo(a.x, a.y)
            lineTo(b.x, b.y)
            lineTo(c.x, c.y)
        }
        drawPath(path, prompt, style = Stroke(width = 70 * scale, cap = StrokeCap.Round, join = StrokeJoin.Round))
        drawRoundRect(MARK_MINT, topLeft = point(548f, 618f), size = Size(194 * scale, 66 * scale), cornerRadius = GeometryCornerRadius(33 * scale))
        drawCircle(MARK_GREEN, radius = 28 * scale, center = point(734f, 300f))
    }
}

private val MARK_MINT = Color(0xFF51CDB9)
private val MARK_GREEN = Color(0xFF2CB483)

/** 页面正中一个转圈加一行字。 */
@Composable
internal fun LoadingState(text: String, modifier: Modifier = Modifier) {
    Column(modifier, horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(12.dp)) {
        CircularProgressIndicator(Modifier.size(24.dp), color = themeColors.secondary, strokeWidth = 2.dp)
        Text(text, style = Type.subheadline, color = themeColors.secondary)
    }
}

internal val ScreenPadding: Dp = 16.dp
