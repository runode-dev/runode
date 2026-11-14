package dev.runode.mobile.terminal

import android.content.Context
import android.graphics.Paint
import android.graphics.Path
import android.graphics.Typeface
import android.graphics.fonts.Font
import android.graphics.fonts.FontFamily
import android.graphics.fonts.SystemFonts
import android.graphics.text.TextRunShaper
import android.icu.lang.UCharacter
import android.icu.lang.UProperty
import android.icu.text.BreakIterator
import kotlin.math.floor
import kotlin.math.max
import kotlin.math.min
import kotlin.math.sqrt

/**
 * 打包在 app 里的 Nerd Fonts「Symbols Only」等宽版（Symbols Nerd Font Mono，Nerd Fonts v3.5.1，字体本身 MIT 许可；
 * 各图标集的来源和许可写在随字体一起打包的 README 和 LICENSE 里）。zsh 提示符里的 Powerline、Git 分支这类私用区
 * 图标主字体里没有，画单元格时退到这里。
 */
object SymbolFont {
    /** 字体在 assets 里的路径。 */
    const val ASSET_PATH = "NerdFontsSymbolsOnly/SymbolsNerdFontMono-Regular.ttf"

    @Volatile
    private var loaded: Typeface? = null

    @Volatile
    private var missing = false

    /** 符号字体；资源缺了、读不出来时为空。第一次用时从 assets 读，之后整个进程共用。 */
    fun typeface(context: Context): Typeface? {
        loaded?.let { return it }
        if (missing) return null
        return synchronized(this) {
            loaded ?: runCatching { Typeface.createFromAsset(context.assets, ASSET_PATH) }.getOrNull()
                .also { if (it == null) missing = true else loaded = it }
        }
    }

    /** 私用区等码位：Powerline、Nerd Font 的图标都在这里，主字体缺字时退到符号字体。终端视图和会话列表的预览用同一个判断。 */
    fun isPrivateUse(codepoint: Int): Boolean =
        codepoint in 0xE000..0xF8FF || codepoint in 0xF0000..0xFFFFD || codepoint in 0x23FB..0x23FE ||
            codepoint == 0x2B58
}

/**
 * 默认按文字画、却也有 emoji 样子的字（Claude Code 的 `⏺`、转圈的 `✳`、`✔`、`⚠` 这些）。后面跟上 U+FE0E
 * （文字样式选择符），找后备字体时才不会挑中彩色 emoji 字体，画成不跟前景色的彩色方块。`❗` 这类默认就是 emoji
 * 的不算，照旧画 emoji。
 */
object TextPresentation {
    const val SELECTOR: Char = '︎'

    fun prefersText(codepoint: Int): Boolean =
        // ASCII 里的数字、`#`、`*` 也算 emoji 字符，排除掉。
        codepoint > 0x7F && UCharacter.hasBinaryProperty(codepoint, UProperty.EMOJI) &&
            !UCharacter.hasBinaryProperty(codepoint, UProperty.EMOJI_PRESENTATION)

    /** 给 [text] 里单码位的这类字后面补上文字样式选择符，交给自己找后备字体的地方。 */
    fun apply(text: String): String {
        val result = StringBuilder(text.length)
        val characters = BreakIterator.getCharacterInstance()
        characters.setText(text)
        var start = characters.first()
        var end = characters.next()
        while (end != BreakIterator.DONE) {
            result.append(text, start, end)
            val codepoint = text.codePointAt(start)
            if (Character.charCount(codepoint) == end - start && prefersText(codepoint)) {
                result.append(SELECTOR)
            }
            start = end
            end = characters.next()
        }
        return result.toString()
    }
}

/**
 * 系统里只有文字字形的符号字体（Noto Sans Symbols 系列）：Android 找后备字体时不认 U+FE0E，[TextPresentation] 的那类字
 * 先在这几个字体里找，有字形就用它画，不会落到彩色 emoji 字体上。系统里没有这类字体、或者都没有某个字时，照旧交给后备字体。
 */
object TextSymbolFont {
    /** 系统字体文件名的前缀。 */
    private const val PREFIX = "NotoSansSymbols"

    /** 找到的字体，按文件名排；第一次用时读系统字体表，之后整个进程共用。 */
    val fonts: List<Font> by lazy {
        runCatching {
            SystemFonts.getAvailableFonts()
                .filter { it.file?.name?.startsWith(PREFIX) == true }
                .distinctBy { it.file }
                .sortedBy { it.file?.name }
        }.getOrDefault(emptyList())
    }

    /** [fonts] 各自单独做成的字体，配给画字的 [Paint]，和 [fonts] 一一对应。 */
    val typefaces: List<Typeface> by lazy {
        fonts.map { Typeface.CustomFallbackBuilder(FontFamily.Builder(it).build()).build() }
    }

    /**
     * [codepoint] 该用 [paints] 里的第几个画：按 [fonts] 的顺序，排出来的字形确实出自那个字体的第一个；都不是时为空。
     * [paints] 按 [typefaces] 一一对应。自定义字体后面仍接着系统的后备字体，所以不用 `Paint.hasGlyph`，看排版实际取的字体。
     */
    fun pick(codepoint: Int, paints: List<Paint>): Int? {
        val text = String(Character.toChars(codepoint))
        return fonts.indices.firstOrNull { index ->
            val glyphs = TextRunShaper.shapeTextRun(text, 0, text.length, 0, text.length, 0f, 0f, false, paints[index])
            glyphs.glyphCount() > 0 && glyphs.getGlyphId(0) != 0 && glyphs.getFont(0).file == fonts[index].file
        }
    }
}

/** 字形的坐标系原点在左上、y 向下，单位和格子的一样。 */
internal data class GlyphPoint(val x: Float, val y: Float)

internal data class GlyphRect(val x: Float, val y: Float, val width: Float, val height: Float) {
    val minX: Float get() = x
    val minY: Float get() = y
    val maxX: Float get() = x + width
    val maxY: Float get() = y + height
    val midX: Float get() = x + width / 2
    val midY: Float get() = y + height / 2
    val isEmpty: Boolean get() = width <= 0f || height <= 0f

    fun offsetBy(dx: Float, dy: Float): GlyphRect = GlyphRect(x + dx, y + dy, width, height)
}

/** 不依赖平台的路径，画的一方用 [toAndroidPath] 转成 [Path]。没封口的子路径填色时按封口算。 */
internal class GlyphPath {
    sealed interface Segment

    data class MoveTo(val x: Float, val y: Float) : Segment

    data class LineTo(val x: Float, val y: Float) : Segment

    data class CubicTo(
        val x1: Float, val y1: Float, val x2: Float, val y2: Float, val x: Float, val y: Float,
    ) : Segment

    data object Close : Segment

    private val mutableSegments = mutableListOf<Segment>()
    val segments: List<Segment> get() = mutableSegments

    fun moveTo(x: Float, y: Float) = apply { mutableSegments += MoveTo(x, y) }

    fun lineTo(x: Float, y: Float) = apply { mutableSegments += LineTo(x, y) }

    fun cubicTo(x1: Float, y1: Float, x2: Float, y2: Float, x: Float, y: Float) =
        apply { mutableSegments += CubicTo(x1, y1, x2, y2, x, y) }

    fun close() = apply { mutableSegments += Close }

    /** 从第一个点起依次连到后面各点，不封口。 */
    fun addLines(vararg points: GlyphPoint) = apply {
        points.forEachIndexed { index, point -> if (index == 0) moveTo(point.x, point.y) else lineTo(point.x, point.y) }
    }

    fun addRect(rect: GlyphRect) = apply {
        moveTo(rect.minX, rect.minY).lineTo(rect.maxX, rect.minY).lineTo(rect.maxX, rect.maxY)
            .lineTo(rect.minX, rect.maxY).close()
    }

    /** 每个点都经 [transform] 换算后的一份。 */
    fun mapped(transform: (Float, Float) -> GlyphPoint): GlyphPath {
        val result = GlyphPath()
        for (segment in mutableSegments) {
            result.mutableSegments += when (segment) {
                is MoveTo -> transform(segment.x, segment.y).let { MoveTo(it.x, it.y) }
                is LineTo -> transform(segment.x, segment.y).let { LineTo(it.x, it.y) }
                is CubicTo -> {
                    val c1 = transform(segment.x1, segment.y1)
                    val c2 = transform(segment.x2, segment.y2)
                    val end = transform(segment.x, segment.y)
                    CubicTo(c1.x, c1.y, c2.x, c2.y, end.x, end.y)
                }
                Close -> Close
            }
        }
        return result
    }

    /** 路径经过的点围成的外框，不算曲线的控制点；没有点时为 `GlyphRect(0, 0, 0, 0)`。 */
    val bounds: GlyphRect
        get() {
            val points = polygons().flatten()
            if (points.isEmpty()) return GlyphRect(0f, 0f, 0f, 0f)
            val minX = points.minOf { it.x }
            val minY = points.minOf { it.y }
            return GlyphRect(minX, minY, points.maxOf { it.x } - minX, points.maxOf { it.y } - minY)
        }

    /** 按非零环绕规则，点在不在填色的区域里。 */
    fun contains(point: GlyphPoint): Boolean {
        var winding = 0
        for (polygon in polygons()) {
            for (i in polygon.indices) {
                val a = polygon[i]
                val b = polygon[(i + 1) % polygon.size]
                val side = (b.x - a.x) * (point.y - a.y) - (point.x - a.x) * (b.y - a.y)
                if (a.y <= point.y) {
                    if (b.y > point.y && side > 0) winding++
                } else if (b.y <= point.y && side < 0) {
                    winding--
                }
            }
        }
        return winding != 0
    }

    fun toAndroidPath(): Path {
        val path = Path()
        for (segment in mutableSegments) {
            when (segment) {
                is MoveTo -> path.moveTo(segment.x, segment.y)
                is LineTo -> path.lineTo(segment.x, segment.y)
                is CubicTo -> path.cubicTo(segment.x1, segment.y1, segment.x2, segment.y2, segment.x, segment.y)
                Close -> path.close()
            }
        }
        return path
    }

    /** 各个子路径折成的多边形，曲线按 [CURVE_STEPS] 段折线算。 */
    private fun polygons(): List<List<GlyphPoint>> {
        val polygons = mutableListOf<List<GlyphPoint>>()
        var current = mutableListOf<GlyphPoint>()
        fun finish() {
            if (current.isNotEmpty()) polygons += current
            current = mutableListOf()
        }
        for (segment in mutableSegments) {
            when (segment) {
                is MoveTo -> {
                    finish()
                    current += GlyphPoint(segment.x, segment.y)
                }
                is LineTo -> current += GlyphPoint(segment.x, segment.y)
                is CubicTo -> {
                    val start = current.lastOrNull() ?: GlyphPoint(0f, 0f)
                    for (step in 1..CURVE_STEPS) {
                        val t = step.toFloat() / CURVE_STEPS
                        val u = 1 - t
                        current += GlyphPoint(
                            u * u * u * start.x + 3 * u * u * t * segment.x1 + 3 * u * t * t * segment.x2 +
                                t * t * t * segment.x,
                            u * u * u * start.y + 3 * u * u * t * segment.y1 + 3 * u * t * t * segment.y2 +
                                t * t * t * segment.y,
                        )
                    }
                }
                Close -> {
                    val start = current.firstOrNull()
                    finish()
                    // 封口后接着画的从子路径起点开始。
                    if (start != null) current += start
                }
            }
        }
        finish()
        return polygons.filter { it.size > 1 }
    }

    private companion object {
        const val CURVE_STEPS = 16
    }
}

/** 字形怎么上色。 */
internal sealed class GlyphPaint {
    data object Fill : GlyphPaint()

    /** 沿路径描线，线宽 [width]，线头平齐、拐角圆滑。 */
    data class Stroke(val width: Float) : GlyphPaint()
}

internal data class GlyphShape(val path: GlyphPath, val paint: GlyphPaint)

/** 半圆用三次贝塞尔近似四分之一圆弧时控制点离端点的比例，和 Ghostty 的一样。 */
private val arcControl = (sqrt(2.0).toFloat() - 1) * 4 / 3

/** 把坐标按设备像素对齐：`scale` 是设备像素和格子单位的比例。 */
private fun snap(value: Float, scale: Float): Float = floor(value * scale + 0.5f) / scale

/**
 * Powerline 和 Powerline Extra 里几何形状的分隔符（U+E0B0–U+E0BF、U+E0D2、U+E0D4），照 Ghostty 的做法（它的
 * `drawE0B0` 这一组函数）不用字体，按单元格的矩形自己画：形状正好填满格子，和相邻格子的背景色之间不会有缝。
 */
internal object PowerlineGlyph {
    fun handles(codepoint: Int): Boolean = codepoint in 0xE0B0..0xE0BF || codepoint == 0xE0D2 || codepoint == 0xE0D4

    /** [codepoint] 在 [rect] 这个格子里的形状；不是这里画的字符返回空。[thickness] 是细线的线宽。 */
    fun path(codepoint: Int, rect: GlyphRect, thickness: Float): GlyphShape? {
        val w = rect.width
        val h = rect.height
        val path = GlyphPath()
        var paint: GlyphPaint = GlyphPaint.Fill
        var flip = false
        when (codepoint) {
            // 右指的实心三角
            0xE0B0 -> path.addLines(GlyphPoint(0f, 0f), GlyphPoint(w, h / 2), GlyphPoint(0f, h)).close()
            // 左指的实心三角
            0xE0B2 -> path.addLines(GlyphPoint(w, 0f), GlyphPoint(0f, h / 2), GlyphPoint(w, h)).close()
            // 右指、左指的细折线
            0xE0B1, 0xE0B3 -> {
                path.addLines(GlyphPoint(0f, 0f), GlyphPoint(w, h / 2), GlyphPoint(0f, h))
                paint = GlyphPaint.Stroke(thickness)
                flip = codepoint == 0xE0B3
            }
            // 右半圆、左半圆，实心
            0xE0B4, 0xE0B6 -> {
                addHalfCircle(path, w, h, closed = true)
                flip = codepoint == 0xE0B6
            }
            // 右半圆、左半圆，细线
            0xE0B5, 0xE0B7 -> {
                addHalfCircle(path, w, h, closed = false)
                paint = GlyphPaint.Stroke(thickness)
                flip = codepoint == 0xE0B7
            }
            // 左下三角
            0xE0B8 -> path.addLines(GlyphPoint(0f, 0f), GlyphPoint(w, h), GlyphPoint(0f, h)).close()
            // 右下三角
            0xE0BA -> path.addLines(GlyphPoint(w, 0f), GlyphPoint(w, h), GlyphPoint(0f, h)).close()
            // 左上三角
            0xE0BC -> path.addLines(GlyphPoint(0f, 0f), GlyphPoint(w, 0f), GlyphPoint(0f, h)).close()
            // 右上三角
            0xE0BE -> path.addLines(GlyphPoint(0f, 0f), GlyphPoint(w, 0f), GlyphPoint(w, h)).close()
            // 左上到右下的细斜线
            0xE0B9, 0xE0BF -> {
                path.addLines(GlyphPoint(0f, 0f), GlyphPoint(w, h))
                paint = GlyphPaint.Stroke(thickness)
            }
            // 右上到左下的细斜线
            0xE0BB, 0xE0BD -> {
                path.addLines(GlyphPoint(w, 0f), GlyphPoint(0f, h))
                paint = GlyphPaint.Stroke(thickness)
            }
            // 上下两块梯形夹一条缝
            0xE0D2, 0xE0D4 -> {
                path.addLines(
                    GlyphPoint(0f, 0f), GlyphPoint(w, 0f), GlyphPoint(w / 2, h / 2 - thickness / 2),
                    GlyphPoint(0f, h / 2 - thickness / 2),
                ).close()
                path.addLines(
                    GlyphPoint(0f, h), GlyphPoint(w, h), GlyphPoint(w / 2, h / 2 + thickness / 2),
                    GlyphPoint(0f, h / 2 + thickness / 2),
                ).close()
                flip = codepoint == 0xE0D4
            }
            else -> return null
        }
        val placed = path.mapped { x, y -> GlyphPoint(rect.minX + if (flip) w - x else x, rect.minY + y) }
        return GlyphShape(placed, paint)
    }

    /** 贴着左边的半圆（右半边是弧），半径取宽度和半高里小的那个，和 Ghostty 的一样用贝塞尔近似圆弧。 */
    private fun addHalfCircle(path: GlyphPath, w: Float, h: Float, closed: Boolean) {
        val c = arcControl
        val r = min(w, h / 2)
        path.moveTo(0f, 0f)
        path.cubicTo(r * c, 0f, r, r - r * c, r, r)
        path.lineTo(r, h - r)
        path.cubicTo(r, h - r + r * c, r * c, h, 0f, h)
        if (closed) path.close()
    }
}

/**
 * 制表符（U+2500–U+257F 里常用的那些：细线、粗线、直角和圆角、T 形、十字、半截线），照 Ghostty 的做法不用字体
 * 按格子自己画。字体里的制表符画不满格子，一排 `─` 会成虚线；这里每条线从格子中心画到边上，边按设备像素对齐，
 * 相邻格子算出同一条边界，接起来没有缝。
 */
internal object BoxDrawing {
    /** 一个字符四个方向上的线：0 没有，1 细，2 粗。 */
    private data class Arms(val up: Int, val right: Int, val down: Int, val left: Int)

    private val arms: Map<Int, Arms> = mapOf(
        0x2500 to Arms(0, 1, 0, 1), 0x2501 to Arms(0, 2, 0, 2),
        0x2502 to Arms(1, 0, 1, 0), 0x2503 to Arms(2, 0, 2, 0),
        0x250C to Arms(0, 1, 1, 0), 0x250F to Arms(0, 2, 2, 0),
        0x2510 to Arms(0, 0, 1, 1), 0x2513 to Arms(0, 0, 2, 2),
        0x2514 to Arms(1, 1, 0, 0), 0x2517 to Arms(2, 2, 0, 0),
        0x2518 to Arms(1, 0, 0, 1), 0x251B to Arms(2, 0, 0, 2),
        0x251C to Arms(1, 1, 1, 0), 0x2523 to Arms(2, 2, 2, 0),
        0x2524 to Arms(1, 0, 1, 1), 0x252B to Arms(2, 0, 2, 2),
        0x252C to Arms(0, 1, 1, 1), 0x2533 to Arms(0, 2, 2, 2),
        0x2534 to Arms(1, 1, 0, 1), 0x253B to Arms(2, 2, 0, 2),
        0x253C to Arms(1, 1, 1, 1), 0x254B to Arms(2, 2, 2, 2),
        0x2574 to Arms(0, 0, 0, 1), 0x2575 to Arms(1, 0, 0, 0),
        0x2576 to Arms(0, 1, 0, 0), 0x2577 to Arms(0, 0, 1, 0),
        0x2578 to Arms(0, 0, 0, 2), 0x2579 to Arms(2, 0, 0, 0),
        0x257A to Arms(0, 2, 0, 0), 0x257B to Arms(0, 0, 2, 0),
    )

    fun handles(codepoint: Int): Boolean = codepoint in arms || codepoint in 0x256D..0x2570

    /**
     * [codepoint] 在 [rect] 这个格子里的形状；不是这里画的字符返回空。[thickness] 是细线的粗细，[scale] 是设备像素
     * 和格子单位的比例，边按它对齐。直线是填色的矩形，圆角是描成细线的圆弧。
     */
    fun path(codepoint: Int, rect: GlyphRect, thickness: Float, scale: Float): GlyphShape? {
        val light = max(snap(thickness, scale), 1 / scale)
        val left = snap(rect.minX, scale)
        val right = snap(rect.maxX, scale)
        val top = snap(rect.minY, scale)
        val bottom = snap(rect.maxY, scale)

        /** 中线两边对称的一条带子：粗细 `width`，起止对齐像素。 */
        fun band(center: Float, width: Float): Pair<Float, Float> {
            val start = snap(center - width / 2, scale)
            return start to start + max(snap(width, scale), 1 / scale)
        }

        val lines = arms[codepoint]
        if (lines != null) {
            val path = GlyphPath()
            val widest = maxOf(lines.up, lines.down, lines.left, lines.right) * light
            val (vx0, _) = band(rect.midX, widest)
            val (hy0, _) = band(rect.midY, widest)

            // 横线、竖线各自伸到中心那块的另一边，拐角和十字处补满。
            fun horizontal(weight: Int, x0: Float, x1: Float) {
                if (weight <= 0) return
                val (y0, y1) = band(rect.midY, weight * light)
                path.addRect(GlyphRect(x0, y0, x1 - x0, y1 - y0))
            }

            fun vertical(weight: Int, y0: Float, y1: Float) {
                if (weight <= 0) return
                val (x0, x1) = band(rect.midX, weight * light)
                path.addRect(GlyphRect(x0, y0, x1 - x0, y1 - y0))
            }
            val joinRight = vx0 + max(snap(widest, scale), 1 / scale)
            val joinBottom = hy0 + max(snap(widest, scale), 1 / scale)
            horizontal(lines.left, left, if (lines.right > 0) right else joinRight)
            if (lines.right != lines.left) horizontal(lines.right, vx0, right)
            vertical(lines.up, top, if (lines.down > 0) bottom else joinBottom)
            if (lines.down != lines.up) vertical(lines.down, hy0, bottom)
            return GlyphShape(path, GlyphPaint.Fill)
        }
        if (codepoint !in 0x256D..0x2570) return null
        // 圆角：从一条边的中点画四分之一圆弧到另一条边的中点。╭ ╮ 向下，╯ ╰ 向上；╭ ╰ 向右，╮ ╯ 向左。
        val centerX = (left + right) / 2
        val centerY = (top + bottom) / 2
        val radius = min(rect.width, rect.height) / 2
        val down = codepoint <= 0x256E
        val toRight = codepoint == 0x256D || codepoint == 0x2570
        val side = if (toRight) right else left
        val ySign = if (down) 1f else -1f
        val xSign = if (toRight) 1f else -1f
        val arcStartY = centerY + ySign * radius
        val arcEndX = centerX + xSign * radius
        val path = GlyphPath()
            .moveTo(centerX, if (down) bottom else top)
            .lineTo(centerX, arcStartY)
            .cubicTo(
                centerX, arcStartY - ySign * radius * arcControl,
                arcEndX - xSign * radius * arcControl, centerY,
                arcEndX, centerY,
            )
            .lineTo(side, centerY)
        return GlyphShape(path, GlyphPaint.Stroke(light))
    }
}

/**
 * 块元素（U+2580–U+259F：半块、八分之几的块、整块、阴影 `░▒▓`、象限块），和桌面一样不用字体按格子自己画：块正好
 * 铺满格子，阴影用前景色加 1/4、1/2、3/4 的不透明度铺满，不画字体里的点阵，相邻格子接起来没有花纹和缝。
 */
internal object BlockElement {
    /** 象限块 U+2596–U+259F 的组成：1 左上、2 右上、4 左下、8 右下。 */
    private val quadrants = intArrayOf(4, 8, 1, 13, 9, 7, 11, 2, 6, 14)

    data class Shape(val path: GlyphPath, val alpha: Float)

    fun handles(codepoint: Int): Boolean = codepoint in 0x2580..0x259F

    /**
     * [codepoint] 在 [rect] 这个格子里要填的形状和填色的不透明度；不是这里画的字符返回空。边按 [scale]（设备像素和
     * 格子单位的比例）对齐。
     */
    fun shape(codepoint: Int, rect: GlyphRect, scale: Float): Shape? {
        /** 格子里按比例取的一块，x、y 都在 0..1。 */
        fun part(x0: Float, x1: Float, y0: Float, y1: Float): GlyphRect {
            val left = snap(rect.minX + rect.width * x0, scale)
            val right = snap(rect.minX + rect.width * x1, scale)
            val top = snap(rect.minY + rect.height * y0, scale)
            val bottom = snap(rect.minY + rect.height * y1, scale)
            return GlyphRect(left, top, right - left, bottom - top)
        }
        var alpha = 1f
        val rects: List<GlyphRect> = when (codepoint) {
            0x2580 -> listOf(part(0f, 1f, 0f, 0.5f))
            // ▁▂▃▄▅▆▇█：下方 1/8 到整格。
            in 0x2581..0x2588 -> listOf(part(0f, 1f, 1 - (codepoint - 0x2580) / 8f, 1f))
            // ▉▊▋▌▍▎▏：左侧 7/8 到 1/8。
            in 0x2589..0x258F -> listOf(part(0f, (0x2590 - codepoint) / 8f, 0f, 1f))
            0x2590 -> listOf(part(0.5f, 1f, 0f, 1f))
            in 0x2591..0x2593 -> {
                alpha = (codepoint - 0x2590) / 4f
                listOf(part(0f, 1f, 0f, 1f))
            }
            0x2594 -> listOf(part(0f, 1f, 0f, 0.125f))
            0x2595 -> listOf(part(0.875f, 1f, 0f, 1f))
            in 0x2596..0x259F -> {
                val mask = quadrants[codepoint - 0x2596]
                (0 until 4).filter { mask shr it and 1 != 0 }.map { index ->
                    val x = (index % 2) / 2f
                    val y = (index / 2) / 2f
                    part(x, x + 0.5f, y, y + 0.5f)
                }
            }
            else -> return null
        }
        val path = GlyphPath()
        rects.forEach(path::addRect)
        return Shape(path, alpha)
    }
}

/**
 * 系统字体里只有彩色 emoji 字形、没有文字字形的几何符号，不用字体按格子自己画，跟前景色。`⏺`（U+23FA）是 Claude Code
 * 每段输出前的圆点，画成圆心在格子中心的实心圆。
 */
internal object GeometricSymbol {
    private const val RECORD = 0x23FA

    /** 圆的直径占格宽的比例，留一点边，和相邻的字不粘在一起。 */
    private const val DIAMETER = 0.8f

    fun handles(codepoint: Int): Boolean = codepoint == RECORD

    /** [codepoint] 在 [rect] 这个格子里的形状；不是这里画的字符返回空。 */
    fun path(codepoint: Int, rect: GlyphRect): GlyphShape? {
        if (codepoint != RECORD) return null
        val r = min(rect.width, rect.height) * DIAMETER / 2
        val cx = rect.midX
        val cy = rect.midY
        val k = r * arcControl
        val path = GlyphPath()
            .moveTo(cx + r, cy)
            .cubicTo(cx + r, cy + k, cx + k, cy + r, cx, cy + r)
            .cubicTo(cx - k, cy + r, cx - r, cy + k, cx - r, cy)
            .cubicTo(cx - r, cy - k, cx - k, cy - r, cx, cy - r)
            .cubicTo(cx + k, cy - r, cx + r, cy - k, cx + r, cy)
            .close()
        return GlyphShape(path, GlyphPaint.Fill)
    }
}
