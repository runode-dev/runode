package dev.runode.mobile.terminal

import dev.runode.mobile.protocol.Rgb
import dev.runode.mobile.protocol.TermSettings
import dev.runode.mobile.protocol.TerminalColor
import kotlin.math.abs
import kotlin.math.floor
import kotlin.math.hypot
import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt

/** 拖柄贴着的那条格子边：[x] 是边的横坐标，[top]、[bottom] 是那一行的上下沿。 */
internal data class HandleAnchor(val x: Float, val top: Float, val bottom: Float)

/** 选中的格子的字色和底色。 */
internal data class SelectionColors(val foreground: Rgb, val background: Rgb)

/** 选区的几何和配色，不碰 View。坐标以网格左上角为原点，单位是像素。 */
internal object SelectionGeometry {
    /** 没配选区底色时，深色背景上的选区底色。 */
    val ON_DARK = Rgb(0x264F78u)

    /** 没配选区底色时，浅色背景上的选区底色。 */
    val ON_LIGHT = Rgb(0xADD6FFu)

    /** 拖柄刚越出边界时自动滚动、平移的速度（格每秒）。 */
    const val MIN_EDGE_CELLS_PER_SECOND = 6f

    /** 拖柄每多越出一格，速度加快多少（格每秒）。 */
    const val EDGE_CELLS_PER_SECOND_PER_CELL = 8f

    /** 自动滚动、平移的速度上限（格每秒）。 */
    const val MAX_EDGE_CELLS_PER_SECOND = 40f

    /**
     * 拖柄拖到 ([x], [y]) 时选区那端落在哪一格：横向按 [x] 四舍五入到格子边界，起点取边界右边那格，终点取左边那格；
     * 纵向取 [y] 所在的行。都夹在 [columns] × [rows] 的网格里。
     */
    fun handleCell(
        end: SelectionEnd, x: Float, y: Float, cellWidth: Float, cellHeight: Float, columns: Int, rows: Int,
    ): GridCell {
        val boundary = (x / cellWidth).roundToInt()
        val column = if (end == SelectionEnd.Start) boundary else boundary - 1
        val row = floor(y / cellHeight).toInt()
        return GridCell(column.coerceIn(0, max(columns - 1, 0)), row.coerceIn(0, max(rows - 1, 0)))
    }

    /**
     * 选区那端的格子 [cell]（宽度 [width]）上拖柄贴着的边：起点在格子左边缘，落在宽字符后一格时退到前一格；终点在
     * 格子右边缘，宽字符的头一格算两格宽。
     */
    fun handleAnchor(end: SelectionEnd, cell: GridCell, width: CellWidth?, cellWidth: Float, cellHeight: Float): HandleAnchor {
        val edge = when (end) {
            SelectionEnd.Start -> if (width == CellWidth.SpacerTail) max(cell.column - 1, 0) else cell.column
            SelectionEnd.End -> cell.column + if (width == CellWidth.Wide) 2 else 1
        }
        return HandleAnchor(edge * cellWidth, cell.row * cellHeight, (cell.row + 1) * cellHeight)
    }

    /**
     * 拖柄图的左边：拖柄图挂在行底下，起点柄的尖在图宽的 3/4 处、往左伸，终点柄的尖在 1/4 处、往右伸，尖对着
     * [anchorX]。
     */
    fun handleLeft(end: SelectionEnd, anchorX: Float, handleWidth: Float): Float =
        if (end == SelectionEnd.Start) anchorX - handleWidth * 3 / 4 else anchorX - handleWidth / 4

    /**
     * 手指按在 ([x], [y]) 时按住的是哪个拖柄：只认挂在行底下的拖柄图，行里的字照样能点；离得不超过 [slop] 的里取
     * 最近的，两个一样近时取横向离拖柄图中线近的，还一样时取终点。看不见的那端传空。
     */
    fun hitHandle(
        x: Float, y: Float, start: HandleAnchor?, end: HandleAnchor?, handleWidth: Float, handleHeight: Float,
        slop: Float,
    ): SelectionEnd? {
        var best: SelectionEnd? = null
        var bestDistance = Float.MAX_VALUE
        var bestSpread = Float.MAX_VALUE
        for (which in SelectionEnd.entries) {
            val anchor = (if (which == SelectionEnd.Start) start else end) ?: continue
            val left = handleLeft(which, anchor.x, handleWidth)
            val right = left + handleWidth
            val dx = max(max(left - x, x - right), 0f)
            val dy = max(max(anchor.bottom - y, y - (anchor.bottom + handleHeight)), 0f)
            val distance = hypot(dx, dy)
            if (distance > slop) continue
            val spread = abs(x - (left + right) / 2)
            if (distance < bestDistance || (distance == bestDistance && spread <= bestSpread)) {
                best = which
                bestDistance = distance
                bestSpread = spread
            }
        }
        return best
    }

    /**
     * 拖柄纵向在 ([top], [bottom]) 之外时回滚历史每秒滚几行：在上面为正（往回看更早的内容），在下面为负，在里面为
     * 0；越出去越快，不超过 [MAX_EDGE_CELLS_PER_SECOND]。
     */
    fun autoscrollRate(y: Float, top: Float, bottom: Float, cellHeight: Float): Float =
        edgeRate(-y, -bottom, -top, cellHeight)

    /** 拖柄横向在 ([left], [right]) 之外时网格每秒平移几列：在右边为正（露出右边的列），在左边为负，在里面为 0。 */
    fun panRate(x: Float, left: Float, right: Float, cellWidth: Float): Float = edgeRate(x, left, right, cellWidth)

    private fun edgeRate(position: Float, low: Float, high: Float, cellSize: Float): Float {
        if (cellSize <= 0f) return 0f
        val overshoot = when {
            position < low -> position - low
            position > high -> position - high
            else -> return 0f
        } / cellSize
        val speed = min(MAX_EDGE_CELLS_PER_SECOND, MIN_EDGE_CELLS_PER_SECOND + abs(overshoot) * EDGE_CELLS_PER_SECOND_PER_CELL)
        return if (overshoot < 0) -speed else speed
    }

    /**
     * 选中的格子怎么上色：[foreground]、[background] 是格子本来的字色和底色（反显已经对调过）。没配选区颜色时底色按
     * 屏幕背景 [screenBackground] 的深浅取 [ON_DARK] 或 [ON_LIGHT]、字色不变；只配了底色时字色取格子本来的底色；
     * 配了的照配的取。
     */
    fun selectionColors(foreground: Rgb, background: Rgb, settings: TermSettings, screenBackground: Rgb): SelectionColors {
        val configuredBackground = settings.selectionBackground
        val configuredForeground = settings.selectionForeground
        val selectedBackground = configuredBackground?.let { resolve(it, foreground, background) }
            ?: defaultBackground(screenBackground)
        val selectedForeground = when {
            configuredForeground != null -> resolve(configuredForeground, foreground, background)
            configuredBackground != null -> background
            else -> foreground
        }
        return SelectionColors(selectedForeground, selectedBackground)
    }

    private fun defaultBackground(screenBackground: Rgb): Rgb {
        val luma = 0.299f * screenBackground.r.toInt() + 0.587f * screenBackground.g.toInt() +
            0.114f * screenBackground.b.toInt()
        return if (luma < 128f) ON_DARK else ON_LIGHT
    }

    private fun resolve(color: TerminalColor, foreground: Rgb, background: Rgb): Rgb = when (color) {
        is TerminalColor.Rgb -> color.color
        TerminalColor.CellForeground -> foreground
        TerminalColor.CellBackground -> background
    }
}
