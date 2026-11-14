package dev.runode.mobile.terminal

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView

/**
 * 把 [TerminalView] 嵌进 Compose。视图建好后经 [onMake] 交出去（下一轮主循环再交，免得在组合中途改状态），视图模型
 * 拿它当 [TerminalDisplay]，按键栏（[KeyboardAccessoryBar]）也要用它；页面离开组合时经 [onRelease] 交回。
 *
 * @param topObstruction 叠在终端顶上的东西（断线横幅）的高度
 * @param fontSizeOverride 用户在设置里定的字号（dp）；为空时跟随系统字体大小
 * @param acceptsInput 能打字；会话结束了时为假
 * @param dictating 正在录音，按键栏上的麦克风键显示成按下去的样子
 */
@Composable
fun Terminal(
    delegate: TerminalViewDelegate,
    modifier: Modifier = Modifier,
    topObstruction: Dp = 0.dp,
    fontSizeOverride: Float? = null,
    bellHaptics: Boolean = true,
    acceptsInput: Boolean = true,
    dictating: Boolean = false,
    onMake: (TerminalView) -> Unit = {},
    onRelease: (TerminalView) -> Unit = {},
) {
    val obstruction = with(LocalDensity.current) { topObstruction.toPx() }
    AndroidView(
        factory = { context ->
            TerminalView(context).also { view ->
                view.delegate = delegate
                view.post { onMake(view) }
            }
        },
        modifier = modifier,
        update = { view ->
            view.delegate = delegate
            view.topObstruction = obstruction
            view.fontSizeOverride = fontSizeOverride
            view.bellHaptics = bellHaptics
            view.acceptsInput = acceptsInput
            view.dictating = dictating
        },
        onRelease = { view ->
            view.delegate = null
            onRelease(view)
        },
    )
}
