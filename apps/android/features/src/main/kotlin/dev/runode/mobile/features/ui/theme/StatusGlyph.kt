package dev.runode.mobile.features.ui.theme

import android.provider.Settings
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius as GeometryCornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.scale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import dev.runode.mobile.features.model.SessionStatus
import dev.runode.mobile.protocol.AgentState
import kotlinx.coroutines.delay

/** 状态标记画的是哪一种。 */
enum class StatusMark {
    /** 琥珀色的实心方块光标：agent 在等你回答。 */
    Waiting,

    /** 依次点亮的六点盲文格：agent 在干活。 */
    Working,

    /** 提示符 `>_`：普通的 shell，或者空闲的 agent。 */
    Idle,

    /** 绿色的对勾：做完了。 */
    Done,
    ;

    companion object {
        fun of(status: SessionStatus): StatusMark = when (status) {
            SessionStatus.Waiting -> Waiting
            SessionStatus.Working -> Working
            else -> Idle
        }

        fun of(state: AgentState): StatusMark = when (state) {
            AgentState.Blocked -> Waiting
            AgentState.Working -> Working
            else -> Idle
        }
    }
}

/** 系统设置里关掉了动画（开发者选项或无障碍里的「移除动画」）。 */
@Composable
fun reduceMotion(): Boolean {
    val resolver = LocalContext.current.contentResolver
    return remember(resolver) { Settings.Global.getFloat(resolver, Settings.Global.ANIMATOR_DURATION_SCALE, 1f) == 0f }
}

/**
 * 状态标记，边长 [size]，在 24 格的坐标里画。只有图形，读屏读不到，状态由旁边的文字说。盲文格按绝对时间走，几处同时在
 * 干活的标记步调一致；关掉了动画时停在前三个点亮。
 */
@Composable
fun StatusGlyph(mark: StatusMark, size: Dp, modifier: Modifier = Modifier) {
    val colors = themeColors
    val animates = mark == StatusMark.Working && !reduceMotion()
    var tick by remember { mutableLongStateOf(STILL_TICK) }
    if (animates) {
        LaunchedEffect(Unit) {
            while (true) {
                val now = System.currentTimeMillis()
                tick = now / STEP_MILLIS
                delay(STEP_MILLIS - now % STEP_MILLIS)
            }
        }
    }
    val step = if (animates) tick else STILL_TICK
    Canvas(modifier.size(size)) {
        val unit = this.size.width / 24f
        scale(unit, unit, pivot = Offset.Zero) {
            when (mark) {
                StatusMark.Waiting -> drawRoundRect(
                    colors.waiting,
                    topLeft = Offset(5.5f, 1f),
                    size = Size(13f, 22f),
                    cornerRadius = GeometryCornerRadius(1.5f),
                )
                StatusMark.Working -> DOTS.forEachIndexed { index, center ->
                    drawCircle(
                        colors.working,
                        radius = DOT_RADIUS,
                        center = center,
                        alpha = if (isLit(index, step)) 1f else DIM_ALPHA,
                    )
                }
                StatusMark.Idle -> drawPath(
                    Path().apply {
                        moveTo(5f, 17f)
                        lineTo(11f, 12f)
                        lineTo(5f, 7f)
                        moveTo(13f, 18f)
                        lineTo(20f, 18f)
                    },
                    colors.secondary,
                    style = Stroke(width = 2.2f, cap = StrokeCap.Round, join = StrokeJoin.Round),
                )
                StatusMark.Done -> drawPath(
                    Path().apply {
                        moveTo(5f, 12f)
                        lineTo(10f, 17f)
                        lineTo(19f, 7f)
                    },
                    colors.success,
                    style = Stroke(width = 2.8f, cap = StrokeCap.Round, join = StrokeJoin.Round),
                )
            }
        }
    }
}

/** 第 [index] 个点在第 [tick] 步亮不亮：每个点比前一个晚一步亮起，亮三步。 */
private fun isLit(index: Int, tick: Long): Boolean = Math.floorMod(tick - index, DOTS.size.toLong()) < 3

/** 盲文格每走一步的毫秒数。 */
private const val STEP_MILLIS = 150L

/** 不动时停的那一步：亮的正好是前三个点。 */
private const val STILL_TICK = 2L

/** 盲文格六个点的圆心，按点亮的先后。 */
private val DOTS = listOf(
    Offset(7.5f, 4f), Offset(16.5f, 4f), Offset(16.5f, 12f),
    Offset(16.5f, 20f), Offset(7.5f, 20f), Offset(7.5f, 12f),
)
private const val DOT_RADIUS = 2.5f
private const val DIM_ALPHA = 0.22f

/**
 * 首页大标题后面闪的光标，表示有 agent 在等你回答。一个周期里前一段亮、后一段暗下去，按绝对时间对齐到周期；关掉了动画
 * 时常亮。读屏读不到。
 */
@Composable
fun BlinkingCursor(modifier: Modifier = Modifier, width: Dp = 13.dp, height: Dp = 26.dp) {
    val colors = themeColors
    val still = reduceMotion()
    var lit by remember { mutableLongStateOf(1L) }
    if (!still) {
        LaunchedEffect(Unit) {
            while (true) {
                val phase = System.currentTimeMillis() % BLINK_PERIOD
                lit = if (phase < BLINK_LIT) 1L else 0L
                delay(if (phase < BLINK_LIT) BLINK_LIT - phase else BLINK_PERIOD - phase)
            }
        }
    }
    val alpha = if (still || lit == 1L) 1f else BLINK_DIM
    Canvas(modifier.size(width, height)) {
        drawRoundRect(colors.waiting, cornerRadius = GeometryCornerRadius(2.dp.toPx()), alpha = alpha)
    }
}

/** 一亮一暗的周期，毫秒。 */
private const val BLINK_PERIOD = 1200L

/** 一个周期里亮着的那段，毫秒。 */
private const val BLINK_LIT = 720L
private const val BLINK_DIM = 0.3f
