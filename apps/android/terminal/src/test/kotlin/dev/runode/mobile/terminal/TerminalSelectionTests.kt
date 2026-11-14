package dev.runode.mobile.terminal

import dev.runode.mobile.protocol.Rgb
import dev.runode.mobile.protocol.TermSettings
import dev.runode.mobile.protocol.TerminalColor
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue
import org.junit.Test

/** 选区的拖柄落点、命中、自动滚动的速度和配色，不经 View 和 VT。 */
class TerminalSelectionTests {
    private val cellWidth = 10f
    private val cellHeight = 20f

    private fun cell(end: SelectionEnd, x: Float, y: Float = 30f): GridCell =
        SelectionGeometry.handleCell(end, x, y, cellWidth, cellHeight, columns = 8, rows = 4)

    /** 手指按四舍五入落到格子边界上：起点取边界右边那格，终点取左边那格。 */
    @Test
    fun handlesRoundToTheNearestCellBoundary() {
        assertEquals(GridCell(1, 1), cell(SelectionEnd.Start, 14f))
        assertEquals(GridCell(2, 1), cell(SelectionEnd.Start, 16f))
        assertEquals(GridCell(0, 1), cell(SelectionEnd.End, 14f))
        assertEquals(GridCell(1, 1), cell(SelectionEnd.End, 16f))
    }

    @Test
    fun handleCellsAreClampedToTheGrid() {
        assertEquals(GridCell(7, 0), cell(SelectionEnd.Start, 200f, -5f))
        assertEquals(GridCell(0, 3), cell(SelectionEnd.End, -30f, 999f))
        assertEquals(GridCell(7, 3), cell(SelectionEnd.End, 200f, 999f))
    }

    /** 起点落在宽字符后一格时退到前一格的左边缘；终点落在宽字符头一格时贴着两格之后的右边缘。 */
    @Test
    fun anchorsCoverWideCharacters() {
        val start = SelectionGeometry.handleAnchor(SelectionEnd.Start, GridCell(3, 1), CellWidth.SpacerTail, cellWidth, cellHeight)
        assertEquals(HandleAnchor(20f, 20f, 40f), start)
        val narrowStart = SelectionGeometry.handleAnchor(SelectionEnd.Start, GridCell(3, 1), CellWidth.Narrow, cellWidth, cellHeight)
        assertEquals(30f, narrowStart.x)
        val wideEnd = SelectionGeometry.handleAnchor(SelectionEnd.End, GridCell(2, 0), CellWidth.Wide, cellWidth, cellHeight)
        assertEquals(HandleAnchor(40f, 0f, 20f), wideEnd)
        val narrowEnd = SelectionGeometry.handleAnchor(SelectionEnd.End, GridCell(2, 0), null, cellWidth, cellHeight)
        assertEquals(30f, narrowEnd.x)
    }

    /** 拖柄图的尖对着格边：起点柄往左伸，终点柄往右伸。 */
    @Test
    fun handleImagesHangOnTheOuterSide() {
        assertEquals(15f, SelectionGeometry.handleLeft(SelectionEnd.Start, 30f, 20f))
        assertEquals(25f, SelectionGeometry.handleLeft(SelectionEnd.End, 30f, 20f))
    }

    /** 两个拖柄叠在一起时按手指偏向哪边分；离得太远哪个都不算；看不见的那端不参与。 */
    @Test
    fun overlappingHandlesAreToldApartBySide() {
        val anchor = HandleAnchor(30f, 20f, 40f)
        fun hit(x: Float, y: Float, start: HandleAnchor? = anchor, end: HandleAnchor? = anchor) =
            SelectionGeometry.hitHandle(x, y, start, end, handleWidth = 20f, handleHeight = 20f, slop = 8f)
        assertEquals(SelectionEnd.Start, hit(28f, 50f))
        assertEquals(SelectionEnd.End, hit(33f, 50f))
        assertEquals(SelectionEnd.End, hit(30f, 50f))
        assertNull(hit(100f, 100f))
        assertEquals(SelectionEnd.Start, hit(36f, 50f, end = null))
        assertEquals(SelectionEnd.End, hit(20f, 50f, start = null))
        // 拖柄图下面、行里的字上超过容差就不算了。
        assertNull(hit(30f, 70f))
        assertNull(hit(30f, 25f))
    }

    /** 在可见区域里不滚；往上越出往回看更早的内容（正），往下越出为负；越出去越快，有上限。 */
    @Test
    fun autoscrollSpeedsUpOutsideAndIsCapped() {
        assertEquals(0f, SelectionGeometry.autoscrollRate(50f, 0f, 100f, cellHeight))
        val justAbove = SelectionGeometry.autoscrollRate(-1f, 0f, 100f, cellHeight)
        val farAbove = SelectionGeometry.autoscrollRate(-40f, 0f, 100f, cellHeight)
        assertTrue(justAbove > 0f)
        assertTrue(farAbove > justAbove)
        assertTrue(SelectionGeometry.autoscrollRate(101f, 0f, 100f, cellHeight) < 0f)
        assertEquals(SelectionGeometry.MAX_EDGE_CELLS_PER_SECOND, SelectionGeometry.autoscrollRate(-10_000f, 0f, 100f, cellHeight))
        assertEquals(-SelectionGeometry.MAX_EDGE_CELLS_PER_SECOND, SelectionGeometry.autoscrollRate(10_000f, 0f, 100f, cellHeight))
    }

    /** 拖到右边平移出右边的列（正），拖到左边为负。 */
    @Test
    fun panRateFollowsTheSide() {
        assertEquals(0f, SelectionGeometry.panRate(50f, 10f, 90f, cellWidth))
        assertTrue(SelectionGeometry.panRate(95f, 10f, 90f, cellWidth) > 0f)
        assertTrue(SelectionGeometry.panRate(5f, 10f, 90f, cellWidth) < 0f)
    }

    private val text = Rgb(0xEEEEEEu)
    private val cellBackground = Rgb(0x112233u)
    private val dark = Rgb(0x171618u)
    private val light = Rgb(0xFAFAFAu)

    private fun colors(background: TerminalColor? = null, foreground: TerminalColor? = null, screen: Rgb = dark) =
        SelectionGeometry.selectionColors(
            text, cellBackground,
            TermSettings.default.copy(selectionBackground = background, selectionForeground = foreground), screen,
        )

    /** 没配颜色：底色按屏幕背景的深浅取，字色不变。 */
    @Test
    fun defaultSelectionColorDependsOnTheScreenBackground() {
        assertEquals(SelectionColors(text, SelectionGeometry.ON_DARK), colors())
        assertEquals(SelectionColors(text, SelectionGeometry.ON_LIGHT), colors(screen = light))
    }

    /** 只配了底色：字色取格子本来的底色。 */
    @Test
    fun configuredBackgroundTakesTheCellBackgroundAsText() {
        val red = Rgb(0xFF0000u)
        assertEquals(SelectionColors(cellBackground, red), colors(background = TerminalColor.Rgb(red)))
    }

    /** 两样都配了照配的取，跟随格子的颜色按格子本来的解析。 */
    @Test
    fun configuredColorsResolveAgainstTheCell() {
        assertEquals(
            SelectionColors(cellBackground, text),
            colors(background = TerminalColor.CellForeground, foreground = TerminalColor.CellBackground),
        )
        val green = Rgb(0x00FF00u)
        assertEquals(SelectionColors(green, SelectionGeometry.ON_DARK), colors(foreground = TerminalColor.Rgb(green)))
    }
}
