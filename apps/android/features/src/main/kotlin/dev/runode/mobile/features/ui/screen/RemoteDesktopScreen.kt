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
import androidx.compose.ui.input.pointer.PointerInputScope
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.layout
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalFocusManager
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
 * 电脑的整块屏幕。触屏模式点按是左键，长按是右键，单指拖是按着左键拖；触控板模式单指挪光标，点按是左键，双指点按是
 * 右键，长按后拖是按着左键拖。双指滑是滚动，捏合放大，放大后双指拖着看别处。返回按钮浮在左上角，右上角是解锁、切换
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
    val currentShown by rememberUpdatedState(shown)
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
                    screenGestures(model, { currentShown }) { centroid, panChange, zoomChange ->
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
 * 画面上的手势，[shown] 是画面现在摆在哪里（放大后的）。双指捏合或放大后的双指拖交给 [transform]（捏合中心、平移、
 * 缩放倍数），没放大时双指滑是滚动。
 */
private suspend fun PointerInputScope.screenGestures(
    model: ScreenModel,
    shown: () -> Rect,
    transform: (centroid: Offset, pan: Offset, zoom: Float) -> Unit,
) = awaitEachGesture {
    val down = awaitFirstDown(requireUnconsumed = false)
    val slop = viewConfiguration.touchSlop
    val trackpad = model.pointerMode == ScreenModel.PointerMode.Trackpad
    fun fraction(point: Offset): Pair<Double, Double> {
        val rect = shown()
        if (rect.width <= 0f || rect.height <= 0f) return 0.0 to 0.0
        return ScreenModel.clamped(((point.x - rect.left) / rect.width).toDouble()) to
            ScreenModel.clamped(((point.y - rect.top) / rect.height).toDouble())
    }
    fun cursorDelta(from: Offset, to: Offset) {
        val rect = shown()
        if (rect.width <= 0f || rect.height <= 0f) return
        model.moveCursor(((to.x - from.x) / rect.width).toDouble(), ((to.y - from.y) / rect.height).toDouble())
    }
    // 等到长按的时限还没分出来（没抬起、没挪开、没落下第二根手指）就是长按，为空。
    val opening = withTimeoutOrNull(viewConfiguration.longPressTimeoutMillis) {
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
    when (opening) {
        Opening.Tap -> when {
            trackpad -> model.clickAtCursor(ScreenMouseButton.Left)
            shown().contains(down.position) -> {
                val (x, y) = fraction(down.position)
                model.tap(x, y, down.position / density)
            }
        }
        null -> {
            // 长按：触屏模式是右键；触控板模式按下左键，拖着挪光标，松手时松开。
            if (!trackpad) {
                if (shown().contains(down.position)) {
                    val (x, y) = fraction(down.position)
                    model.rightClick(x, y)
                }
                untilReleased(down.id) { _, _ -> }
            } else {
                model.dragAtCursor(true)
                untilReleased(down.id) { from, to -> cursorDelta(from, to) }
                model.dragAtCursor(false)
            }
        }
        Opening.Drag -> {
            val start = currentEvent.changes.firstOrNull { it.id == down.id }?.position ?: down.position
            if (trackpad) {
                untilReleased(down.id) { from, to -> cursorDelta(from, to) }
            } else {
                val (x, y) = fraction(start)
                model.drag(ScreenModel.DragPhase.Began, x, y)
                val end = untilReleased(down.id) { _, to ->
                    val (mx, my) = fraction(to)
                    model.drag(ScreenModel.DragPhase.Moved, mx, my)
                }
                val (ex, ey) = fraction(end ?: start)
                model.drag(ScreenModel.DragPhase.Ended, ex, ey)
            }
        }
        Opening.TwoFingers -> {
            val began = down.uptimeMillis
            var mode = TwoFingerMode.Undecided
            var totalPan = Offset.Zero
            var totalZoom = 1f
            var ended = began
            while (true) {
                val event = awaitPointerEvent()
                ended = event.changes.maxOfOrNull { it.uptimeMillis } ?: ended
                if (event.changes.none { it.pressed }) break
                if (event.changes.count { it.pressed } < 2) continue
                val panChange = event.calculatePan()
                val zoomChange = event.calculateZoom()
                val centroid = event.calculateCentroid(useCurrent = true)
                event.changes.forEach { it.consume() }
                if (mode == TwoFingerMode.Undecided) {
                    totalPan += panChange
                    totalZoom *= zoomChange
                    val pinched = abs(1 - totalZoom) * event.calculateCentroidSize(useCurrent = true) > slop
                    mode = when {
                        pinched -> TwoFingerMode.Transform
                        totalPan.getDistance() > slop -> if (shown().width > size.width * 1.01f) TwoFingerMode.Transform else TwoFingerMode.Scroll
                        else -> TwoFingerMode.Undecided
                    }
                }
                when (mode) {
                    TwoFingerMode.Scroll -> model.scroll(panChange.x, panChange.y, shown().width)
                    TwoFingerMode.Transform -> transform(centroid, panChange, zoomChange)
                    TwoFingerMode.Undecided -> {}
                }
            }
            // 两指点一下：触控板模式是右键。
            if (mode == TwoFingerMode.Undecided && trackpad && ended - began < viewConfiguration.longPressTimeoutMillis) {
                model.clickAtCursor(ScreenMouseButton.Right)
            }
        }
    }
}

private enum class TwoFingerMode {
    Undecided,
    Scroll,
    Transform,
}

/** 跟着 [pointer] 那根手指到它抬起，每挪一下交给 [onMove]（上一个位置、这一个位置）；返回最后的位置。 */
private suspend fun androidx.compose.ui.input.pointer.AwaitPointerEventScope.untilReleased(
    pointer: androidx.compose.ui.input.pointer.PointerId,
    onMove: (Offset, Offset) -> Unit,
): Offset? {
    var last: Offset? = currentEvent.changes.firstOrNull { it.id == pointer }?.position
    while (true) {
        val event = awaitPointerEvent()
        val change = event.changes.firstOrNull { it.id == pointer } ?: return last
        val previous = last ?: change.previousPosition
        if (change.position != previous) onMove(previous, change.position)
        last = change.position
        change.consume()
        if (!change.pressed) return last
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
