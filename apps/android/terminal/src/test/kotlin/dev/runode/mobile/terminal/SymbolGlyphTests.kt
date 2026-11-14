package dev.runode.mobile.terminal

import kotlin.math.abs
import kotlin.math.floor
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import org.junit.Test

private fun near(a: Float, b: Float, tolerance: Float = 0.001f) = abs(a - b) < tolerance

private fun snapped(value: Float, scale: Float) = floor(value * scale + 0.5f) / scale

class PowerlineGlyphTests {
    /** 实心的分隔符正好填满格子：外框就是格子的矩形，相邻格子之间没有空隙。 */
    @Test
    fun solidSeparatorsFillTheCell() {
        val rect = GlyphRect(15.5f, 32f, 7.8f, 16f)
        for (codepoint in listOf(0xE0B0, 0xE0B2, 0xE0B4, 0xE0B6, 0xE0B8, 0xE0BA, 0xE0BC, 0xE0BE)) {
            val name = "U+${codepoint.toString(16)}"
            val shape = assertNotNull(PowerlineGlyph.path(codepoint, rect, 1f), name)
            assertEquals(GlyphPaint.Fill, shape.paint, name)
            val box = shape.path.bounds
            // 半圆的半径取宽度和半高里小的那个，宽度用满。
            assertTrue(near(box.minX, rect.minX) && near(box.maxX, rect.maxX), "$name $box")
            assertTrue(near(box.minY, rect.minY) && near(box.maxY, rect.maxY), "$name $box")
        }
    }

    @Test
    fun rightTriangleTouchesTheLeftEdgeTopToBottom() {
        val rect = GlyphRect(0f, 0f, 8f, 16f)
        val path = assertNotNull(PowerlineGlyph.path(0xE0B0, rect, 1f)).path
        assertTrue(path.contains(GlyphPoint(0.1f, 0.5f)))
        assertTrue(path.contains(GlyphPoint(0.1f, 15.5f)))
        assertTrue(path.contains(GlyphPoint(7.5f, 8f)))
        assertFalse(path.contains(GlyphPoint(7.5f, 1f)))
        // 左指的是镜像。
        val left = assertNotNull(PowerlineGlyph.path(0xE0B2, rect, 1f)).path
        assertTrue(left.contains(GlyphPoint(7.9f, 0.5f)))
        assertFalse(left.contains(GlyphPoint(0.5f, 1f)))
    }

    @Test
    fun thinSeparatorsAreStroked() {
        val rect = GlyphRect(0f, 0f, 8f, 16f)
        for (codepoint in listOf(0xE0B1, 0xE0B3, 0xE0B5, 0xE0B7, 0xE0B9, 0xE0BB, 0xE0BD, 0xE0BF)) {
            val shape = assertNotNull(PowerlineGlyph.path(codepoint, rect, 1.5f))
            val paint = assertIs<GlyphPaint.Stroke>(shape.paint, "U+${codepoint.toString(16)}")
            assertEquals(1.5f, paint.width)
        }
    }

    @Test
    fun otherCharactersAreLeftToFonts() {
        assertFalse(PowerlineGlyph.handles(0xE0A0))
        assertFalse(PowerlineGlyph.handles('A'.code))
        assertNull(PowerlineGlyph.path(0xF113, GlyphRect(0f, 0f, 8f, 16f), 1f))
    }
}

class BoxDrawingTests {
    private val rect = GlyphRect(7.83f, 16f, 7.83f, 16f)

    /** `─` 从格子左边画到右边，两头对齐设备像素：相邻两格算出同一条边，一排接起来没有缝。 */
    @Test
    fun horizontalLinesSpanTheCellOnPixelEdges() {
        val scale = 3f
        val shape = assertNotNull(BoxDrawing.path(0x2500, rect, 1f, scale))
        assertEquals(GlyphPaint.Fill, shape.paint)
        val box = shape.path.bounds
        assertTrue(near(box.minX, snapped(rect.minX, scale), 0.0001f))
        assertTrue(near(box.maxX, snapped(rect.maxX, scale), 0.0001f))
        val next = assertNotNull(BoxDrawing.path(0x2500, rect.offsetBy(rect.width, 0f), 1f, scale))
        assertTrue(near(next.path.bounds.minX, box.maxX, 0.0001f))
        assertTrue(box.height >= 1 / scale)
        assertTrue(box.midY > rect.minY && box.midY < rect.maxY)
    }

    @Test
    fun cornersReachTheirTwoEdges() {
        val box = assertNotNull(BoxDrawing.path(0x250C, rect, 1f, 2f)).path.bounds
        // ┌ 往右、往下：碰到右边和下边，碰不到左边和上边。
        assertTrue(near(box.maxX, snapped(rect.maxX, 2f), 0.0001f))
        assertTrue(near(box.maxY, snapped(rect.maxY, 2f), 0.0001f))
        assertTrue(box.minX > rect.minX + 1)
        assertTrue(box.minY > rect.minY + 1)
    }

    @Test
    fun heavyLinesAreThicker() {
        val light = assertNotNull(BoxDrawing.path(0x2500, rect, 1f, 3f)).path.bounds
        val heavy = assertNotNull(BoxDrawing.path(0x2501, rect, 1f, 3f)).path.bounds
        assertTrue(heavy.height > light.height)
    }

    /** 圆角描成细线，从一条边的中点弯到另一条边的中点。 */
    @Test
    fun roundedCornersAndUnknowns() {
        for (codepoint in listOf(0x256D, 0x256E, 0x256F, 0x2570)) {
            val shape = assertNotNull(BoxDrawing.path(codepoint, rect, 1f, 3f))
            assertIs<GlyphPaint.Stroke>(shape.paint)
            assertFalse(shape.path.bounds.isEmpty)
        }
        // ╭ 从下边的中点出发，到右边的中点。
        val box = assertNotNull(BoxDrawing.path(0x256D, rect, 1f, 3f)).path.bounds
        assertTrue(near(box.maxY, snapped(rect.maxY, 3f)))
        assertTrue(near(box.maxX, snapped(rect.maxX, 3f)))
        assertFalse(BoxDrawing.handles('A'.code))
        assertNull(BoxDrawing.path(0x2550, rect, 1f, 3f))
    }
}

class SymbolFallbackTests {
    /** 预览和终端视图用同一个判断决定哪些字退到符号字体：私用区（含辅助平面的）和几个电源符号，中文、emoji 照旧交给系统的后备字体。 */
    @Test
    fun privateUseDetection() {
        for (codepoint in listOf(0xE0A0, 0xE0B0, 0xF113, 0xF0001, 0x23FB)) {
            assertTrue(SymbolFont.isPrivateUse(codepoint), "U+${codepoint.toString(16)}")
        }
        for (codepoint in listOf('A'.code, '中'.code, 0x2705, 0x2500)) {
            assertFalse(SymbolFont.isPrivateUse(codepoint), "U+${codepoint.toString(16)}")
        }
    }

    /** 阴影块铺满格子、按深浅取不透明度；半块和象限块按像素对齐地占格子的一部分。 */
    @Test
    fun blockElementsFillTheirShareOfTheCell() {
        val rect = GlyphRect(7.8f, 16f, 7.8f, 16f)
        for ((codepoint, alpha) in listOf(0x2591 to 0.25f, 0x2592 to 0.5f, 0x2593 to 0.75f)) {
            val shade = assertNotNull(BlockElement.shape(codepoint, rect, 2f))
            assertEquals(alpha, shade.alpha)
            assertEquals(GlyphRect(8f, 16f, 7.5f, 16f), shade.path.bounds)
        }
        val lower = assertNotNull(BlockElement.shape(0x2584, rect, 2f))
        assertEquals(GlyphRect(8f, 24f, 7.5f, 8f), lower.path.bounds)
        // ▚：左上和右下。
        val quadrant = assertNotNull(BlockElement.shape(0x259A, rect, 2f)).path
        assertTrue(quadrant.contains(GlyphPoint(9f, 17f)) && quadrant.contains(GlyphPoint(15f, 31f)))
        assertFalse(quadrant.contains(GlyphPoint(15f, 17f)) || quadrant.contains(GlyphPoint(9f, 31f)))
        assertNull(BlockElement.shape(0x25A0, rect, 2f))
    }

    /** `⏺` 是圆心在格子中心的实心圆，格子的四角不填；别的字不归它画。 */
    @Test
    fun recordSymbolIsACenteredDisc() {
        val rect = GlyphRect(16f, 32f, 8f, 16f)
        val shape = assertNotNull(GeometricSymbol.path(0x23FA, rect))
        assertEquals(GlyphPaint.Fill, shape.paint)
        val box = shape.path.bounds
        assertTrue(near(box.midX, rect.midX) && near(box.midY, rect.midY), "$box")
        assertTrue(near(box.width, box.height) && box.width <= rect.width && box.width > rect.width / 2, "$box")
        assertTrue(shape.path.contains(GlyphPoint(rect.midX, rect.midY)))
        assertFalse(shape.path.contains(GlyphPoint(rect.minX + 0.5f, rect.minY + 0.5f)))
        assertFalse(GeometricSymbol.handles(0x23FB))
        assertNull(GeometricSymbol.path('A'.code, rect))
    }
}
