package dev.runode.mobile.features.model

import dev.runode.mobile.protocol.Rgb
import dev.runode.mobile.protocol.TermSettings
import kotlinx.serialization.Serializable

/**
 * 终端页以外的界面（首页、会话列表、设置、配对）用的配色，从电脑上终端的主题推出来，进出终端页时底色和明暗不跳。
 * 深色主题照系统深色模式的样子：页面铺终端的底色，卡片往前景色提亮一点；浅色主题照系统浅色模式：卡片是终端的底色，
 * 页面往前景色压暗一点。字、强调色仍用系统的，按 [isDark] 选深色或浅色模式；状态的颜色取 [status]。
 */
@Serializable
data class AppTheme(
    /** 终端的底色。 */
    val background: Rgb,
    /** 终端的前景色。 */
    val foreground: Rgb,
) {
    constructor(settings: TermSettings) : this(settings.background, settings.foreground)

    val isDark: Boolean get() = background.isDark

    /** 页面的底色，相当于系统的分组列表底色。 */
    val page: Rgb get() = if (isDark) background else background.mixed(foreground, 0.05)

    /** 卡片的底色，相当于系统分组列表里一行的底色。 */
    val card: Rgb get() = if (isDark) background.mixed(foreground, 0.05) else background

    /** 卡片里凸起的一块：按键、选项、标签。 */
    val raised: Rgb get() = background.mixed(foreground, if (isDark) 0.09 else 0.06)

    /** 凹下去的底槽：终端摘录、输入框、分段控件的底。深色时比页面还暗，浅色时比卡片略灰。 */
    val well: Rgb get() = if (isDark) background.mixed(Rgb(0x000000u), 0.35) else background.mixed(foreground, 0.04)

    /** 卡片的边和分隔线。 */
    val hairline: Rgb get() = background.mixed(foreground, 0.12)

    /** 比 [hairline] 显眼的线：次要按钮的边、发送键的底。 */
    val strongHairline: Rgb get() = background.mixed(foreground, if (isDark) 0.16 else 0.18)

    /** 首页卡片底下露出来的那截后一张卡的边。 */
    val stackEdge: Rgb get() = background.mixed(foreground, if (isDark) 0.07 else 0.02)

    /** 表示状态的几种颜色，按明暗选一组。 */
    val status: StatusPalette get() = if (isDark) StatusPalette.dark else StatusPalette.light
}

/**
 * 只用来表示状态的颜色：琥珀是等你回答，蓝是在干活，绿是成功，红是失败；界面上别的地方不用彩色。
 * 浅色那组的字色在浅色页面上要有 4.5:1 的对比度。
 */
data class StatusPalette(
    /** 等你回答的标记和字。 */
    val waiting: Rgb,
    /** 等你回答时实心按钮的底。 */
    val waitingFill: Rgb,
    /** [waitingFill] 上的字。 */
    val onWaitingFill: Rgb,
    val working: Rgb,
    val success: Rgb,
    val failure: Rgb,
    /** diff 里加的行。 */
    val diffAdded: Rgb,
    /** diff 里删的行。 */
    val diffRemoved: Rgb,
) {
    companion object {
        val dark = StatusPalette(
            waiting = Rgb(0xF5A83Cu), waitingFill = Rgb(0xF5A83Cu), onWaitingFill = Rgb(0x1B1203u),
            working = Rgb(0x7FA9FFu), success = Rgb(0x7FD49Bu), failure = Rgb(0xF0958Au),
            diffAdded = Rgb(0x9BE0B2u), diffRemoved = Rgb(0xF0958Au),
        )

        val light = StatusPalette(
            waiting = Rgb(0x9C5B00u), waitingFill = Rgb(0xF5A83Cu), onWaitingFill = Rgb(0x1B1203u),
            working = Rgb(0x2F66D0u), success = Rgb(0x197A42u), failure = Rgb(0xC23A2Eu),
            diffAdded = Rgb(0x197A42u), diffRemoved = Rgb(0xC23A2Eu),
        )
    }
}

/** 按相对亮度看是深色还是浅色背景，决定上面的导航栏、底栏用深色还是浅色模式。 */
val Rgb.isDark: Boolean
    get() = 0.2126 * r.toDouble() + 0.7152 * g.toDouble() + 0.0722 * b.toDouble() < 128

/** 往 [other] 靠 [amount]（0 到 1）的颜色，每个分量四舍五入。 */
fun Rgb.mixed(other: Rgb, amount: Double): Rgb {
    fun channel(a: UByte, b: UByte): UByte =
        Math.round(a.toDouble() + (b.toDouble() - a.toDouble()) * amount).toInt().toUByte()
    return Rgb(channel(r, other.r), channel(g, other.g), channel(b, other.b))
}
