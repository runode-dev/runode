package dev.runode.mobile.terminal

import dev.runode.mobile.protocol.Rgb
import dev.runode.mobile.protocol.TermSettings
import kotlin.time.Duration

// 画一屏要的全部数据，从 libghostty 的 render state 抄出来的纯 Kotlin 值。画的一方只读这些，不碰 libghostty。

/** 单元格占几列。 */
enum class CellWidth {
    Narrow,

    /** 宽字符的头一格，画的时候占两列。 */
    Wide,

    /** 宽字符的后一格，不画。 */
    SpacerTail,

    /** 行末放不下宽字符时的占位，不画。 */
    SpacerHead,
}

/** 下划线的样式，对应 SGR 4:x；`ordinal` 就是 x。 */
enum class UnderlineStyle {
    None, Single, Double, Curly, Dotted, Dashed,
}

@JvmInline
value class CellAttributes(val rawValue: Int) {
    operator fun contains(other: CellAttributes): Boolean = rawValue and other.rawValue == other.rawValue

    operator fun plus(other: CellAttributes): CellAttributes = CellAttributes(rawValue or other.rawValue)

    val isEmpty: Boolean get() = rawValue == 0

    override fun toString(): String = "CellAttributes(0x${rawValue.toString(16)})"

    companion object {
        val none = CellAttributes(0)
        val bold = CellAttributes(1 shl 0)
        val italic = CellAttributes(1 shl 1)
        val faint = CellAttributes(1 shl 2)
        val inverse = CellAttributes(1 shl 3)
        val invisible = CellAttributes(1 shl 4)
        val strikethrough = CellAttributes(1 shl 5)
        val blink = CellAttributes(1 shl 6)
    }
}

data class ScreenCell(
    /** 整个字素簇；空格子为空串。 */
    val text: String,
    /** 解析过调色板的前景色；为空用默认前景。 */
    val foreground: Rgb?,
    /** 解析过调色板的背景色；为空用默认背景。 */
    val background: Rgb?,
    val attributes: CellAttributes,
    val underline: UnderlineStyle,
    val width: CellWidth,
) {
    companion object {
        val blank = ScreenCell(
            text = "", foreground = null, background = null, attributes = CellAttributes.none,
            underline = UnderlineStyle.None, width = CellWidth.Narrow,
        )
    }
}

enum class CursorShape {
    Block, BlockHollow, Bar, Underline,
}

data class ScreenCursor(
    val column: Int,
    val row: Int,
    val shape: CursorShape,
    val blinking: Boolean,
    /** 光标在宽字符的后一格上。 */
    val onWideTail: Boolean,
)

/** 图片画在哪一层，按画的先后排。 */
enum class ImageLayer {
    BelowBackground, BelowText, AboveText,
}

/**
 * 程序用 Kitty 图形协议放在屏幕上的一块图片。位置和大小以单元格计，可以有小数；左上角相对视口，图片上半截滚出了
 * 视口时为负。像素不在这里，画的一方按 [id] 和 [generation] 向 [VTerminal.image] 要，同一个 [generation] 的
 * 像素不会变，可以直接拿来当缓存键。
 */
data class ScreenImage(
    val id: UInt,
    val generation: ULong,
    val x: Double,
    val y: Double,
    val width: Double,
    val height: Double,
    /** 只画图片的这一块（以图片像素计），缩放到上面的大小。 */
    val sourceX: UInt,
    val sourceY: UInt,
    val sourceWidth: UInt,
    val sourceHeight: UInt,
    val layer: ImageLayer,
)

/** 一屏的内容。[VTerminal.refresh] 就地改它，只动变了的部分。 */
class ScreenFrame {
    var columns: Int = 0
    var rows: Int = 0

    /** 一行一个数组，每个数组 [columns] 个格子。 */
    var cells: Array<Array<ScreenCell>> = emptyArray()

    /** 视口上面那一行，平滑滚动错开时从顶上露出一部分；视口在历史最顶上时为空。 */
    var above: Array<ScreenCell> = emptyArray()

    /** 平滑滚动时整屏往下错开几分之一行，在 [0, 1) 里；[above] 为空时总是 0。 */
    var scrollOffset: Double = 0.0
    var background: Rgb = TermSettings.default.background
    var foreground: Rgb = TermSettings.default.foreground

    /** 程序或主题明确设了的光标色。 */
    var cursorColor: Rgb? = null

    /** 光标可见且在视口里时才有。 */
    var cursor: ScreenCursor? = null

    /** 屏幕上的图片，按画的先后排好（先按层，同层按 z 值）。 */
    var images: List<ScreenImage> = emptyList()

    /** 图片动画下一帧离现在多久该画；没有在播的动画时为空。 */
    var nextImageFrame: Duration? = null

    /** 屏幕上的文字，一行一个，行尾空白去掉；宽字符的占位格不算。 */
    val lines: List<String>
        get() = cells.map(::text)

    companion object {
        /** 一行的文字，行尾空白去掉；宽字符的占位格不算。 */
        fun text(row: Array<ScreenCell>): String {
            val line = StringBuilder()
            for (cell in row) {
                if (cell.width == CellWidth.SpacerTail || cell.width == CellWidth.SpacerHead) continue
                line.append(cell.text.ifEmpty { " " })
            }
            return line.trimEnd(' ').toString()
        }
    }
}

/** 一次刷新改了哪些：整屏，还是某几行。 */
data class FrameChange(
    val full: Boolean,
    val rows: List<Int>,
    /** 光标变了（位置、形状、可见）。 */
    val cursorChanged: Boolean,
    /** 视口上面那一行（[ScreenFrame.above]）变了。 */
    val aboveChanged: Boolean,
    /** 图片（[ScreenFrame.images]）变了：放了、删了、挪了，或者动画换了一帧。 */
    val imagesChanged: Boolean,
) {
    val isEmpty: Boolean get() = !full && rows.isEmpty() && !cursorChanged && !aboveChanged && !imagesChanged

    companion object {
        val none = FrameChange(
            full = false, rows = emptyList(), cursorChanged = false, aboveChanged = false, imagesChanged = false,
        )
    }
}
