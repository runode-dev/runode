package dev.runode.mobile.terminal

import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Path
import android.graphics.Rect
import android.graphics.RectF
import android.graphics.Typeface
import dev.runode.mobile.protocol.Rgb
import dev.runode.mobile.protocol.TermSettings
import dev.runode.mobile.protocol.TerminalColor
import kotlin.math.abs
import kotlin.math.ceil
import kotlin.math.floor
import kotlin.math.max
import kotlin.math.min

/** 终端用的等宽字体和由它算出的单元格尺寸，单位都是像素。 */
internal class TerminalFont(val size: Float, symbolTypeface: Typeface?) {
    private val regular = textPaint(Typeface.MONOSPACE)
    private val bold = textPaint(Typeface.create(Typeface.MONOSPACE, Typeface.BOLD))
    private val italic = textPaint(Typeface.create(Typeface.MONOSPACE, Typeface.ITALIC))
    private val boldItalic = textPaint(Typeface.create(Typeface.MONOSPACE, Typeface.BOLD_ITALIC))

    /** 主字体里没有的私用区图标（Powerline、Nerd Font）退到的符号字体。 */
    val symbols: Paint? = symbolTypeface?.let(::textPaint)

    /** 按文字画的符号（[TextPresentation]）用的只有文字字形的字体，按 [TextSymbolFont.typefaces] 的顺序。 */
    val textSymbols: List<Paint> = TextSymbolFont.typefaces.map(::textPaint)

    val cellWidth: Float = regular.measureText("M")
    val ascent: Float
    val cellHeight: Float

    /** 四种样式各自的字宽是不是正好 [cellWidth]：是的话 ASCII 能连成一段一次画，字不会和格子错开。 */
    private val uniform: BooleanArray

    init {
        val metrics = regular.fontMetrics
        ascent = -metrics.ascent
        cellHeight = ceil(metrics.descent - metrics.ascent + metrics.leading)
        uniform = BooleanArray(4) { abs(paint(it).measureText("M") - cellWidth) < 0.01f }
    }

    /** 细线（下划线、删除线、Powerline 的细折线）的粗细。 */
    val thickness: Float get() = max(1f, size / 14)

    fun paint(bold: Boolean, italic: Boolean): Paint = paint(style(bold, italic))

    fun isUniform(bold: Boolean, italic: Boolean): Boolean = uniform[style(bold, italic)]

    private fun style(bold: Boolean, italic: Boolean): Int = (if (bold) 1 else 0) or (if (italic) 2 else 0)

    private fun paint(style: Int): Paint = when (style) {
        1 -> bold
        2 -> italic
        3 -> boldItalic
        else -> regular
    }

    private fun textPaint(typeface: Typeface): Paint = Paint(Paint.ANTI_ALIAS_FLAG or Paint.SUBPIXEL_TEXT_FLAG).apply {
        this.typeface = typeface
        textSize = size
    }
}

/**
 * 在 Canvas 上画 [ScreenFrame] 的一行：背景、三层图片、文字、下划线和删除线；另画光标和组字中的文字。坐标以这一行
 * 的左上角为原点，单位是像素。字体、设置和图片缓存都在这里，[TerminalView] 每行录一份显示列表时调它。
 */
internal class TerminalRenderer(var font: TerminalFont) {
    var settings: TermSettings = TermSettings.default

    /** 按 [ScreenImage] 的 id 和版本取图片的像素，由持有 VT 的一方给（[VTerminal.image]）。 */
    var imageSource: ((UInt, ULong) -> Bitmap?)? = null

    /** 画过的图片，按版本存，[pruneImages] 丢掉帧里没有了的。 */
    private val imageCache = HashMap<ULong, Bitmap>()

    /** 非 ASCII 字的前进宽度，按样式和文字缓存；换字体时清空。 */
    private val advanceCache = HashMap<String, Float>()
    private var advanceFont: TerminalFont? = null

    /** 符号字体里有没有这个码位的字形。 */
    private val symbolGlyphs = HashMap<Int, Boolean>()

    /** 按文字画的码位用 [TerminalFont.textSymbols] 里的第几个，都没有时是 -1；和字号无关，换字体也不清。 */
    private val textSymbolChoice = HashMap<Int, Int>()

    private val fill = Paint().apply { style = Paint.Style.FILL }
    private val stroke = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeCap = Paint.Cap.BUTT
        strokeJoin = Paint.Join.ROUND
    }
    private val imagePaint = Paint(Paint.FILTER_BITMAP_FLAG)
    private val symbolPaint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val run = StringBuilder()
    private val scratchRect = Rect()
    private val scratchRectF = RectF()
    private val wave = Path()

    /** 网格的像素宽度。 */
    fun gridWidth(frame: ScreenFrame): Float = frame.columns * font.cellWidth

    /**
     * 画一行。[row] 是这一行在视口里的行号，算图片的位置用；视口上面那一行传 -1。只画在 `(0, 0)` 到
     * `(网格宽, 行高)` 之内。
     */
    fun drawRow(canvas: Canvas, frame: ScreenFrame, cells: Array<ScreenCell>, row: Int) {
        val cellHeight = font.cellHeight
        val width = gridWidth(frame)
        canvas.save()
        canvas.clipRect(0f, 0f, width, cellHeight)
        fill.color = argb(frame.background)
        canvas.drawRect(0f, 0f, width, cellHeight, fill)
        drawImages(canvas, frame, row, ImageLayer.BelowBackground)
        drawBackgrounds(canvas, frame, cells)
        drawImages(canvas, frame, row, ImageLayer.BelowText)
        drawText(canvas, frame, cells)
        drawImages(canvas, frame, row, ImageLayer.AboveText)
        canvas.restore()
    }

    /** 背景按同色的一段一起填：格子宽度不是整像素，一格一格填时同色的相邻格子之间会露出细缝。 */
    private fun drawBackgrounds(canvas: Canvas, frame: ScreenFrame, cells: Array<ScreenCell>) {
        var start = -1
        var color = 0
        val cellWidth = font.cellWidth
        fun flush(end: Int) {
            if (start < 0) return
            fill.color = color
            canvas.drawRect(start * cellWidth, 0f, end * cellWidth, font.cellHeight, fill)
            start = -1
        }
        val defaultBackground = argb(frame.background)
        for (column in cells.indices) {
            val cell = cells[column]
            // 宽字符的后一格跟着前一格的背景走。
            val source = if (cell.width == CellWidth.SpacerTail && column > 0) cells[column - 1] else cell
            val background = argb(background(frame, source))
            if (start >= 0 && background == color) continue
            flush(column)
            if (background != defaultBackground) {
                start = column
                color = background
            }
        }
        flush(cells.size)
    }

    private fun drawText(canvas: Canvas, frame: ScreenFrame, cells: Array<ScreenCell>) {
        val cellWidth = font.cellWidth
        val baseline = font.ascent
        var runStart = -1
        var runPaint: Paint? = null
        var runColor = 0
        fun flush() {
            val paint = runPaint
            if (runStart >= 0 && paint != null) {
                val text = run.trimEnd(' ')
                if (text.isNotEmpty()) {
                    paint.color = runColor
                    canvas.drawText(text, 0, text.length, runStart * cellWidth, baseline, paint)
                }
            }
            run.setLength(0)
            runStart = -1
            runPaint = null
        }
        for (column in cells.indices) {
            val cell = cells[column]
            if (cell.width == CellWidth.SpacerTail || cell.width == CellWidth.SpacerHead) continue
            val x = column * cellWidth
            val width = if (cell.width == CellWidth.Wide) cellWidth * 2 else cellWidth
            val ink = ink(frame, cell)
            val bold = CellAttributes.bold in cell.attributes
            val italic = CellAttributes.italic in cell.attributes
            if (!hasText(cell)) {
                // 空格接在一段 ASCII 后面，免得一行字被空格切成很多段。
                if (runStart >= 0) run.append(' ')
            } else if (isAscii(cell) && font.isUniform(bold, italic)) {
                val paint = font.paint(bold, italic)
                if (runStart < 0 || paint !== runPaint || ink != runColor) {
                    flush()
                    runStart = column
                    runPaint = paint
                    runColor = ink
                }
                run.append(cell.text)
            } else {
                flush()
                drawCell(canvas, cells, column, x, 0f, width, ink, bold, italic)
            }
            if (cell.underline != UnderlineStyle.None) drawUnderline(canvas, cell.underline, x, width, ink)
            if (CellAttributes.strikethrough in cell.attributes) {
                fill.color = ink
                val y = font.cellHeight / 2
                canvas.drawRect(x, y, x + width, y + font.thickness, fill)
            }
        }
        flush()
    }

    private fun isAscii(cell: ScreenCell): Boolean =
        cell.width == CellWidth.Narrow && cell.text.length == 1 && cell.text[0].code in 0x21..0x7E

    private fun drawUnderline(canvas: Canvas, style: UnderlineStyle, x: Float, width: Float, ink: Int) {
        val thickness = font.thickness
        val y = font.ascent + max(1f, font.size * 0.12f)
        fill.color = ink
        when (style) {
            UnderlineStyle.None -> Unit
            UnderlineStyle.Single -> canvas.drawRect(x, y, x + width, y + thickness, fill)
            UnderlineStyle.Double -> {
                canvas.drawRect(x, y, x + width, y + thickness, fill)
                canvas.drawRect(x, y + thickness * 2, x + width, y + thickness * 3, fill)
            }
            UnderlineStyle.Curly -> {
                // 一格一个整波，相邻格子接得上。
                val amplitude = thickness * 1.5f
                wave.rewind()
                wave.moveTo(x, y)
                wave.cubicTo(x + width / 4, y - amplitude, x + width / 4, y - amplitude, x + width / 2, y)
                wave.cubicTo(x + width * 3 / 4, y + amplitude, x + width * 3 / 4, y + amplitude, x + width, y)
                stroke.color = ink
                stroke.strokeWidth = thickness
                canvas.drawPath(wave, stroke)
            }
            UnderlineStyle.Dotted -> {
                val step = thickness * 2
                var dot = x
                while (dot < x + width) {
                    canvas.drawRect(dot, y, min(dot + thickness, x + width), y + thickness, fill)
                    dot += step
                }
            }
            UnderlineStyle.Dashed -> {
                val dash = width / 3
                canvas.drawRect(x, y, x + dash, y + thickness, fill)
                canvas.drawRect(x + dash * 2, y, x + width, y + thickness, fill)
            }
        }
    }

    /**
     * 逐格画一个字素簇：制表符、块元素和 Powerline 分隔符按格子自己画；私用区的图标退到符号字体，后面跟着空格子时
     * 可以占两格宽；按文字画的符号先用 [TextSymbolFont] 里有它的字体；其余交给系统按后备字体画，比格子窄时在格子里居中
     * （宽字符占的两格比中文字形宽一点）。
     */
    private fun drawCell(
        canvas: Canvas, cells: Array<ScreenCell>, column: Int, x: Float, top: Float, width: Float, ink: Int,
        bold: Boolean, italic: Boolean,
    ) {
        val text = cells[column].text
        val codepoint = text.codePointAt(0)
        val single = Character.charCount(codepoint) == text.length
        val rect = GlyphRect(x, top, width, font.cellHeight)
        if (single && drawGeometric(canvas, codepoint, rect, ink)) return
        if (single && SymbolFont.isPrivateUse(codepoint) && hasSymbol(codepoint)) {
            val iconWidth = if (cells[column].width == CellWidth.Narrow && followedByBlank(cells, column)) width * 2 else width
            drawSymbol(canvas, text, x, top, iconWidth, ink)
            return
        }
        val prefersText = single && TextPresentation.prefersText(codepoint)
        val textSymbol = if (prefersText) textSymbol(codepoint) else null
        val shown = if (prefersText && textSymbol == null) text + TextPresentation.SELECTOR else text
        val paint = textSymbol ?: font.paint(bold, italic)
        val advance = advance(paint, shown)
        paint.color = ink
        canvas.drawText(shown, x + max(0f, (width - advance) / 2), top + font.ascent, paint)
    }

    /** 这一格后面紧跟着一个空格子：图标可以占两格宽，和 Ghostty 处理 Nerd Font 图标的做法一样。 */
    private fun followedByBlank(cells: Array<ScreenCell>, column: Int): Boolean {
        val next = cells.getOrNull(column + 1) ?: return false
        return next.width == CellWidth.Narrow && (next.text.isEmpty() || next.text == " ")
    }

    /**
     * 制表符、块元素、没有文字字形的几何符号和 Powerline 的几何分隔符按格子自己画；不是这类字符时返回 false。Powerline 的形状左右各多画
     * 半个像素，盖住格子宽度不是整像素时和相邻格子背景之间露出的缝。
     */
    private fun drawGeometric(canvas: Canvas, codepoint: Int, rect: GlyphRect, ink: Int): Boolean {
        if (BoxDrawing.handles(codepoint)) {
            val shape = BoxDrawing.path(codepoint, rect, font.thickness, 1f) ?: return false
            drawShape(canvas, shape, ink)
            return true
        }
        if (BlockElement.handles(codepoint)) {
            val shape = BlockElement.shape(codepoint, rect, 1f) ?: return false
            fill.color = withAlpha(ink, shape.alpha)
            canvas.drawPath(shape.path.toAndroidPath(), fill)
            return true
        }
        if (GeometricSymbol.handles(codepoint)) {
            val shape = GeometricSymbol.path(codepoint, rect) ?: return false
            drawShape(canvas, shape, ink)
            return true
        }
        if (!PowerlineGlyph.handles(codepoint)) return false
        val padded = GlyphRect(rect.x - 0.5f, rect.y, rect.width + 1f, rect.height)
        val shape = PowerlineGlyph.path(codepoint, padded, font.thickness) ?: return false
        canvas.save()
        canvas.clipRect(padded.minX, padded.minY, padded.maxX, padded.maxY)
        drawShape(canvas, shape, ink)
        canvas.restore()
        return true
    }

    private fun drawShape(canvas: Canvas, shape: GlyphShape, ink: Int) {
        val path = shape.path.toAndroidPath()
        when (val paint = shape.paint) {
            GlyphPaint.Fill -> {
                fill.color = ink
                fill.isAntiAlias = true
                canvas.drawPath(path, fill)
                fill.isAntiAlias = false
            }
            is GlyphPaint.Stroke -> {
                stroke.color = ink
                stroke.strokeWidth = paint.width
                canvas.drawPath(path, stroke)
            }
        }
    }

    /** 画 [codepoint] 用的只有文字字形的字体；系统里没有带它的时为空。 */
    private fun textSymbol(codepoint: Int): Paint? {
        val paints = font.textSymbols
        if (paints.isEmpty()) return null
        val index = textSymbolChoice.getOrPut(codepoint) { TextSymbolFont.pick(codepoint, paints) ?: -1 }
        return paints.getOrNull(index)
    }

    private fun hasSymbol(codepoint: Int): Boolean {
        val symbols = font.symbols ?: return false
        return symbolGlyphs.getOrPut(codepoint) { symbols.hasGlyph(String(Character.toChars(codepoint))) }
    }

    /** 按字形的外框缩到 [width] 宽、不超过格子高，水平居中，竖直方向以格子中线居中。 */
    private fun drawSymbol(canvas: Canvas, text: String, x: Float, top: Float, width: Float, ink: Int) {
        val symbols = font.symbols ?: return
        symbols.getTextBounds(text, 0, text.length, scratchRect)
        if (scratchRect.width() <= 0 || scratchRect.height() <= 0) return
        val scale = min(1f, min(width * 0.96f / scratchRect.width(), font.cellHeight * 0.96f / scratchRect.height()))
        symbolPaint.set(symbols)
        symbolPaint.textSize = symbols.textSize * scale
        symbolPaint.color = ink
        symbolPaint.getTextBounds(text, 0, text.length, scratchRect)
        val left = x + (width - scratchRect.width()) / 2 - scratchRect.left
        val baseline = top + font.cellHeight / 2 - (scratchRect.top + scratchRect.bottom) / 2f
        canvas.drawText(text, left, baseline, symbolPaint)
    }

    private fun advance(paint: Paint, text: String): Float {
        if (advanceFont !== font) {
            advanceCache.clear()
            advanceFont = font
        }
        val key = if (paint === font.paint(bold = false, italic = false)) text else "${System.identityHashCode(paint)}:$text"
        advanceCache[key]?.let { return it }
        if (advanceCache.size > CACHE_LIMIT) advanceCache.clear()
        return paint.measureText(text).also { advanceCache[key] = it }
    }

    /** 画一层图片。整张画、靠 [drawRow] 的裁剪只留这一行，跨几行的图片只重画一行时也对得上。 */
    private fun drawImages(canvas: Canvas, frame: ScreenFrame, row: Int, layer: ImageLayer) {
        for (image in frame.images) {
            if (image.layer != layer) continue
            val bitmap = cachedImage(image) ?: continue
            scratchRect.set(
                image.sourceX.toInt(), image.sourceY.toInt(), (image.sourceX + image.sourceWidth).toInt(),
                (image.sourceY + image.sourceHeight).toInt(),
            )
            val left = (image.x * font.cellWidth).toFloat()
            val top = ((image.y - row) * font.cellHeight).toFloat()
            scratchRectF.set(
                left, top, left + (image.width * font.cellWidth).toFloat(), top + (image.height * font.cellHeight).toFloat(),
            )
            canvas.drawBitmap(bitmap, scratchRect, scratchRectF, imagePaint)
        }
    }

    private fun cachedImage(image: ScreenImage): Bitmap? {
        imageCache[image.generation]?.let { return it }
        val bitmap = imageSource?.invoke(image.id, image.generation) ?: return null
        imageCache[image.generation] = bitmap
        return bitmap
    }

    /** 丢掉帧里已经没有了的图片。 */
    fun pruneImages(images: List<ScreenImage>) {
        if (imageCache.isEmpty()) return
        val shown = images.mapTo(HashSet()) { it.generation }
        imageCache.keys.retainAll(shown)
    }

    fun clearImages() {
        imageCache.clear()
    }

    /** 光标所在格子（宽字符占两格）在网格里的矩形；没有光标时为空。 */
    fun cursorRect(frame: ScreenFrame): RectF? {
        val cursor = frame.cursor ?: return null
        val column = if (cursor.onWideTail) max(cursor.column - 1, 0) else cursor.column
        val wide = cellAt(frame, cursor.row, column)?.width == CellWidth.Wide
        val left = column * font.cellWidth
        val top = cursor.row * font.cellHeight
        return RectF(left, top, left + font.cellWidth * (if (wide) 2 else 1), top + font.cellHeight)
    }

    /** 画光标，坐标是整个网格的。没有键盘焦点时画成空心的。 */
    fun drawCursor(canvas: Canvas, frame: ScreenFrame, focused: Boolean) {
        val cursor = frame.cursor ?: return
        val rect = cursorRect(frame) ?: return
        val column = if (cursor.onWideTail) max(cursor.column - 1, 0) else cursor.column
        val cell = cellAt(frame, cursor.row, column) ?: ScreenCell.blank
        val cellForeground = foreground(frame, cell)
        val cellBackground = background(frame, cell)
        val color = when (val configured = settings.cursorColor) {
            is TerminalColor.Rgb -> configured.color
            TerminalColor.CellForeground -> cellForeground
            TerminalColor.CellBackground -> cellBackground
            null -> frame.cursorColor ?: frame.foreground
        }
        fill.color = argb(color)
        stroke.color = argb(color)
        val bar = max(1.5f, font.size / 8)
        when (if (focused) cursor.shape else CursorShape.BlockHollow) {
            CursorShape.Block -> {
                canvas.drawRect(rect, fill)
                // 隐藏的字（SGR 8）光标压在上面也不露出来。
                if (!hasText(cell)) return
                val text = when (val configured = settings.cursorText) {
                    is TerminalColor.Rgb -> configured.color
                    TerminalColor.CellForeground -> cellForeground
                    TerminalColor.CellBackground, null -> cellBackground
                }
                val ink = argb(text)
                val glyph = GlyphRect(rect.left, rect.top, rect.width(), rect.height())
                val codepoint = cell.text.codePointAt(0)
                if (Character.charCount(codepoint) == cell.text.length && drawGeometric(canvas, codepoint, glyph, ink)) {
                    return
                }
                val cells = frame.cells.getOrNull(cursor.row) ?: return
                drawCell(
                    canvas, cells, column, rect.left, rect.top, rect.width(), ink, CellAttributes.bold in cell.attributes,
                    CellAttributes.italic in cell.attributes,
                )
            }
            CursorShape.BlockHollow -> {
                stroke.strokeWidth = 1f
                canvas.drawRect(rect.left + 0.5f, rect.top + 0.5f, rect.right - 0.5f, rect.bottom - 0.5f, stroke)
            }
            CursorShape.Bar -> canvas.drawRect(rect.left, rect.top, rect.left + bar, rect.bottom, fill)
            CursorShape.Underline -> canvas.drawRect(rect.left, rect.bottom - bar, rect.right, rect.bottom, fill)
        }
    }

    /** 组字中的文字画在光标处，带下划线，盖住下面的格子。 */
    fun drawMarkedText(canvas: Canvas, frame: ScreenFrame, text: String) {
        val rect = cursorRect(frame) ?: return
        val paint = font.paint(bold = false, italic = false)
        val width = max(paint.measureText(text), rect.width())
        fill.color = argb(frame.background)
        canvas.drawRect(rect.left, rect.top, rect.left + width, rect.bottom, fill)
        paint.color = argb(frame.foreground)
        canvas.drawText(text, rect.left, rect.top + font.ascent, paint)
        fill.color = argb(frame.foreground)
        val y = rect.top + font.ascent + max(1f, font.size * 0.12f)
        canvas.drawRect(rect.left, y, rect.left + width, y + font.thickness, fill)
    }

    private fun cellAt(frame: ScreenFrame, row: Int, column: Int): ScreenCell? =
        frame.cells.getOrNull(row)?.getOrNull(column)

    private fun hasText(cell: ScreenCell): Boolean =
        cell.text.isNotEmpty() && cell.text != " " && CellAttributes.invisible !in cell.attributes

    /** 单元格实际的前景、背景色：没设的用默认色，反显时对调。 */
    private fun foreground(frame: ScreenFrame, cell: ScreenCell): Rgb =
        if (CellAttributes.inverse in cell.attributes) cell.background ?: frame.background
        else cell.foreground ?: frame.foreground

    private fun background(frame: ScreenFrame, cell: ScreenCell): Rgb =
        if (CellAttributes.inverse in cell.attributes) cell.foreground ?: frame.foreground
        else cell.background ?: frame.background

    private fun ink(frame: ScreenFrame, cell: ScreenCell): Int {
        val color = argb(foreground(frame, cell))
        return if (CellAttributes.faint in cell.attributes) withAlpha(color, FAINT_ALPHA) else color
    }

    companion object {
        /** 淡色（SGR 2）的字的不透明度。 */
        private const val FAINT_ALPHA = 0.6f

        private const val CACHE_LIMIT = 20_000

        fun argb(rgb: Rgb): Int =
            (0xFF shl 24) or (rgb.r.toInt() shl 16) or (rgb.g.toInt() shl 8) or rgb.b.toInt()

        private fun withAlpha(color: Int, alpha: Float): Int =
            (floor(((color ushr 24) and 0xFF) * alpha + 0.5f).toInt() shl 24) or (color and 0xFFFFFF)
    }
}
