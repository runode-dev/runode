package dev.runode.mobile.terminal

import android.view.SoundEffectConstants
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.ScrollState
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.graphics.BlendMode
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.CompositingStrategy
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** 按键栏上按钮的配色。 */
@Immutable
data class KeyBarPalette(
    val key: Color,
    val border: Color,
    val label: Color,
    /** 按下去的样子（粘住的修饰键、开着的键盘、正在录音）的底和字。 */
    val selectedKey: Color,
    val selectedLabel: Color,
)

private val LocalKeyBarPalette = staticCompositionLocalOf<KeyBarPalette?> { null }

/**
 * 一条快捷键栏的底座：左边一排横着能拖的键（[keys]），右端几个不跟着滚走的键（[trailing]）。不画底色，
 * 透出调用方的背景；键盘弹出、收起时栏不变高矮。按钮的样子在 [KeyBarKey]，各条栏只管摆哪些键。
 * 那排键还能往哪边拖，哪边的边缘就渐隐下去，看得出后面还有键。
 *
 * @param palette 按钮的配色；为空时用 Material 主题的颜色
 */
@Composable
fun KeyBar(
    modifier: Modifier = Modifier,
    palette: KeyBarPalette? = null,
    trailing: @Composable RowScope.() -> Unit = {},
    keys: @Composable RowScope.() -> Unit,
) {
    val scroll = rememberScrollState()
    CompositionLocalProvider(LocalKeyBarPalette provides palette) {
        Row(
            modifier = modifier
                .height(44.dp)
                .padding(vertical = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Row(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxHeight()
                    .fadingEdges(scroll)
                    .horizontalScroll(scroll)
                    .padding(horizontal = 8.dp),
                horizontalArrangement = Arrangement.spacedBy(4.dp),
                content = keys,
            )
            Row(
                modifier = Modifier
                    .fillMaxHeight()
                    .padding(end = 8.dp),
                horizontalArrangement = Arrangement.spacedBy(4.dp),
                content = trailing,
            )
        }
    }
}

/** 还能往左、往右拖时，那一边 [FadeWidth] 宽的一段渐隐成透明；放得下时不遮。 */
private fun Modifier.fadingEdges(scroll: ScrollState): Modifier = this
    .graphicsLayer { compositingStrategy = CompositingStrategy.Offscreen }
    .drawWithContent {
        drawContent()
        val edge = FadeWidth.toPx().coerceAtMost(size.width / 2)
        if (edge <= 0f) return@drawWithContent
        if (scroll.value > 0) {
            drawRect(
                Brush.horizontalGradient(listOf(Color.Transparent, Color.Black), startX = 0f, endX = edge),
                blendMode = BlendMode.DstIn,
            )
        }
        if (scroll.value < scroll.maxValue) {
            drawRect(
                Brush.horizontalGradient(listOf(Color.Black, Color.Transparent), startX = size.width - edge, endX = size.width),
                blendMode = BlendMode.DstIn,
            )
        }
    }

private val FadeWidth = 28.dp

/**
 * 按键栏上的一个键：[title] 是键上的字，给了 [icon] 时画图标、不写字；[label] 报给读屏。按下时响一声按键音再做
 * [onClick]；[onClick] 为空时不接点按，给要自己接按下、松开的键用（经 [modifier] 加手势）。[selected] 为真时画成
 * 按下去的样子。
 */
@Composable
fun KeyBarKey(
    title: String,
    label: String,
    modifier: Modifier = Modifier,
    icon: ImageVector? = null,
    selected: Boolean = false,
    stateDescription: String? = null,
    onClick: (() -> Unit)? = null,
) {
    val palette = LocalKeyBarPalette.current
    val colors = MaterialTheme.colorScheme
    val keyColor = palette?.let { if (selected) it.selectedKey else it.key }
        ?: if (selected) colors.onSurface else colors.surfaceVariant
    val labelColor = palette?.let { if (selected) it.selectedLabel else it.label }
        ?: if (selected) colors.surface else colors.onSurface
    val border = palette?.let { BorderStroke(1.dp, if (selected) it.selectedKey else it.border) }
    val shape = RoundedCornerShape(10.dp)
    val view = LocalView.current
    var key = modifier
        .fillMaxHeight()
        .widthIn(min = 36.dp)
        .background(keyColor, shape)
    if (border != null) key = key.border(border, shape)
    if (onClick != null) {
        key = key.clickable(role = Role.Button) {
            view.playSoundEffect(SoundEffectConstants.CLICK)
            onClick()
        }
    }
    // 读屏只看到一个节点：clickable 已经把键上的字合并进来，这里只补说明和状态；没有 clickable 时自己合并。
    key = key.semantics(mergeDescendants = onClick == null) {
        contentDescription = label
        if (onClick == null) role = Role.Button
        this.selected = selected
        if (stateDescription != null) this.stateDescription = stateDescription
    }
    Box(
        modifier = key.padding(KeyPadding),
        contentAlignment = Alignment.Center,
    ) {
        if (icon != null) {
            Icon(icon, contentDescription = null, tint = labelColor, modifier = Modifier.size(16.dp))
        } else {
            Text(text = title, color = labelColor, fontFamily = FontFamily.Monospace, fontSize = 13.sp)
        }
    }
}

private val KeyPadding = PaddingValues(horizontal = 10.dp, vertical = 4.dp)
