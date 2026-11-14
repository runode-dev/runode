package dev.runode.mobile.features.ui.theme

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.clickable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.LocalTextStyle
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.runode.mobile.features.model.AppTheme
import dev.runode.mobile.features.model.StatusPalette
import dev.runode.mobile.protocol.Rgb
import dev.runode.mobile.protocol.TermSettings

/** 协议里的颜色换成 Compose 的。 */
fun Rgb.toColor(): Color = Color(r.toInt(), g.toInt(), b.toInt())

/**
 * 终端页以外的界面上的几种底色和字色，从主题（[AppTheme]）的底色和前景色推出来，进出终端页时底色和明暗不跳。界面上不直接
 * 用 Material 的颜色，一律从 [LocalThemeColors] 取。状态色取自 `StatusPalette`，只表示状态。
 */
@Immutable
data class ThemeColors(
    val isDark: Boolean,
    /** 页面的底。 */
    val page: Color,
    /** 卡片的底。 */
    val card: Color,
    /** 卡片里的小块（输入框、图标底）：前景色加透明度，放在页面和卡片上都看得出来。 */
    val fill: Color,
    /** 按下时盖在卡片上的一层。 */
    val pressed: Color,
    /** 卡片里凸起的一块：按键、选项、标签。 */
    val raised: Color,
    /** 凹下去的底槽：终端摘录、输入框、分段控件的底。 */
    val well: Color,
    /** 卡片的边和分隔线。 */
    val hairline: Color,
    /** 次要按钮的边、发送键的底。 */
    val strongHairline: Color,
    /** 首页卡片底下露出来的那截卡边。 */
    val stackEdge: Color,
    /** 正文的字。 */
    val primary: Color,
    /** 说明的字。 */
    val secondary: Color,
    /** 更淡的字：目录、标签页号、箭头。 */
    val tertiary: Color,
    val waiting: Color,
    val waitingFill: Color,
    val onWaitingFill: Color,
    val working: Color,
    val success: Color,
    val failure: Color,
    /** diff 里加的行。 */
    val diffAdded: Color,
    /** diff 里删的行。 */
    val diffRemoved: Color,
) {
    constructor(theme: AppTheme) : this(theme, theme.foreground.toColor(), theme.status)

    private constructor(theme: AppTheme, foreground: Color, status: StatusPalette) : this(
        isDark = theme.isDark,
        page = theme.page.toColor(),
        card = theme.card.toColor(),
        fill = foreground.copy(alpha = FILL_ALPHA),
        pressed = foreground.copy(alpha = PRESSED_ALPHA),
        raised = theme.raised.toColor(),
        well = theme.well.toColor(),
        hairline = theme.hairline.toColor(),
        strongHairline = theme.strongHairline.toColor(),
        stackEdge = theme.stackEdge.toColor(),
        primary = foreground,
        secondary = foreground.copy(alpha = SECONDARY_ALPHA),
        tertiary = foreground.copy(alpha = TERTIARY_ALPHA),
        waiting = status.waiting.toColor(),
        waitingFill = status.waitingFill.toColor(),
        onWaitingFill = status.onWaitingFill.toColor(),
        working = status.working.toColor(),
        success = status.success.toColor(),
        failure = status.failure.toColor(),
        diffAdded = status.diffAdded.toColor(),
        diffRemoved = status.diffRemoved.toColor(),
    )

    /** Material 部件（菜单、对话框、开关、输入框）用的配色：强调色是前景色，黑白为主，不用 Material 的紫色。 */
    fun colorScheme(): ColorScheme {
        val base = if (isDark) darkColorScheme() else lightColorScheme()
        return base.copy(
            primary = primary,
            onPrimary = page,
            secondary = primary,
            onSecondary = page,
            tertiary = primary,
            background = page,
            onBackground = primary,
            surface = page,
            onSurface = primary,
            surfaceVariant = raised,
            onSurfaceVariant = secondary,
            surfaceContainerLowest = page,
            surfaceContainerLow = card,
            surfaceContainer = card,
            surfaceContainerHigh = card,
            surfaceContainerHighest = raised,
            outline = strongHairline,
            outlineVariant = hairline,
            error = failure,
        )
    }

    private companion object {
        const val FILL_ALPHA = 0.10f
        const val PRESSED_ALPHA = 0.18f
        const val SECONDARY_ALPHA = 0.64f
        const val TERTIARY_ALPHA = 0.40f
    }
}

val LocalThemeColors = staticCompositionLocalOf { ThemeColors(AppTheme(TermSettings.default)) }

/** 现在套着的配色。 */
val themeColors: ThemeColors
    @Composable get() = LocalThemeColors.current

/**
 * 套用主题：配色放进 [LocalThemeColors]，Material 的部件按 [ThemeColors.colorScheme] 上色，字默认是正文色。根界面套一次，
 * 终端页按它那个会话的主题再套一次。
 */
@Composable
fun RunodeTheme(theme: AppTheme, content: @Composable () -> Unit) {
    val colors = remember(theme) { ThemeColors(theme) }
    MaterialTheme(colorScheme = remember(colors) { colors.colorScheme() }) {
        CompositionLocalProvider(
            LocalThemeColors provides colors,
            LocalContentColor provides colors.primary,
            LocalTextStyle provides Type.body,
            content = content,
        )
    }
}

/** 几档字：照系统动态字体的几种用途定字号，跟着系统的字体大小缩放（sp）。 */
object Type {
    val largeTitle = TextStyle(fontSize = 34.sp, lineHeight = 41.sp, fontWeight = FontWeight.ExtraBold)
    val title = TextStyle(fontSize = 28.sp, lineHeight = 34.sp, fontWeight = FontWeight.SemiBold)
    val title2 = TextStyle(fontSize = 22.sp, lineHeight = 28.sp, fontWeight = FontWeight.Bold)
    val headline = TextStyle(fontSize = 17.sp, lineHeight = 22.sp, fontWeight = FontWeight.SemiBold)
    val body = TextStyle(fontSize = 17.sp, lineHeight = 22.sp)
    val callout = TextStyle(fontSize = 16.sp, lineHeight = 21.sp)
    val subheadline = TextStyle(fontSize = 15.sp, lineHeight = 20.sp)
    val footnote = TextStyle(fontSize = 13.sp, lineHeight = 18.sp)
    val caption = TextStyle(fontSize = 12.sp, lineHeight = 16.sp)
}

/** 界面上的几档圆角。 */
object CornerRadius {
    /** 卡片：列表里的一块、首页的统计格、终端页的横幅。 */
    val card: Dp = 20.dp

    /** 几行并成一张的列表卡。 */
    val group: Dp = 18.dp

    /** 底部的大按钮。 */
    val action: Dp = 15.dp

    /** 卡片里的小块：输入框、预览、图标底、一般的按钮。 */
    val inner: Dp = 12.dp

    /** 按键。 */
    val key: Dp = 10.dp
}

object Shapes {
    val card = RoundedCornerShape(CornerRadius.card)
    val group = RoundedCornerShape(CornerRadius.group)
    val action = RoundedCornerShape(CornerRadius.action)
    val inner = RoundedCornerShape(CornerRadius.inner)
    val key = RoundedCornerShape(CornerRadius.key)
}

/** 画成一张卡片：[ThemeColors.card] 的底，[shape] 的圆角，描一圈 1dp 的 [ThemeColors.hairline]，四边留 [padding]。 */
fun Modifier.cardBackground(colors: ThemeColors, padding: Dp = 12.dp, shape: Shape = Shapes.card): Modifier = this
    .background(colors.card, shape)
    .border(1.dp, colors.hairline, shape)
    .padding(padding)

/** 按钮的几种面。颜色只表示状态，除了 [Waiting] 都是中性色。 */
enum class ActionKind {
    /** 实心，底是前景色、字用页面的底色：一屏里最要紧的那个操作。 */
    Primary,

    /** 卡片的底加一圈 [ThemeColors.strongHairline]。 */
    Secondary,

    /** 凸起的浅块，不描边：卡片里的按键、选项。 */
    Raised,

    /** 琥珀实心：回答在等你的 agent。 */
    Waiting,
    ;

    fun background(colors: ThemeColors): Color = when (this) {
        Primary -> colors.primary
        Secondary -> colors.card
        Raised -> colors.raised
        Waiting -> colors.waitingFill
    }

    fun foreground(colors: ThemeColors): Color = when (this) {
        Primary -> colors.page
        Waiting -> colors.onWaitingFill
        Secondary, Raised -> colors.primary
    }

    val weight: FontWeight
        get() = when (this) {
            Primary, Waiting -> FontWeight.SemiBold
            Secondary, Raised -> FontWeight.Medium
        }
}

/**
 * 套上 [kind] 的面的按钮：至少高 [height]，圆角 [cornerRadius]；按下时变淡，不可用时整块变淡。宽度和左右的空由调用方定。
 * 实心的 [ActionKind.Primary] 不可用时换成灰的填充色和次要色的字：浅色的实心块只变淡，看着还像能按。
 * [selected] 不为空时是能选中的一项，读屏念出选没选中。
 */
@Composable
fun SurfaceButton(
    kind: ActionKind,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    height: Dp = 44.dp,
    cornerRadius: Dp = CornerRadius.inner,
    selected: Boolean? = null,
    contentPadding: Dp = 0.dp,
    content: @Composable RowScope.() -> Unit,
) {
    val colors = themeColors
    val shape = RoundedCornerShape(cornerRadius)
    val interaction = remember { MutableInteractionSource() }
    val pressed by interaction.collectIsPressedAsState()
    val click = if (selected != null) {
        Modifier.selectable(selected, enabled = enabled, role = Role.Button, interactionSource = interaction, indication = null, onClick = onClick)
    } else {
        Modifier.clickable(enabled = enabled, role = Role.Button, interactionSource = interaction, indication = null, onClick = onClick)
    }
    val grayedOut = !enabled && kind == ActionKind.Primary
    var surface = modifier
        .alpha(if (!enabled && !grayedOut) DISABLED_ALPHA else if (pressed) PRESSED_ALPHA else 1f)
        .defaultMinSize(minHeight = height)
        .background(if (grayedOut) colors.fill else kind.background(colors), shape)
    if (kind == ActionKind.Secondary) surface = surface.border(BorderStroke(1.dp, colors.strongHairline), shape)
    Row(
        modifier = surface.then(click).padding(horizontal = contentPadding),
        horizontalArrangement = Arrangement.Center,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        CompositionLocalProvider(
            LocalContentColor provides if (grayedOut) colors.secondary else kind.foreground(colors),
            LocalTextStyle provides LocalTextStyle.current.copy(fontWeight = kind.weight),
        ) { content() }
    }
}

private const val DISABLED_ALPHA = 0.4f
private const val PRESSED_ALPHA = 0.7f
