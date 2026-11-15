package dev.runode.mobile.features.ui.screen

import android.content.pm.ActivityInfo
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.Window
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.calculateCentroid
import androidx.compose.foundation.gestures.calculateCentroidSize
import androidx.compose.foundation.gestures.calculatePan
import androidx.compose.foundation.gestures.calculateZoom
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.displayCutout
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.isImeVisible
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.AwaitPointerEventScope
import androidx.compose.ui.input.pointer.PointerId
import androidx.compose.ui.input.pointer.PointerInputChange
import androidx.compose.ui.input.pointer.PointerInputScope
import androidx.compose.ui.input.pointer.changedToDown
import androidx.compose.ui.input.pointer.changedToUp
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.layout
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.toggleableState
import androidx.compose.ui.state.ToggleableState
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.DialogWindowProvider
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import dev.runode.mobile.features.R
import dev.runode.mobile.features.findActivity
import dev.runode.mobile.features.model.ScreenModel
import dev.runode.mobile.features.model.SurfaceVideoOutput
import dev.runode.mobile.features.ui.AppIcons
import dev.runode.mobile.features.ui.theme.Shapes
import dev.runode.mobile.features.ui.theme.Type
import dev.runode.mobile.features.ui.theme.resolve
import dev.runode.mobile.features.ui.theme.themeColors
import dev.runode.mobile.protocol.ScreenMods
import dev.runode.mobile.protocol.ScreenMouseButton
import dev.runode.mobile.terminal.KeyBar
import dev.runode.mobile.terminal.KeyBarKey
import dev.runode.mobile.terminal.KeyBarPalette
import kotlin.math.abs
import kotlin.math.min
import kotlin.math.roundToInt
import kotlinx.coroutines.Job
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * 远程桌面页：横屏、全屏（藏起状态栏和导航栏，从边上划出来），离开时换回原来的方向和系统栏。页面本身在
 * [RemoteDesktopPage]。
 */
@Composable
fun RemoteDesktopScreen(model: ScreenModel, onBack: () -> Unit) {
    val activity = LocalContext.current.findActivity()
    if (activity != null) {
        DisposableEffect(activity) {
            val orientation = activity.requestedOrientation
            activity.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE
            val restoreSystemBars = hideSystemBars(activity.window)
            onDispose {
                restoreSystemBars()
                activity.requestedOrientation = orientation
            }
        }
    }
    RemoteDesktopPage(model, onBack)
}

/** 藏起 [window] 的状态栏和导航栏，从边上划出来；返回换回原来的样子的函数。 */
private fun hideSystemBars(window: Window): () -> Unit {
    val controller = WindowCompat.getInsetsController(window, window.decorView)
    val behavior = controller.systemBarsBehavior
    controller.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
    controller.hide(WindowInsetsCompat.Type.systemBars())
    return {
        controller.show(WindowInsetsCompat.Type.systemBars())
        controller.systemBarsBehavior = behavior
    }
}

/**
 * 电脑的整块屏幕。触屏模式点按是左键，长按后原地松手是右键、接着拖是按着左键拖，没放大时单指拖也是按着左键拖，
 * 放大后单指拖是挪画面；触控板模式单指挪光标，点按是左键，点一下再按住拖是按着左键拖。两种模式下双指点按、一指按住
 * 另一指点都是右键。双指滑是滚动，捏合放大，放大后双指拖着看别处，按着左键拖到或光标挪到页面边上时画面跟着往那边
 * 挪。返回按钮浮在左上角，右上角是解锁、切换
 * 指针模式和（有几块屏幕时）切换屏幕。底部一条快捷键（软键盘没有的键，右端开关软键盘），软键盘弹出时跟在键盘上面。
 * 进来时开始连（[ScreenModel.start]），离开页面时由 `AppModel` 断开。
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun RemoteDesktopPage(model: ScreenModel, onBack: () -> Unit) {
    var keyboardVisible by remember { mutableStateOf(false) }
    LaunchedEffect(model) { model.start() }
    // 系统收起软键盘（返回手势）时底部那条跟着换回去。
    val imeVisible = WindowInsets.isImeVisible
    var imeShown by remember { mutableStateOf(false) }
    LaunchedEffect(imeVisible) {
        if (imeVisible) {
            imeShown = true
        } else if (imeShown) {
            imeShown = false
            keyboardVisible = false
        }
    }
    val phase = model.phase
    val failedForPermission = phase is ScreenModel.Phase.Failed && model.lacksScreenCapture
    val permissionMessage = model.missingPermissions.takeIf { it.isNotEmpty() }?.let { missing ->
        stringResource(R.string.screen_missing_permission, missing.map { it.resolve() }.joinToString(", "))
    }
    Column(Modifier.fillMaxSize().background(Color.Black).imePadding()) {
        Box(Modifier.weight(1f).fillMaxWidth()) {
            ScreenSurface(model, Modifier.fillMaxSize())
            when (phase) {
                ScreenModel.Phase.Connecting -> Connecting()
                is ScreenModel.Phase.Failed -> Failed(
                    title = stringResource(if (failedForPermission) R.string.screen_needs_permission else R.string.screen_disconnected),
                    detail = if (failedForPermission) permissionMessage ?: phase.reason.resolve() else phase.reason.resolve(),
                    permission = failedForPermission,
                ) { model.retry() }
                ScreenModel.Phase.Live -> {}
            }
            // 中间的失败页说着缺的权限时不再重复；排在浮动按钮下面，不被它们盖住。
            if (permissionMessage != null && !failedForPermission) {
                val colors = themeColors
                Text(
                    permissionMessage,
                    style = Type.footnote,
                    color = colors.onWaitingFill,
                    modifier = Modifier
                        .windowInsetsPadding(WindowInsets.displayCutout)
                        .padding(start = FloatingMargin, end = FloatingMargin, top = FloatingSize + FloatingMargin * 2)
                        .fillMaxWidth()
                        .background(colors.waitingFill, Shapes.inner)
                        .padding(horizontal = 12.dp, vertical = 8.dp),
                )
            }
            FloatingButton(
                AppIcons.back,
                stringResource(R.string.action_back),
                onBack,
                Modifier.align(Alignment.TopStart).windowInsetsPadding(WindowInsets.displayCutout).padding(FloatingMargin),
            )
            FloatingActions(model, Modifier.align(Alignment.TopEnd).windowInsetsPadding(WindowInsets.displayCutout).padding(FloatingMargin))
        }
        if (phase == ScreenModel.Phase.Live) {
            ScreenKeys(model, keyboardOpen = keyboardVisible) { keyboardVisible = !keyboardVisible }
        }
    }
    KeyInput(model, keyboardVisible)
    if (model.askingPassword) UnlockDialog(model)
}

/** 浮在画面左上、右上角的圆形按钮：边长和离屏幕边的距离。画面放得下时让开它们那一条。 */
private val FloatingSize = 44.dp
private val FloatingMargin = 8.dp

@Composable
private fun FloatingButton(
    icon: ImageVector,
    label: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    content: (@Composable () -> Unit)? = null,
) {
    Box(
        modifier
            .size(FloatingSize)
            .background(Color.Black.copy(alpha = 0.55f), CircleShape)
            .clickable(enabled = enabled, role = Role.Button, onClick = onClick)
            .semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) {
        if (content != null) content() else Icon(icon, null, tint = Color.White, modifier = Modifier.size(20.dp))
    }
}

/** 右上角：电脑锁着屏时的解锁，触屏和触控板模式的切换，几块屏幕时的切换。 */
@Composable
private fun FloatingActions(model: ScreenModel, modifier: Modifier) {
    Row(modifier, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        if (model.locked) {
            FloatingButton(AppIcons.lockOpen, stringResource(R.string.screen_unlock), { model.unlock() }, enabled = !model.unlocking) {
                if (model.unlocking) {
                    CircularProgressIndicator(Modifier.size(20.dp), color = Color.White, strokeWidth = 2.dp)
                } else {
                    Icon(AppIcons.lockOpen, null, tint = Color.White, modifier = Modifier.size(20.dp))
                }
            }
        }
        val touch = model.pointerMode == ScreenModel.PointerMode.Touch
        FloatingButton(
            if (touch) AppIcons.touch else AppIcons.pointer,
            stringResource(if (touch) R.string.screen_trackpad_mode else R.string.screen_touch_mode),
            {
                model.pointerMode = if (touch) ScreenModel.PointerMode.Trackpad else ScreenModel.PointerMode.Touch
            },
        )
        if (model.displays.size > 1) {
            var expanded by remember { mutableStateOf(false) }
            Box {
                FloatingButton(AppIcons.displays, stringResource(R.string.screen_switch_display), { expanded = true })
                DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
                    for (display in model.displays) {
                        val current = model.display?.id == display.id
                        DropdownMenuItem(
                            text = { Text(model.name(display).resolve()) },
                            trailingIcon = if (current) ({ Icon(AppIcons.check, null, Modifier.size(18.dp)) }) else null,
                            modifier = Modifier.semantics { toggleableState = ToggleableState(current) },
                            onClick = {
                                expanded = false
                                model.switchDisplay(display)
                            },
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun Connecting() {
    Box(Modifier.fillMaxSize().background(Color.Black), contentAlignment = Alignment.Center) {
        Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(12.dp)) {
            CircularProgressIndicator(color = Color.White)
            Text(stringResource(R.string.screen_connecting), style = Type.subheadline, color = Color.White)
        }
    }
}

/** 断开了：为什么（缺屏幕录制时说缺什么、去哪里开），重新连接。 */
@Composable
private fun Failed(title: String, detail: String, permission: Boolean, onRetry: () -> Unit) {
    Box(Modifier.fillMaxSize().background(Color.Black), contentAlignment = Alignment.Center) {
        Column(
            Modifier.widthIn(max = 480.dp).padding(32.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Icon(if (permission) AppIcons.lock else AppIcons.display, null, tint = Color.White, modifier = Modifier.size(36.dp))
            Text(title, style = Type.title2, color = Color.White, textAlign = TextAlign.Center)
            Text(detail, style = Type.subheadline, color = Color.White.copy(alpha = 0.7f), textAlign = TextAlign.Center)
            Box(
                Modifier
                    .padding(top = 6.dp)
                    .background(Color.White.copy(alpha = 0.15f), Shapes.action)
                    .clickable(role = Role.Button, onClick = onRetry)
                    .padding(horizontal = 18.dp, vertical = 12.dp),
            ) {
                Text(stringResource(R.string.screen_reconnect), style = Type.headline, color = Color.White)
            }
        }
    }
}

/**
 * 画面和手势。画面按电脑屏幕的比例放进页面，两边的黑边放不下浮动按钮时让到按钮那一条下面，电脑屏幕的左上角、右上角
 * 才点得到。放大时 SurfaceView 跟着变大挪开（它不吃 graphicsLayer 的变换）。
 */
@Composable
private fun ScreenSurface(model: ScreenModel, modifier: Modifier) {
    val density = LocalDensity.current
    val direction = LocalLayoutDirection.current
    var container by remember { mutableStateOf(IntSize.Zero) }
    var zoom by remember { mutableFloatStateOf(1f) }
    var pan by remember { mutableStateOf(Offset.Zero) }
    val videoSize = model.videoSize
    // 换了尺寸时退回不放大，画面按新比例重新摆。
    LaunchedEffect(videoSize) {
        zoom = 1f
        pan = Offset.Zero
    }
    val cutout = WindowInsets.displayCutout
    val reach = with(density) { (FloatingMargin + FloatingSize).toPx() }
    val fitted = fittedRect(
        container,
        videoSize,
        reachLeft = cutout.getLeft(density, direction) + reach,
        reachTop = cutout.getTop(density) + reach,
    )
    val shown = Rect(fitted.topLeft * zoom + pan, fitted.size * zoom)
    // 手势里每挪一下都可能改 zoom 和 pan，画面在哪按这一刻的值算，不等重组。
    val currentFitted by rememberUpdatedState(fitted)
    val haptics = LocalHapticFeedback.current
    var cursor by remember { mutableStateOf(model.cursor) }
    DisposableEffect(model) {
        model.onCursor = { x, y -> cursor = x to y }
        onDispose { model.onCursor = null }
    }
    val trackpad = model.pointerMode == ScreenModel.PointerMode.Trackpad
    // 切进触控板模式时电脑上的光标挪到显示的光标那里，点下去的就是看到的位置。
    LaunchedEffect(trackpad) { if (trackpad) model.moveCursor(0.0, 0.0) }
    val label = stringResource(R.string.screen_computer_screen)
    Box(modifier.onSizeChanged { container = it }.clipToBounds()) {
        AndroidView(
            factory = { context ->
                SurfaceView(context).apply { holder.addCallback(SurfaceCallback(model)) }
            },
            modifier = Modifier.layout { measurable, constraints ->
                val placeable = measurable.measure(
                    Constraints.fixed(shown.width.roundToInt().coerceAtLeast(1), shown.height.roundToInt().coerceAtLeast(1)),
                )
                layout(constraints.maxWidth, constraints.maxHeight) {
                    placeable.place(shown.left.roundToInt(), shown.top.roundToInt())
                }
            },
        )
        if (trackpad && videoSize != null) {
            Icon(
                AppIcons.pointer,
                null,
                tint = Color.White,
                modifier = Modifier
                    .offset {
                        IntOffset(
                            (shown.left + cursor.first.toFloat() * shown.width).roundToInt(),
                            (shown.top + cursor.second.toFloat() * shown.height).roundToInt(),
                        )
                    }
                    .size(20.dp),
            )
        }
        Box(
            Modifier
                .fillMaxSize()
                .semantics {
                    contentDescription = label
                    role = Role.Image
                }
                .pointerInput(model) {
                    screenGestures(
                        model,
                        shown = { Rect(currentFitted.topLeft * zoom + pan, currentFitted.size * zoom) },
                        zoomed = { zoom > 1.01f },
                        haptic = { haptics.performHapticFeedback(HapticFeedbackType.LongPress) },
                    ) { centroid, panChange, zoomChange ->
                        val next = (zoom * zoomChange).coerceIn(1f, MaxZoom)
                        val moved = centroid - (centroid - pan) * (next / zoom) + panChange
                        zoom = next
                        pan = Offset(
                            moved.x.coerceIn(container.width * (1 - next), 0f),
                            moved.y.coerceIn(container.height * (1 - next), 0f),
                        )
                    }
                },
        )
    }
}

private const val MaxZoom = 5f

/** 画面放进 [container] 的位置：保持比例居中；左上角放不下浮动按钮时整块挪到 [reachTop] 以下。 */
internal fun fittedRect(container: IntSize, video: IntSize?, reachLeft: Float, reachTop: Float): Rect {
    val bounds = Rect(Offset.Zero, Size(container.width.toFloat(), container.height.toFloat()))
    if (video == null || video.width <= 0 || video.height <= 0 || bounds.isEmpty) return bounds
    val fitted = aspectFit(video, bounds)
    if (fitted.left >= reachLeft || fitted.top >= reachTop) return fitted
    return aspectFit(video, Rect(0f, reachTop, bounds.right, bounds.bottom))
}

private fun aspectFit(video: IntSize, bounds: Rect): Rect {
    val scale = min(bounds.width / video.width, bounds.height / video.height)
    val size = Size(video.width * scale, video.height * scale)
    return Rect(Offset(bounds.left + (bounds.width - size.width) / 2, bounds.top + (bounds.height - size.height) / 2), size)
}

/** surface 建好时把画面接上去、要一个关键帧从头画；销毁前先停掉解码器。 */
private class SurfaceCallback(private val model: ScreenModel) : SurfaceHolder.Callback {
    override fun surfaceCreated(holder: SurfaceHolder) {
        model.attachOutput(SurfaceVideoOutput(holder.surface))
        model.requestKeyframe()
    }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {}

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        model.detachOutput()
    }
}

/** 一次手势在单指的阶段怎么收场。 */
private enum class Opening {
    Tap,
    Drag,
    TwoFingers,
}

/**
 * 画面上的手势，[shown] 是画面现在摆在哪里（放大后的），[zoomed] 是放大着。双指捏合、放大后的双指拖和触屏模式
 * 放大后的单指拖交给 [transform]（捏合中心、平移、缩放倍数），没放大时双指滑是滚动；放大着时按着左键拖到页面边上、
 * 光标挪到页面边上，也经 [transform] 把画面往那边挪。触屏模式的长按在认出来时调 [haptic]。
 */
private suspend fun PointerInputScope.screenGestures(
    model: ScreenModel,
    shown: () -> Rect,
    zoomed: () -> Boolean,
    haptic: () -> Unit,
    transform: (centroid: Offset, pan: Offset, zoom: Float) -> Unit,
) = coroutineScope {
    // 触控板模式点一下的左键压到双击的时限过了才发：时限里又按下时，这一下是双击或点按拖移的开头。
    var pendingClick: Job? = null
    awaitEachGesture {
        val down = awaitFirstDown(requireUnconsumed = false)
        val armed = pendingClick != null
        pendingClick?.cancel()
        pendingClick = null
        screenGesture(model, down, armed, shown, zoomed, haptic, transform) {
            pendingClick = launch {
                delay(viewConfiguration.doubleTapTimeoutMillis)
                pendingClick = null
                model.clickAtCursor(ScreenMouseButton.Left)
            }
        }
    }
}

/**
 * 从 [down] 开始的一次手势。[armed] 是上一下点按的左键还压着没发（触控板模式）；这次点按时 [deferClick] 把它压下。
 */
private suspend fun AwaitPointerEventScope.screenGesture(
    model: ScreenModel,
    down: PointerInputChange,
    armed: Boolean,
    shown: () -> Rect,
    zoomed: () -> Boolean,
    haptic: () -> Unit,
    transform: (centroid: Offset, pan: Offset, zoom: Float) -> Unit,
    deferClick: () -> Unit,
) {
    val slop = viewConfiguration.touchSlop
    val trackpad = model.pointerMode == ScreenModel.PointerMode.Trackpad
    val edge = EdgeZone.toPx()
    val edgeStep = EdgeStep.toPx()
    fun fraction(point: Offset): Pair<Double, Double> {
        val rect = shown()
        if (rect.width <= 0f || rect.height <= 0f) return 0.0 to 0.0
        return ScreenModel.clamped(((point.x - rect.left) / rect.width).toDouble()) to
            ScreenModel.clamped(((point.y - rect.top) / rect.height).toDouble())
    }
    /** [point] 离页面边不到 [edge] 时画面这一下往那边挪多少，越靠边越快。 */
    fun edgePush(point: Offset): Offset {
        fun axis(value: Float, extent: Float) = when {
            value < edge -> edgeStep * min(1f, (edge - value) / edge)
            value > extent - edge -> -edgeStep * min(1f, (value - extent + edge) / edge)
            else -> 0f
        }
        return Offset(axis(point.x, size.width.toFloat()), axis(point.y, size.height.toFloat()))
    }
    /** 光标出了页面边上那一圈时把画面挪到光标露出来（没放大时 [transform] 挪不动）。 */
    fun revealCursor() {
        val rect = shown()
        val (x, y) = model.cursor
        val point = Offset(rect.left + x.toFloat() * rect.width, rect.top + y.toFloat() * rect.height)
        fun axis(value: Float, extent: Float) = when {
            value < edge -> edge - value
            value > extent - edge -> extent - edge - value
            else -> 0f
        }
        val push = Offset(axis(point.x, size.width.toFloat()), axis(point.y, size.height.toFloat()))
        if (push != Offset.Zero) transform(Offset.Zero, push, 1f)
    }
    fun cursorDelta(from: Offset, to: Offset) {
        val rect = shown()
        if (rect.width <= 0f || rect.height <= 0f) return
        model.moveCursor(((to.x - from.x) / rect.width).toDouble(), ((to.y - from.y) / rect.height).toDouble())
        revealCursor()
    }
    fun rightClickAt(point: Offset) {
        if (!shown().contains(point)) return
        val (x, y) = fraction(point)
        model.rightClick(x, y)
    }
    // 等到长按的时限还没分出来（没抬起、没挪开、没落下第二根手指）就是长按，为空。
    val opening = withTimeoutOrNull(LongPressMillis) {
        var decided: Opening? = null
        while (decided == null) {
            val event = awaitPointerEvent()
            val pressed = event.changes.count { it.pressed }
            val moved = event.changes.firstOrNull { it.id == down.id }
                ?.let { (it.position - down.position).getDistance() > slop } == true
            decided = when {
                pressed >= 2 -> Opening.TwoFingers
                pressed == 0 -> Opening.Tap
                moved -> Opening.Drag
                else -> null
            }
        }
        decided
    }
    // 上一下点按压着的左键：这次是点按就连成双击，是拖或长按就是点按拖移，别的手势先把它发出去。
    if (armed && (!trackpad || opening == Opening.TwoFingers)) model.clickAtCursor(ScreenMouseButton.Left)
    val dragging = armed && trackpad && (opening == Opening.Drag || opening == null)
    when {
        opening == Opening.Tap -> when {
            trackpad && armed -> repeat(2) { model.clickAtCursor(ScreenMouseButton.Left) }
            trackpad -> deferClick()
            shown().contains(down.position) -> {
                val (x, y) = fraction(down.position)
                model.tap(x, y, down.position / density)
            }
        }
        opening == Opening.TwoFingers -> twoFingers(model, down, trackpad, shown, zoomed, ::rightClickAt, transform)
        opening == null && !trackpad -> {
            // 长按的右键松手时才发，按着时另一根手指点一下就在按住的地方右键；挪开了就是从按下的地方按着左键拖。
            haptic()
            var tapped = false
            var moved = false
            untilReleased(
                down.id,
                onMove = { _, to -> if ((to - down.position).getDistance() > PressDragSlop.toPx()) moved = true },
                onSecondTap = {
                    tapped = true
                    rightClickAt(down.position)
                },
                until = { moved && !tapped },
            )
            val held = currentEvent.changes.firstOrNull { it.id == down.id }?.pressed == true
            when {
                moved && !tapped && held -> touchDrag(down, down.position, ::fraction, ::edgePush, model, transform)
                !moved && !tapped -> rightClickAt(down.position)
            }
        }
        trackpad -> {
            if (dragging) model.dragAtCursor(true)
            untilReleased(
                down.id,
                onMove = ::cursorDelta,
                onSecondTap = if (dragging) null else ({ model.clickAtCursor(ScreenMouseButton.Right) }),
            )
            if (dragging) model.dragAtCursor(false)
        }
        // 放大着时单指拖是挪画面，要在电脑上拖就长按后再拖。
        zoomed() -> untilReleased(down.id, onMove = { from, to -> transform(Offset.Zero, to - from, 1f) })
        else -> {
            val start = currentEvent.changes.firstOrNull { it.id == down.id }?.position ?: down.position
            touchDrag(down, start, ::fraction, ::edgePush, model, transform)
        }
    }
}

/**
 * 触屏模式从 [start] 按下左键拖，跟着 [down] 那根手指到它抬起。手指停在页面边上时也每一帧经 [edgePush] 挪画面，
 * 手指下的位置跟着变。
 */
private suspend fun AwaitPointerEventScope.touchDrag(
    down: PointerInputChange,
    start: Offset,
    fraction: (Offset) -> Pair<Double, Double>,
    edgePush: (Offset) -> Offset,
    model: ScreenModel,
    transform: (centroid: Offset, pan: Offset, zoom: Float) -> Unit,
) {
    var last = currentEvent.changes.firstOrNull { it.id == down.id }?.position ?: start
    var sent = fraction(start)
    model.drag(ScreenModel.DragPhase.Began, sent.first, sent.second)
    while (true) {
        val event = withTimeoutOrNull(EdgeFrameMillis) { awaitPointerEvent() }
        if (event != null) {
            val change = event.changes.firstOrNull { it.id == down.id } ?: break
            change.consume()
            last = change.position
            if (!change.pressed) break
        }
        val push = edgePush(last)
        if (push != Offset.Zero) transform(Offset.Zero, push, 1f)
        val now = fraction(last)
        if (now != sent) {
            model.drag(ScreenModel.DragPhase.Moved, now.first, now.second)
            sent = now
        }
    }
    model.drag(ScreenModel.DragPhase.Ended, sent.first, sent.second)
}

/**
 * 两根手指：捏合或放大后的拖交给 [transform]，没放大时滑是滚动。第二根手指按下后很快抬起（两指一起点，或一指按住、
 * 另一指点）是右键：触控板模式在光标处，触屏模式经 [rightClickAt] 点在先按下的那根手指处。
 */
private suspend fun AwaitPointerEventScope.twoFingers(
    model: ScreenModel,
    down: PointerInputChange,
    trackpad: Boolean,
    shown: () -> Rect,
    zoomed: () -> Boolean,
    rightClickAt: (Offset) -> Unit,
    transform: (centroid: Offset, pan: Offset, zoom: Float) -> Unit,
) {
    val slop = viewConfiguration.touchSlop
    val secondDown = currentEvent.changes.filter { it.id != down.id && it.pressed }.maxOfOrNull { it.uptimeMillis }
        ?: down.uptimeMillis
    var first = currentEvent.changes.firstOrNull { it.id == down.id }?.position ?: down.position
    var mode = TwoFingerMode.Undecided
    var totalPan = Offset.Zero
    var totalZoom = 1f
    while (true) {
        val event = awaitPointerEvent()
        event.changes.firstOrNull { it.id == down.id }?.let { first = it.position }
        val pressed = event.changes.count { it.pressed }
        if (pressed < 2 && mode == TwoFingerMode.Undecided) {
            val lifted = event.changes.maxOfOrNull { it.uptimeMillis } ?: secondDown
            if (lifted - secondDown < viewConfiguration.longPressTimeoutMillis) {
                if (trackpad) model.clickAtCursor(ScreenMouseButton.Right) else rightClickAt(first)
            }
            mode = TwoFingerMode.Done
        }
        event.changes.forEach { it.consume() }
        if (pressed == 0) break
        if (pressed < 2 || mode == TwoFingerMode.Done) continue
        val panChange = event.calculatePan()
        val zoomChange = event.calculateZoom()
        val centroid = event.calculateCentroid(useCurrent = true)
        if (mode == TwoFingerMode.Undecided) {
            totalPan += panChange
            totalZoom *= zoomChange
            val pinched = abs(1 - totalZoom) * event.calculateCentroidSize(useCurrent = true) > slop
            mode = when {
                pinched -> TwoFingerMode.Transform
                totalPan.getDistance() > slop -> if (zoomed()) TwoFingerMode.Transform else TwoFingerMode.Scroll
                else -> TwoFingerMode.Undecided
            }
        }
        when (mode) {
            TwoFingerMode.Scroll -> model.scroll(panChange.x, panChange.y, shown().width)
            TwoFingerMode.Transform -> transform(centroid, panChange, zoomChange)
            TwoFingerMode.Undecided, TwoFingerMode.Done -> {}
        }
    }
}

private enum class TwoFingerMode {
    Undecided,
    Scroll,
    Transform,

    /** 点过了，或按得太久不算点：剩下的只等手指都抬起。 */
    Done,
}

/** 手指或光标离页面边不到这么远时，放大着的画面往那边挪。 */
private val EdgeZone = 40.dp

/** 手指停在页面最边上时画面每一帧挪多远。 */
private val EdgeStep = 12.dp

/** 手指停在页面边上不动时多久挪一次画面。 */
private const val EdgeFrameMillis = 16L

/** 按住多久算长按。 */
private const val LongPressMillis = 350L

/** 长按开始以后手指挪开多远就不再是右键、开始按着左键拖。 */
private val PressDragSlop = 4.dp

/**
 * 跟着 [pointer] 那根手指到它抬起，每挪一下交给 [onMove]（上一个位置、这一个位置）；返回最后的位置。给了
 * [onSecondTap] 时，按着期间另一根手指点一下（按下到抬起不到长按的时限、没挪开）就调它；每一下之后 [until] 为真时
 * 不再跟，手指还按着就返回。
 */
private suspend fun AwaitPointerEventScope.untilReleased(
    pointer: PointerId,
    onMove: (Offset, Offset) -> Unit,
    onSecondTap: (() -> Unit)? = null,
    until: () -> Boolean = { false },
): Offset? {
    val slop = viewConfiguration.touchSlop
    var last: Offset? = currentEvent.changes.firstOrNull { it.id == pointer }?.position
    // 另一根手指按下的那一下；挪开了就是 null，等它抬起再认下一根。
    var other: PointerInputChange? = null
    var otherId: PointerId? = null
    while (true) {
        val event = awaitPointerEvent()
        for (change in event.changes) {
            if (change.id == pointer) continue
            if (change.changedToDown() && otherId == null) {
                otherId = change.id
                other = change
            } else if (change.id == otherId) {
                val start = other
                if (start != null && (change.position - start.position).getDistance() > slop) other = null
                if (change.changedToUp()) {
                    if (start != null && other != null && change.uptimeMillis - start.uptimeMillis < viewConfiguration.longPressTimeoutMillis) {
                        onSecondTap?.invoke()
                    }
                    otherId = null
                    other = null
                }
            }
            change.consume()
        }
        val change = event.changes.firstOrNull { it.id == pointer } ?: return last
        val previous = last ?: change.previousPosition
        if (change.position != previous) onMove(previous, change.position)
        last = change.position
        change.consume()
        if (!change.pressed) return last
        if (until()) return last
    }
}

/** 底部的快捷键：Esc、Tab、按住的 ⌘ ⌃ ⌥ ⇧、方向键，右端开关软键盘（[keyboardOpen] 时是「收起」）。 */
@Composable
private fun ScreenKeys(model: ScreenModel, keyboardOpen: Boolean, onKeyboard: () -> Unit) {
    val mods = model.stickyMods
    fun toggle(change: (ScreenMods) -> ScreenMods) {
        model.stickyMods = change(model.stickyMods)
    }
    KeyBar(
        modifier = Modifier
            .fillMaxWidth()
            .background(Color.Black)
            .windowInsetsPadding(WindowInsets.displayCutout.only(WindowInsetsSides.Horizontal)),
        palette = KeyPalette,
        trailing = {
            KeyBarKey(
                "",
                stringResource(if (keyboardOpen) R.string.screen_hide_keyboard else R.string.screen_show_keyboard),
                icon = if (!keyboardOpen) AppIcons.keyboard else AppIcons.keyboardHide,
                onClick = onKeyboard,
            )
        },
    ) {
        KeyBarKey("esc", stringResource(R.string.screen_key_esc)) { model.press("Escape") }
        KeyBarKey("tab", stringResource(R.string.screen_key_tab)) { model.press("Tab") }
        KeyBarKey("⌘", stringResource(R.string.screen_hold_command), selected = mods.cmd) { toggle { it.copy(cmd = !it.cmd) } }
        KeyBarKey("⌃", stringResource(R.string.screen_hold_control), selected = mods.ctrl) { toggle { it.copy(ctrl = !it.ctrl) } }
        KeyBarKey("⌥", stringResource(R.string.screen_hold_option), selected = mods.alt) { toggle { it.copy(alt = !it.alt) } }
        KeyBarKey("⇧", stringResource(R.string.screen_hold_shift), selected = mods.shift) { toggle { it.copy(shift = !it.shift) } }
        KeyBarKey("", stringResource(R.string.screen_arrow_left), icon = AppIcons.arrowLeft) { model.press("ArrowLeft") }
        KeyBarKey("", stringResource(R.string.screen_arrow_up), icon = AppIcons.arrowUp) { model.press("ArrowUp") }
        KeyBarKey("", stringResource(R.string.screen_arrow_down), icon = AppIcons.arrowDown) { model.press("ArrowDown") }
        KeyBarKey("", stringResource(R.string.screen_arrow_right), icon = AppIcons.arrowRight) { model.press("ArrowRight") }
    }
}

/** 页面一直是黑底，键也一直是深色的。 */
private val KeyPalette = KeyBarPalette(
    key = Color(0xFF2C2C2E),
    border = Color(0xFF2C2C2E),
    label = Color.White,
    selectedKey = Color.White,
    selectedLabel = Color.Black,
)

/** 输入框里先放着的一个零宽字符：软键盘的删除键删掉它时，就是要电脑上的退格键。 */
private const val Sentinel = "​"

/**
 * 接软键盘的隐藏输入框，[visible] 时拿焦点、弹出软键盘。上屏的字（输入法组完的）发给电脑，删除键在框里没字时发退格键，
 * 回车发回车。
 */
@Composable
private fun KeyInput(model: ScreenModel, visible: Boolean) {
    val focus = remember { FocusRequester() }
    val focusManager = LocalFocusManager.current
    val keyboard = LocalSoftwareKeyboardController.current
    var value by remember { mutableStateOf(TextFieldValue(Sentinel, TextRange(Sentinel.length))) }
    LaunchedEffect(visible) {
        if (visible) {
            focus.requestFocus()
            keyboard?.show()
        } else {
            focusManager.clearFocus()
            keyboard?.hide()
        }
    }
    BasicTextField(
        value = value,
        onValueChange = { next ->
            if (next.composition != null) {
                value = next
                return@BasicTextField
            }
            if (!next.text.startsWith(Sentinel)) {
                model.press("Backspace")
            } else {
                val typed = next.text.removePrefix(Sentinel)
                typed.split('\n').forEachIndexed { index, line ->
                    if (index > 0) model.press("Enter")
                    if (line.isNotEmpty()) model.type(line)
                }
            }
            value = TextFieldValue(Sentinel, TextRange(Sentinel.length))
        },
        keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false),
        textStyle = Type.body.copy(color = Color.Transparent),
        cursorBrush = SolidColor(Color.Transparent),
        modifier = Modifier.size(1.dp).focusRequester(focus),
    )
}

/** 电脑锁着屏时输登录密码：发给电脑敲进锁屏的密码框，打开记住时解开以后存在这部手机上。 */
@Composable
private fun UnlockDialog(model: ScreenModel) {
    val colors = themeColors
    var password by remember { mutableStateOf("") }
    var keep by remember { mutableStateOf(true) }
    val focus = remember { FocusRequester() }
    fun send() {
        if (password.isNotEmpty()) model.unlock(password, keep)
    }
    LaunchedEffect(Unit) { focus.requestFocus() }
    // 页面总是横屏，键盘弹出后只剩不到半屏高：整屏的表单，取消和解锁在顶上一行，开关和输入框并排，说明在能滚动的下面。
    Dialog(
        onDismissRequest = { model.askingPassword = false },
        properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
    ) {
        // 对话框是单独的窗口，系统栏按有焦点的窗口显示，页面窗口藏起的它不跟着藏；对话框关掉时窗口一起没了，不用换回。
        val dialogWindow = (LocalView.current.parent as? DialogWindowProvider)?.window
        if (dialogWindow != null) {
            DisposableEffect(dialogWindow) {
                hideSystemBars(dialogWindow)
                onDispose {}
            }
        }
        Column(
            Modifier
                .fillMaxSize()
                .background(colors.page)
                .windowInsetsPadding(WindowInsets.safeDrawing)
                .padding(horizontal = 16.dp),
        ) {
            Row(Modifier.fillMaxWidth().height(52.dp), verticalAlignment = Alignment.CenterVertically) {
                TextButton(onClick = { model.askingPassword = false }) { Text(stringResource(R.string.action_cancel)) }
                Text(
                    stringResource(R.string.screen_unlock),
                    style = Type.headline,
                    color = colors.primary,
                    textAlign = TextAlign.Center,
                    modifier = Modifier.weight(1f).semantics { heading() },
                )
                TextButton(onClick = { send() }, enabled = password.isNotEmpty()) {
                    Text(stringResource(R.string.screen_unlock_action), fontWeight = FontWeight.SemiBold)
                }
            }
            Column(
                Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(bottom = 12.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Row(
                    Modifier.fillMaxWidth(),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(16.dp),
                ) {
                    OutlinedTextField(
                        value = password,
                        onValueChange = { password = it },
                        label = { Text(stringResource(R.string.screen_password)) },
                        singleLine = true,
                        visualTransformation = PasswordVisualTransformation(),
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, imeAction = ImeAction.Go),
                        keyboardActions = KeyboardActions(onGo = { send() }),
                        modifier = Modifier.weight(1f).focusRequester(focus),
                    )
                    Row(
                        Modifier.toggleable(value = keep, role = Role.Switch) { keep = it },
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        Text(stringResource(R.string.screen_remember), style = Type.body, color = colors.primary)
                        Switch(
                            checked = keep,
                            onCheckedChange = null,
                            colors = SwitchDefaults.colors(checkedTrackColor = colors.success, checkedThumbColor = Color.White),
                        )
                    }
                }
                val error = model.unlockError
                if (error != null) {
                    Text(error.resolve(), style = Type.footnote, color = colors.failure)
                } else {
                    Text(stringResource(R.string.screen_password_detail), style = Type.footnote, color = colors.secondary)
                }
            }
        }
    }
}
