package dev.runode.mobile.features.ui.pairing

import android.content.ClipboardManager
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.toggleableState
import androidx.compose.ui.state.ToggleableState
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import dev.runode.mobile.connection.MachineRecord
import dev.runode.mobile.features.R
import dev.runode.mobile.features.model.PairingModel
import dev.runode.mobile.features.model.Presentation
import dev.runode.mobile.features.model.SettingsModel
import dev.runode.mobile.features.ui.AppIcons
import dev.runode.mobile.features.ui.CircleIconButton
import dev.runode.mobile.features.ui.ScreenPadding
import dev.runode.mobile.features.ui.theme.ActionKind
import dev.runode.mobile.features.ui.theme.CornerRadius
import dev.runode.mobile.features.ui.theme.Shapes
import dev.runode.mobile.features.ui.theme.StatusGlyph
import dev.runode.mobile.features.ui.theme.StatusMark
import dev.runode.mobile.features.ui.theme.SurfaceButton
import dev.runode.mobile.features.ui.theme.Type
import dev.runode.mobile.features.ui.theme.cardBackground
import dev.runode.mobile.features.ui.theme.resolve
import dev.runode.mobile.features.ui.theme.themeColors
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

/**
 * 配对页：取景卡里扫码；扫不了码（没有摄像头、没给权限）时卡里写原因，从底下拉出面板粘贴链接。从别处打开的配对链接已经
 * 填在输入框里，一进来就拉出面板，用户看过要连的电脑再点「配对」。配好了换成 [PairedPage]，由用户决定去看会话还是再配一台。
 *
 * @param onFinish 关掉配对页；从配好了的那页去看会话时带着那台电脑
 */
@Composable
fun PairingView(model: PairingModel, settings: SettingsModel, onFinish: (MachineRecord?) -> Unit) {
    val colors = themeColors
    var showingPaste by remember(model) { mutableStateOf(model.linkText.isNotEmpty()) }
    val scope = rememberCoroutineScope()
    val haptics = LocalHapticFeedback.current
    val phase = model.phase
    LaunchedEffect(phase is PairingModel.Phase.Paired) {
        if (phase is PairingModel.Phase.Paired) haptics.performHapticFeedback(HapticFeedbackType.Confirm)
    }
    Box(Modifier.fillMaxSize().background(colors.page)) {
        if (phase is PairingModel.Phase.Paired) {
            PairedPage(phase.machine, settings, onViewSessions = { onFinish(phase.machine) }, onPairAnother = { model.reset() })
        } else {
            ScanPage(model, showingPaste, scope, onShowPaste = { showingPaste = true }, onClose = { onFinish(null) })
        }
    }
    if (showingPaste) {
        PasteLinkSheet(model, onSubmit = { scope.launch { model.submitLink() } }) { showingPaste = false }
    }
}

/** [scope] 是整个配对页的，扫到码后换成配好了的那页时配对不跟着取景卡一起撤掉。 */
@Composable
private fun ScanPage(model: PairingModel, pasteShown: Boolean, scope: CoroutineScope, onShowPaste: () -> Unit, onClose: () -> Unit) {
    FillingColumn {
        Header(onClose)
        Viewfinder(
            unavailable = model.scannerUnavailable?.resolve(),
            onCode = { code ->
                // 粘贴面板开着时用户在看面板里那台电脑，后面的相机扫到别的码不能越过确认去配；配失败了等用户点「重新扫码」。
                if (!pasteShown && model.phase == PairingModel.Phase.Idle) scope.launch { model.scanned(code) }
            },
            onUnavailable = { model.scannerUnavailable = it },
        )
        Status(model)
        Spacer(Modifier.weight(1f))
        SurfaceButton(
            ActionKind.Secondary,
            onClick = onShowPaste,
            height = 50.dp,
            cornerRadius = CornerRadius.action,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Icon(AppIcons.keyboard, contentDescription = null, modifier = Modifier.size(18.dp))
            Spacer(Modifier.width(8.dp))
            Text(stringResource(R.string.pairing_paste_link), style = Type.subheadline)
        }
    }
}

/** 内容不满一屏时撑满一屏，里面 `weight` 的空白才把底下的按钮推到底；字号调大放不下时能滚。 */
@Composable
private fun FillingColumn(content: @Composable androidx.compose.foundation.layout.ColumnScope.() -> Unit) {
    BoxWithConstraints(Modifier.fillMaxSize()) {
        val minHeight = maxHeight
        Column(
            Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .windowInsetsPadding(WindowInsets.navigationBars),
        ) {
            Column(
                Modifier
                    .heightIn(min = minHeight)
                    .windowInsetsPadding(WindowInsets.statusBars)
                    .padding(horizontal = ScreenPadding, vertical = 8.dp),
                verticalArrangement = Arrangement.spacedBy(14.dp),
                content = content,
            )
        }
    }
}

/** 左边关掉配对页的圆键，标题居中；右边留出和圆键一样宽的空，标题才在正中。 */
@Composable
private fun Header(onClose: () -> Unit) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
        CircleIconButton(AppIcons.back, stringResource(R.string.action_close), onClose)
        Text(
            stringResource(R.string.home_pair),
            style = Type.headline,
            textAlign = TextAlign.Center,
            maxLines = 1,
            modifier = Modifier.weight(1f).semantics { heading() },
        )
        Spacer(Modifier.size(44.dp))
    }
}

/**
 * 取景卡：相机画面裁进圆角卡片，四个角标出取景的范围，正中写怎么扫。扫不了码（[unavailable] 是原因）时不放相机，正中改写
 * 原因。相机开着时说明叠在画面上，按深色画、底下压暗，画面再亮也看得清。
 */
@Composable
private fun Viewfinder(unavailable: String?, onCode: (String) -> Unit, onUnavailable: (dev.runode.mobile.features.model.UiText) -> Unit) {
    val colors = themeColors
    val live = unavailable == null
    val shape = RoundedCornerShape(28.dp)
    val textColor = if (live) Color.White else colors.primary
    val secondary = if (live) Color.White.copy(alpha = 0.7f) else colors.secondary
    Box(
        Modifier
            .fillMaxWidth()
            .heightIn(min = 252.dp)
            .background(colors.well, shape)
            .border(1.dp, colors.hairline, shape)
            .semantics(mergeDescendants = true) {},
        contentAlignment = Alignment.Center,
    ) {
        if (live) {
            Box(Modifier.matchParentSize().clip(shape)) {
                QrScanner(onCode, onUnavailable, Modifier.fillMaxSize().clearAndSetSemantics {})
                Box(Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.5f)))
            }
        }
        ReticleCorners(if (live) Color.White else colors.primary, Modifier.matchParentSize())
        Column(
            Modifier.padding(horizontal = 24.dp, vertical = 56.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Icon(if (live) AppIcons.qrcode else AppIcons.camera, contentDescription = null, tint = secondary, modifier = Modifier.size(30.dp))
            if (unavailable != null) {
                Text(unavailable, style = Type.subheadline, color = secondary, textAlign = TextAlign.Center)
            } else {
                Text(
                    stringResource(R.string.pairing_scan_hint),
                    style = Type.subheadline.copy(fontWeight = FontWeight.SemiBold),
                    color = textColor,
                    textAlign = TextAlign.Center,
                )
                Text(stringResource(R.string.pairing_scan_detail), style = Type.footnote, color = secondary, textAlign = TextAlign.Center)
            }
        }
    }
}

/** 取景卡四个角上的 L 形角标，拐角是圆的。 */
@Composable
private fun ReticleCorners(color: Color, modifier: Modifier = Modifier) {
    Canvas(modifier.clearAndSetSemantics {}) {
        val inset = 22.dp.toPx()
        val length = 34.dp.toPx()
        val radius = 12.dp.toPx()
        val left = inset
        val top = inset
        val right = size.width - inset
        val bottom = size.height - inset
        val path = Path()
        for ((corner, dx, dy) in listOf(
            Triple(Offset(left, top), 1f, 1f),
            Triple(Offset(right, top), -1f, 1f),
            Triple(Offset(left, bottom), 1f, -1f),
            Triple(Offset(right, bottom), -1f, -1f),
        )) {
            path.moveTo(corner.x, corner.y + dy * length)
            path.lineTo(corner.x, corner.y + dy * radius)
            path.quadraticTo(corner.x, corner.y, corner.x + dx * radius, corner.y)
            path.lineTo(corner.x + dx * length, corner.y)
        }
        drawPath(path, color, style = Stroke(width = 3.dp.toPx(), cap = StrokeCap.Round, join = StrokeJoin.Round))
    }
}

/** 取景卡下面的状态：正在配对，或者配失败了的原因和「重新扫码」。 */
@Composable
private fun Status(model: PairingModel) {
    val colors = themeColors
    when (val phase = model.phase) {
        is PairingModel.Phase.Pairing -> Row(
            Modifier.fillMaxWidth().cardBackground(colors, padding = 14.dp).semantics(mergeDescendants = true) {},
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            CircularProgressIndicator(Modifier.size(18.dp), color = colors.secondary, strokeWidth = 2.dp)
            Text(stringResource(R.string.pairing_in_progress, phase.hostName), style = Type.subheadline)
        }
        is PairingModel.Phase.Failed -> Column(
            Modifier.fillMaxWidth().cardBackground(colors, padding = 14.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                Icon(AppIcons.close, contentDescription = null, tint = colors.failure, modifier = Modifier.padding(top = 2.dp).size(16.dp))
                Text(phase.message.resolve(), style = Type.subheadline)
            }
            SurfaceButton(ActionKind.Raised, onClick = { model.reset() }, contentPadding = 16.dp) {
                Text(stringResource(R.string.pairing_scan_again), style = Type.subheadline)
            }
        }
        else -> {}
    }
}

/** 配好了的那页：打勾，问要不要在 agent 等回答时收通知，再去看会话或者再配一台。电脑上的会话这时还没读进来，不写有几个。 */
@Composable
private fun PairedPage(machine: MachineRecord, settings: SettingsModel, onViewSessions: () -> Unit, onPairAnother: () -> Unit) {
    val colors = themeColors
    FillingColumn {
        Spacer(Modifier.weight(1f))
        Column(
            Modifier.fillMaxWidth().padding(vertical = 24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            Box(Modifier.size(72.dp).background(colors.success.copy(alpha = 0.14f), CircleShape), contentAlignment = Alignment.Center) {
                StatusGlyph(StatusMark.Done, 34.dp)
            }
            Text(stringResource(R.string.pairing_done), style = Type.title, modifier = Modifier.semantics { heading() })
            Text(
                stringResource(R.string.pairing_done_detail, machine.name),
                style = Type.subheadline,
                color = colors.secondary,
                textAlign = TextAlign.Center,
            )
        }
        Spacer(Modifier.weight(1f))
        val alerts = settings.preferences.alertsBlockedAgents
        val toggle = { settings.preferences = settings.preferences.copy(alertsBlockedAgents = !alerts) }
        Row(
            Modifier
                .fillMaxWidth()
                .cardBackground(colors, padding = 14.dp)
                .clickable(role = Role.Switch, onClick = toggle)
                .semantics(mergeDescendants = true) { toggleableState = ToggleableState(alerts) },
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Icon(AppIcons.bell, contentDescription = null, tint = colors.secondary, modifier = Modifier.size(20.dp))
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(1.dp)) {
                Text(stringResource(R.string.pairing_notify), style = Type.subheadline.copy(fontWeight = FontWeight.SemiBold))
                Text(stringResource(R.string.pairing_notify_detail), style = Type.footnote, color = colors.secondary)
            }
            // 主题的强调色是前景色，开关打开时底和圆钮一样白，看不出开着。
            Switch(
                checked = alerts,
                onCheckedChange = null,
                colors = SwitchDefaults.colors(checkedTrackColor = colors.success, checkedThumbColor = Color.White),
            )
        }
        Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
            SurfaceButton(
                ActionKind.Primary,
                onClick = onViewSessions,
                height = 50.dp,
                cornerRadius = CornerRadius.action,
                modifier = Modifier.fillMaxWidth(),
            ) { Text(stringResource(R.string.pairing_view_sessions), style = Type.subheadline) }
            Box(
                Modifier
                    .fillMaxWidth()
                    .defaultMinSize(minHeight = 44.dp)
                    .clickable(role = Role.Button, onClick = onPairAnother),
                contentAlignment = Alignment.Center,
            ) { Text(stringResource(R.string.pairing_another), style = Type.subheadline, color = colors.secondary) }
        }
    }
}

/**
 * 从底下拉出的面板：粘贴电脑终端里二维码下面那行链接。链接认得出时列出要连的电脑名和地址，用户确认是自己的电脑后点「配对」；
 * 粘贴进来也不直接配。链接是从别处打开时带进来的，面板只让用户确认，认得出时不再显示输入框。
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun PasteLinkSheet(model: PairingModel, onSubmit: () -> Unit, onDismiss: () -> Unit) {
    val colors = themeColors
    val context = LocalContext.current
    val opened = remember { model.linkText.isNotEmpty() }
    val confirmsOnly = opened && model.invitation != null
    val focus = remember { FocusRequester() }
    val state = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = state, containerColor = colors.card) {
        Column(
            Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, bottom = 16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Text(
                    stringResource(if (confirmsOnly) R.string.pairing_confirm_title else R.string.pairing_paste_link),
                    style = Type.headline,
                    modifier = Modifier.semantics { heading() },
                )
                Text(
                    stringResource(if (confirmsOnly) R.string.pairing_confirm_detail else R.string.pairing_paste_detail),
                    style = Type.subheadline,
                    color = colors.secondary,
                )
            }
            if (!confirmsOnly) {
                val placeholder = stringResource(R.string.pairing_link_field)
                Box(
                    Modifier
                        .fillMaxWidth()
                        .background(colors.fill, Shapes.inner)
                        .padding(12.dp),
                ) {
                    if (model.linkText.isEmpty()) {
                        Text(placeholder, style = Type.footnote, fontFamily = FontFamily.Monospace, color = colors.tertiary)
                    }
                    BasicTextField(
                        value = model.linkText,
                        onValueChange = { model.linkText = it },
                        textStyle = Type.footnote.copy(fontFamily = FontFamily.Monospace, color = colors.primary),
                        cursorBrush = SolidColor(colors.primary),
                        maxLines = 3,
                        keyboardOptions = KeyboardOptions(
                            capitalization = KeyboardCapitalization.None,
                            autoCorrectEnabled = false,
                            imeAction = ImeAction.Done,
                        ),
                        modifier = Modifier
                            .fillMaxWidth()
                            .focusRequester(focus)
                            .semantics { contentDescription = placeholder },
                    )
                }
                LaunchedEffect(Unit) { if (!opened) focus.requestFocus() }
            }
            model.invitation?.let { invitation ->
                Column(Modifier.semantics(mergeDescendants = true) {}, verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        Icon(AppIcons.laptop, contentDescription = null, modifier = Modifier.size(16.dp))
                        Text(invitation.hostName, style = Type.subheadline.copy(fontWeight = FontWeight.SemiBold))
                    }
                    Text(
                        Presentation.pairingAddresses(invitation.addresses, invitation.port).resolve(),
                        style = Type.footnote,
                        fontFamily = FontFamily.Monospace,
                        color = colors.secondary,
                    )
                    Text(stringResource(R.string.pairing_check_machine), style = Type.footnote, color = colors.secondary)
                }
            }
            Row(Modifier.defaultMinSize(minHeight = 44.dp), verticalAlignment = Alignment.CenterVertically) {
                if (!confirmsOnly) {
                    TextButton(onClick = {
                        val clip = context.getSystemService(ClipboardManager::class.java)?.primaryClip
                        val text = clip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.coerceToText(context)?.toString()
                        if (!text.isNullOrBlank()) model.linkText = text.trim()
                    }) {
                        Text(stringResource(R.string.action_paste))
                    }
                }
                Spacer(Modifier.weight(1f))
                TextButton(onClick = onDismiss) { Text(stringResource(R.string.action_cancel)) }
                Spacer(Modifier.width(8.dp))
                SurfaceButton(
                    ActionKind.Primary,
                    onClick = {
                        onDismiss()
                        onSubmit()
                    },
                    enabled = model.linkText.isNotEmpty() && !model.isBusy,
                    contentPadding = 20.dp,
                ) { Text(stringResource(R.string.pairing_pair), style = Type.subheadline) }
            }
        }
    }
}
