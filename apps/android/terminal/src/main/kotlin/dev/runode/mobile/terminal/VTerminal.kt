package dev.runode.mobile.terminal

import android.graphics.Bitmap
import android.os.Looper
import dev.runode.mobile.protocol.CursorStyle
import dev.runode.mobile.protocol.GridSize
import dev.runode.mobile.protocol.OptionAsAlt
import dev.runode.mobile.protocol.Rgb
import dev.runode.mobile.protocol.TermSettings
import dev.runode.mobile.protocol.TerminalColor
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder
import kotlin.math.abs
import kotlin.math.floor
import kotlin.math.max
import kotlin.math.min
import kotlin.time.Duration.Companion.milliseconds

/**
 * 手机这边的一份 libghostty-vt 状态机，只消费宿主转来的字节流（VT 重放和 PTY 输出）。
 *
 * 权威的那份 VT 在宿主里，终端查询只由它应答：这里不注册写回 PTY 的回调，查询序列静默忽略。改 VT 状态的操作
 * （改尺寸、换主题）都在宿主插进输出流的 `Resized`、`ThemeApplied` 处做，两份 VT 才不会分叉。按键、粘贴按这份 VT
 * 当前的模式编码好后交给调用方，由它发给宿主。
 *
 * 线程：libghostty 的句柄不能跨线程用，所以只在主线程上建、喂、读和释放，debug 构建里每个入口都断言这一点。
 * 用完调 [close]；忘了调时，对象被回收后由 [HandleCleaner] 的后台线程释放 native 那份（那时已没有别人在用它）。
 *
 * @throws VTerminalError.CreationFailed libghostty 建不出终端时
 */
class VTerminal(size: GridSize, settings: TermSettings) : AutoCloseable {
    private var handle: Long
    private val cleanable: HandleCleaner.Cleanable

    /** 最近一次改尺寸、最近一次喂字节的时刻（[System.nanoTime]），[isRenderHeld] 用。 */
    private var resizedAt = System.nanoTime() - TEN_SECONDS_NS
    private var fedAt = System.nanoTime() - TEN_SECONDS_NS

    /**
     * 平滑滚动时视口之外再往回看的零点几行：画的时候整屏往下错开这么多，露出视口上面那一行的一部分。取值在
     * [0, 1)，视口在历史最顶上时为 0。
     */
    private var scrollFraction = 0.0

    /** [refresh] 让 native 那边写一帧的缓冲区，不够大时换个大的。 */
    private var snapshot: ByteBuffer
    private val scrollbarValues = LongArray(3)

    /** 读格子时上一个前景色、背景色，一行里连着同色的格子共用一个 [Rgb]。 */
    private var lastColorBits = -1
    private var lastColor: Rgb? = null

    var size: GridSize
        private set
    var settings: TermSettings
        private set

    init {
        checkThread()
        val cols = max(size.cols.toInt(), 1)
        val rows = max(size.rows.toInt(), 1)
        val created = VtJni.nativeNew(cols, rows)
        if (created == 0L) throw VTerminalError.CreationFailed()
        handle = created
        cleanable = HandleCleaner.register(this) { VtJni.nativeFree(created) }
        this.size = size
        this.settings = settings
        snapshot = ByteBuffer.allocateDirect(snapshotCapacity(cols, rows)).order(ByteOrder.nativeOrder())
        configureCommon(settings.scrollbackLimit)
        VtJni.nativeResize(handle, cols, rows, size.cellWidthPx.toInt(), size.cellHeightPx.toInt())
        applyTheme(settings)
    }

    override fun close() {
        checkThread()
        if (handle == 0L) return
        handle = 0L
        cleanable.clean()
    }

    // 喂字节、改尺寸、换主题

    /** 把宿主转来的字节（VT 重放或 PTY 输出）喂给 VT。 */
    fun feed(bytes: ByteArray, offset: Int = 0, length: Int = bytes.size - offset) {
        checkThread()
        if (offset < 0 || length < 0 || offset > bytes.size - length) {
            throw IndexOutOfBoundsException("offset $offset、length $length 超出了 ${bytes.size} 字节")
        }
        if (length == 0) return
        fedAt = System.nanoTime()
        VtJni.nativeFeed(live(), bytes, offset, length)
    }

    /** 在宿主改尺寸的位置（`HostMsg.Resized`）改这份 VT 的尺寸。 */
    fun resize(newSize: GridSize) {
        checkThread()
        if (newSize.cols.toInt() == 0 || newSize.rows.toInt() == 0 || newSize == size) return
        size = newSize
        resizedAt = System.nanoTime()
        scrollFraction = 0.0
        VtJni.nativeResize(
            live(), newSize.cols.toInt(), newSize.rows.toInt(), newSize.cellWidthPx.toInt(),
            newSize.cellHeightPx.toInt(),
        )
    }

    /** 在宿主套用主题的位置（`HostMsg.ThemeApplied`）套用同一份主题，步骤和宿主的 `vt::apply_theme` 一样，两份 VT 才不会分叉。 */
    fun applyTheme(newSettings: TermSettings) {
        checkThread()
        settings = newSettings
        val palette = newSettings.palette.map { (it.index.toInt() shl 24) or it.color.packed }.toIntArray()
        val cursor = (newSettings.cursorColor as? TerminalColor.Rgb)?.color?.packed ?: -1
        val cursorStyle = when (newSettings.cursorStyle) {
            CursorStyle.Block -> GhosttyAbi.CURSOR_STYLE_BLOCK
            CursorStyle.BlockHollow -> GhosttyAbi.CURSOR_STYLE_BLOCK_HOLLOW
            CursorStyle.Bar -> GhosttyAbi.CURSOR_STYLE_BAR
            CursorStyle.Underline -> GhosttyAbi.CURSOR_STYLE_UNDERLINE
        }
        // 没配置时默认闪烁；libghostty 的空值是不闪烁，所以显式给 true。
        val blink = newSettings.cursorBlink ?: true
        val applied = VtJni.nativeApplyTheme(
            live(), newSettings.background.packed, newSettings.foreground.packed, cursor, palette, cursorStyle, blink,
        )
        if (applied) configureCommon(newSettings.scrollbackLimit)
    }

    /** 宿主 `vt::configure_common` 里的选项，回滚的字节上限另按 [scrollbackBytesCap] 封顶，图片的按 [kittyImageStorageBytes]。 */
    private fun configureCommon(scrollbackBytes: ULong) {
        val bytes = minOf(scrollbackBytes, scrollbackBytesCap.toULong()).toLong()
        VtJni.nativeConfigureCommon(
            live(), scrollbackLines.toLong(), bytes, unknownSequenceMaxBytes.toLong(), kittyImageStorageBytes,
        )
    }

    /**
     * 先别刷新屏幕：程序用同步输出冻结着屏幕（有时限，免得程序出错时画面卡死），或者刚改过尺寸、程序还没重画完。
     * 后者是因为 shell 集成说了会自己重画提示符（OSC 133 的 `redraw=1`）时，libghostty 改尺寸先把提示符擦掉，等 shell
     * 收到 SIGWINCH 画回来要经宿主转一趟；改尺寸后短时间内等到改完以后的输出再画，不画出擦掉了还没画回来的那一帧。
     */
    val isRenderHeld: Boolean
        get() {
            checkThread()
            val now = System.nanoTime()
            val since = VtJni.nativeRenderHeldSince(live())
            if (since >= 0 && now - since < HOLD_LIMIT_NS) return true
            return now - resizedAt < RESIZE_HOLD_NS && (fedAt < resizedAt || now - fedAt < REDRAW_SETTLE_NS)
        }

    // 模式和状态

    /** 读一个终端模式；[ansi] 为假是 DEC 私有模式（`?` 开头的）。 */
    fun mode(value: Int, ansi: Boolean): Boolean {
        checkThread()
        return VtJni.nativeMode(live(), value, ansi)
    }

    /** 程序开着括号粘贴（mode 2004）。 */
    val bracketedPaste: Boolean get() = mode(2004, ansi = false)

    // 视口滚动

    /** 回滚历史的位置：总行数、视口顶在第几行、视口多高。 */
    data class Scrollbar(val total: Int, val offset: Int, val length: Int) {
        /** 视口在最底下（跟着新输出走）。 */
        val atBottom: Boolean get() = offset + length >= total
    }

    /** 视口停在最底下，也没有平滑滚动错开的零点几行。 */
    val viewportAtBottom: Boolean get() = scrollbar.atBottom && scrollFraction == 0.0

    val scrollbar: Scrollbar
        get() {
            checkThread()
            if (!VtJni.nativeScrollbar(live(), scrollbarValues)) {
                return Scrollbar(size.rows.toInt(), 0, size.rows.toInt())
            }
            return Scrollbar(scrollbarValues[0].toInt(), scrollbarValues[1].toInt(), scrollbarValues[2].toInt())
        }

    /** 回到最底下，跟着新输出走。 */
    fun scrollToBottom() {
        checkThread()
        scrollFraction = 0.0
        VtJni.nativeScrollToBottom(live())
    }

    /**
     * 按像素滚动回滚历史：[lines] 为正往回看更早的内容，可以是零点几行。凑够整行的部分挪视口，剩下的记成错开量
     * （[ScreenFrame.scrollOffset]），做法同桌面的 `Session::scroll_smoothly`。返回画面是否变了；到了历史顶上或者
     * 已经在最底下、挪不动时为假。
     */
    fun scrollSmoothly(lines: Double): Boolean {
        checkThread()
        val beforeOffset = scrollbar.offset
        val beforeFraction = scrollFraction
        var offset = scrollFraction + lines
        val whole = floor(offset)
        if (whole != 0.0) {
            VtJni.nativeScrollBy(live(), -whole.toLong())
            // 到了历史顶上或者已经在底部时视口挪不动，按实际挪了多少扣。
            offset -= (beforeOffset - scrollbar.offset).toDouble()
        }
        // 视口上面没有行时不能往下错开；在底部继续往下滚时也不会错开成负的。
        val top = scrollbar.offset
        scrollFraction = if (top == 0) 0.0 else min(max(offset, 0.0), 0.999)
        return top != beforeOffset || scrollFraction != beforeFraction
    }

    /**
     * 滚动归程序管：开着鼠标上报时滚轮发给它（全屏的 agent 界面多半这样自己滚）；在备用屏上开着备用滚动（模式 1007）
     * 时换成上下方向键。两样都不是时滚的是这份 VT 的回滚历史。
     */
    val programScrolls: Boolean
        get() = mouseTracking || (alternateScreen && mode(1007, ansi = false))

    /** 程序开着鼠标上报（任何一种）。 */
    val mouseTracking: Boolean
        get() {
            checkThread()
            return VtJni.nativeMouseTracking(live())
        }

    private val alternateScreen: Boolean
        get() = VtJni.nativeAlternateScreen(live())

    /**
     * 在第 [row] 行第 [column] 列滚 [lines] 行（负数往上看更早的内容）要发给程序的字节：开着鼠标上报时每行一个滚轮事件
     * （同桌面的 `Session::scroll`），否则按备用滚动每行一个方向键。程序不管滚动时为空。
     */
    fun encodeWheel(lines: Int, column: Int, row: Int): ByteArray {
        checkThread()
        if (lines == 0) return ByteArray(0)
        if (!mouseTracking) {
            if (!programScrolls) return ByteArray(0)
            val arrow = encode(KeyInput(key = if (lines < 0) TerminalKey.Up else TerminalKey.Down))
            val bytes = ByteArrayOutputStream()
            repeat(abs(lines)) { bytes.write(arrow) }
            return bytes.toByteArray()
        }
        val button = if (lines < 0) GhosttyAbi.MOUSE_BUTTON_FOUR else GhosttyAbi.MOUSE_BUTTON_FIVE
        val bytes = ByteArrayOutputStream()
        for (i in 0 until abs(lines)) {
            val event = encodeMouse(GhosttyAbi.MOUSE_ACTION_PRESS, button, column, row) ?: break
            bytes.write(event)
        }
        return bytes.toByteArray()
    }

    /** 在第 [row] 行第 [column] 列点一下（左键按下再松开）要发给程序的字节；程序没开鼠标上报时为空。 */
    fun encodeClick(column: Int, row: Int): ByteArray {
        checkThread()
        if (!mouseTracking) return ByteArray(0)
        val bytes = ByteArrayOutputStream()
        for (action in intArrayOf(GhosttyAbi.MOUSE_ACTION_PRESS, GhosttyAbi.MOUSE_ACTION_RELEASE)) {
            val event = encodeMouse(action, GhosttyAbi.MOUSE_BUTTON_LEFT, column, row) ?: return ByteArray(0)
            bytes.write(event)
        }
        return bytes.toByteArray()
    }

    /** 按程序要的鼠标上报格式编码一个事件；编码失败时为空。 */
    private fun encodeMouse(action: Int, button: Int, column: Int, row: Int): ByteArray? {
        val columns = size.cols.toInt()
        val rows = size.rows.toInt()
        return VtJni.nativeEncodeMouse(
            live(), action, button, column.coerceIn(0, columns - 1), row.coerceIn(0, rows - 1), columns, rows,
            max(size.cellWidthPx.toInt(), 1), max(size.cellHeightPx.toInt(), 1),
        )
    }

    // 读屏幕

    /** 按 render state 把变了的行抄进 [frame]，返回改了哪些。没变的行不碰。 */
    fun refresh(frame: ScreenFrame): FrameChange {
        checkThread()
        val handle = live()
        var retry = false
        repeat(SNAPSHOT_ATTEMPTS) {
            val written = VtJni.nativeSnapshot(
                handle, snapshot, retry, frame.columns, frame.rows, frame.background.packed,
                frame.foreground.packed, frame.cursorColor?.packed ?: -1, frame.above.size,
            )
            if (written == 0) return FrameChange.none
            if (written > 0) return apply(snapshot, frame)
            snapshot = ByteBuffer.allocateDirect(-written + -written / 4).order(ByteOrder.nativeOrder())
            retry = true
        }
        return FrameChange.none
    }

    /** 把 native 那边按 [SnapshotLayout] 写好的一帧抄进 [frame]。 */
    private fun apply(buffer: ByteBuffer, frame: ScreenFrame): FrameChange {
        val flags = buffer.getInt(SnapshotLayout.FLAGS)
        val full = flags and SnapshotLayout.FLAG_FULL != 0
        val columns = buffer.getInt(SnapshotLayout.COLUMNS)
        val rows = buffer.getInt(SnapshotLayout.ROWS)
        if (columns != frame.columns || rows != frame.rows) {
            frame.columns = columns
            frame.rows = rows
            frame.cells = Array(rows) { Array(columns) { ScreenCell.blank } }
        }
        frame.background = rgb(buffer.getInt(SnapshotLayout.BACKGROUND))
        frame.foreground = rgb(buffer.getInt(SnapshotLayout.FOREGROUND))
        frame.cursorColor =
            if (flags and SnapshotLayout.FLAG_CURSOR_COLOR != 0) rgb(buffer.getInt(SnapshotLayout.CURSOR_COLOR)) else null

        val aboveColumns = buffer.getInt(SnapshotLayout.ABOVE_COLUMNS)
        if (frame.above.size != aboveColumns) {
            frame.above = Array(aboveColumns) { ScreenCell.blank }
        }
        frame.scrollOffset = if (aboveColumns == 0) 0.0 else scrollFraction

        val graphemes = buffer.getInt(SnapshotLayout.GRAPHEMES_OFFSET)
        val changedRows = mutableListOf<Int>()
        var at = SnapshotLayout.HEADER_SIZE
        repeat(buffer.getInt(SnapshotLayout.ROW_COUNT)) {
            val y = buffer.getInt(at + SnapshotLayout.ROW_Y)
            val row = if (y == -1) frame.above else frame.cells[y]
            var cellAt = at + SnapshotLayout.ROW_HEADER_SIZE
            for (x in 0 until columns) {
                row[x] = readCell(buffer, cellAt, graphemes)
                cellAt += SnapshotLayout.CELL_SIZE
            }
            if (y >= 0) changedRows += y
            at += SnapshotLayout.ROW_HEADER_SIZE + columns * SnapshotLayout.CELL_SIZE
        }

        val cursor = if (flags and SnapshotLayout.FLAG_CURSOR != 0) {
            ScreenCursor(
                column = buffer.getInt(SnapshotLayout.CURSOR_COLUMN),
                row = buffer.getInt(SnapshotLayout.CURSOR_ROW),
                shape = CursorShape.entries[buffer.getInt(SnapshotLayout.CURSOR_SHAPE)],
                blinking = flags and SnapshotLayout.FLAG_CURSOR_BLINKING != 0,
                onWideTail = flags and SnapshotLayout.FLAG_CURSOR_WIDE_TAIL != 0,
            )
        } else {
            null
        }
        val cursorChanged = cursor != frame.cursor
        frame.cursor = cursor
        val imagesChanged = flags and SnapshotLayout.FLAG_IMAGES_CHANGED != 0
        if (imagesChanged) {
            frame.images = readImages(
                buffer, buffer.getInt(SnapshotLayout.IMAGES_OFFSET), buffer.getInt(SnapshotLayout.IMAGE_COUNT),
            )
        }
        frame.nextImageFrame = if (flags and SnapshotLayout.FLAG_NEXT_IMAGE_FRAME != 0) {
            buffer.getLong(SnapshotLayout.NEXT_IMAGE_FRAME_MS).milliseconds
        } else {
            null
        }
        return FrameChange(
            full = full,
            rows = if (full) emptyList() else changedRows,
            cursorChanged = cursorChanged,
            aboveChanged = flags and SnapshotLayout.FLAG_ABOVE_CHANGED != 0,
            imagesChanged = imagesChanged,
        )
    }

    private fun readCell(buffer: ByteBuffer, at: Int, graphemes: Int): ScreenCell {
        val text = buffer.getInt(at + SnapshotLayout.CELL_TEXT)
        val foreground = buffer.getInt(at + SnapshotLayout.CELL_FOREGROUND)
        val background = buffer.getInt(at + SnapshotLayout.CELL_BACKGROUND)
        val attributes = buffer.get(at + SnapshotLayout.CELL_ATTRIBUTES).toInt() and 0xFF
        val underline = buffer.get(at + SnapshotLayout.CELL_UNDERLINE).toInt() and 0xFF
        val width = buffer.get(at + SnapshotLayout.CELL_WIDTH).toInt() and 0xFF
        if (text == 0 && foreground == 0 && background == 0 && attributes == 0 && underline == 0 && width == 0) {
            return ScreenCell.blank
        }
        return ScreenCell(
            text = text(buffer, text, graphemes),
            foreground = cellColor(foreground),
            background = cellColor(background),
            attributes = CellAttributes(attributes),
            underline = UnderlineStyle.entries[underline],
            width = CellWidth.entries[width],
        )
    }

    private fun text(buffer: ByteBuffer, text: Int, graphemes: Int): String {
        if (text == 0) return ""
        if (text and SnapshotLayout.CELL_TEXT_GRAPHEME == 0) {
            return if (text < asciiStrings.size) asciiStrings[text] else String(Character.toChars(text))
        }
        var at = graphemes + (text and SnapshotLayout.CELL_TEXT_GRAPHEME.inv()) * 4
        val count = buffer.getInt(at)
        val cluster = StringBuilder(count * 2)
        repeat(count) {
            at += 4
            cluster.appendCodePoint(buffer.getInt(at))
        }
        return cluster.toString()
    }

    private fun cellColor(bits: Int): Rgb? {
        if (bits and SnapshotLayout.COLOR_SET == 0) return null
        if (bits == lastColorBits) return lastColor
        return rgb(bits).also {
            lastColorBits = bits
            lastColor = it
        }
    }

    /** 帧里的图片，像素尺寸按单元格的像素尺寸折成格数，排成画的先后。 */
    private fun readImages(buffer: ByteBuffer, offset: Int, count: Int): List<ScreenImage> {
        val cellWidth = max(size.cellWidthPx.toInt(), 1).toDouble()
        val cellHeight = max(size.cellHeightPx.toInt(), 1).toDouble()
        val images = ArrayList<Pair<Int, ScreenImage>>(count)
        for (i in 0 until count) {
            val at = offset + i * SnapshotLayout.IMAGE_SIZE
            fun int(field: Int) = buffer.getInt(at + field)
            fun uint(field: Int) = buffer.getInt(at + field).toUInt()
            val image = ScreenImage(
                id = uint(SnapshotLayout.IMAGE_ID),
                generation = buffer.getLong(at + SnapshotLayout.IMAGE_GENERATION).toULong(),
                x = int(SnapshotLayout.IMAGE_COLUMN) + uint(SnapshotLayout.IMAGE_OFFSET_X).toDouble() / cellWidth,
                y = int(SnapshotLayout.IMAGE_ROW) + uint(SnapshotLayout.IMAGE_OFFSET_Y).toDouble() / cellHeight,
                width = uint(SnapshotLayout.IMAGE_WIDTH).toDouble() / cellWidth,
                height = uint(SnapshotLayout.IMAGE_HEIGHT).toDouble() / cellHeight,
                sourceX = uint(SnapshotLayout.IMAGE_SOURCE_X),
                sourceY = uint(SnapshotLayout.IMAGE_SOURCE_Y),
                sourceWidth = uint(SnapshotLayout.IMAGE_SOURCE_WIDTH),
                sourceHeight = uint(SnapshotLayout.IMAGE_SOURCE_HEIGHT),
                layer = ImageLayer.entries[int(SnapshotLayout.IMAGE_LAYER)],
            )
            images += int(SnapshotLayout.IMAGE_Z) to image
        }
        return images.sortedWith(compareBy({ it.second.layer }, { it.first })).map { it.second }
    }

    /** 帧里一块图片（[ScreenImage.id]、[ScreenImage.generation]）现在的像素。图片已经换了版本或删了时为空，下一帧会带上新的。 */
    fun image(id: UInt, generation: ULong): Bitmap? {
        val pixels = imagePixels(id, generation) ?: return null
        val rgba = pixels.rgba
        val colors = IntArray(pixels.width * pixels.height) { i ->
            val p = i * 4
            (rgba[p + 3].toInt() and 0xFF shl 24) or (rgba[p].toInt() and 0xFF shl 16) or
                (rgba[p + 1].toInt() and 0xFF shl 8) or (rgba[p + 2].toInt() and 0xFF)
        }
        return Bitmap.createBitmap(colors, pixels.width, pixels.height, Bitmap.Config.ARGB_8888)
    }

    /** 一块图片不预乘的 RGBA，每行 `width * 4` 字节。 */
    internal class ImagePixels(val width: Int, val height: Int, val rgba: ByteArray)

    internal fun imagePixels(id: UInt, generation: ULong): ImagePixels? {
        checkThread()
        val dimensions = IntArray(2)
        val rgba = VtJni.nativeImagePixels(live(), id.toInt(), generation.toLong(), dimensions) ?: return null
        return ImagePixels(dimensions[0], dimensions[1], rgba)
    }

    /** 回滚历史现在的字节上限。测试用。 */
    internal val scrollbackMaxBytes: Long
        get() {
            checkThread()
            return VtJni.nativeScrollbackMaxBytes(live())
        }

    /** 当前一屏的文字，一行一个，行尾空白去掉。测试和调试用。 */
    fun screenLines(): List<String> {
        val frame = ScreenFrame()
        refresh(frame)
        return frame.lines
    }

    // 输入编码

    /** 按这份 VT 当前的模式（应用光标键、Kitty 键盘协议、modifyOtherKeys 等）编码一次按键；没有产生字节时返回空。 */
    fun encode(input: KeyInput): ByteArray {
        checkThread()
        val optionAsAlt = when (settings.optionAsAlt) {
            OptionAsAlt.Yes -> true
            OptionAsAlt.Left -> !input.rightAlt
            OptionAsAlt.Right -> input.rightAlt
            OptionAsAlt.No, OptionAsAlt.Auto -> false
        }
        // Option 当作 Alt 时不能算「已消耗」，编码器才会改用未加修饰的字符并加 ESC 前缀。
        val consumed = if (optionAsAlt) input.consumedModifiers - KeyModifiers.alt else input.consumedModifiers
        val option = when (settings.optionAsAlt) {
            OptionAsAlt.Yes -> GhosttyAbi.OPTION_AS_ALT_TRUE
            OptionAsAlt.Left -> GhosttyAbi.OPTION_AS_ALT_LEFT
            OptionAsAlt.Right -> GhosttyAbi.OPTION_AS_ALT_RIGHT
            OptionAsAlt.No, OptionAsAlt.Auto -> GhosttyAbi.OPTION_AS_ALT_FALSE
        }
        // 控制字符不传，由编码器按逻辑键算。
        val text = input.text?.takeUnless { text -> text.any { it.code < 0x20 || it.code == 0x7F } }
        return VtJni.nativeEncodeKey(
            live(), input.key.ghosttyKey, input.modifiers.ghosttyMods(input.rightAlt),
            consumed.ghosttyMods(input.rightAlt), input.unshifted?.code ?: 0, text?.encodeToByteArray(), option,
        )
    }

    /** 粘贴的字节：去掉不安全的控制字符，程序开着括号粘贴时套上括号，否则换行换成回车。 */
    fun encodePaste(text: String): ByteArray {
        checkThread()
        val data = text.encodeToByteArray()
        if (data.isEmpty()) return data
        return VtJni.nativeEncodePaste(data, bracketedPaste)
    }

    private fun live(): Long {
        check(handle != 0L) { "VTerminal 已经关掉了" }
        return handle
    }

    companion object {
        /** 回滚历史最多留多少行，不少于宿主的 `SCROLLBACK_LINES`，重放的历史才放得下；另有字节上限 [scrollbackBytesCap]。 */
        internal const val scrollbackLines = 10_000

        /**
         * 回滚历史的字节上限取宿主下发的 `TermSettings.scrollbackLimit` 和它中小的那个：这份 VT 从宿主的重放起步，
         * 用不着和桌面留一样多；手机内存紧，一个会话占太多会被系统杀掉。
         */
        internal const val scrollbackBytesCap: Long = 4L shl 20

        /** 未知 OSC 最多留多少字节，不少于宿主的 `UNKNOWN_SEQUENCE_MAX_BYTES`，宿主转过来的序列才不会在这里被截断。 */
        internal const val unknownSequenceMaxBytes = 256 * 1024

        /**
         * 每块屏幕最多存多少字节 Kitty 图片的像素，超了先丢最老的。比宿主存得少，因为手机内存紧；这边丢了的只是画不出来，
         * 查询由宿主应答，两份 VT 不会因此分叉。
         */
        internal const val kittyImageStorageBytes: Long = 16L shl 20

        private const val TEN_SECONDS_NS = 10_000_000_000L

        /** 同步输出冻结屏幕最多算多久，免得程序出错时画面卡死。 */
        private const val HOLD_LIMIT_NS = 1_000_000_000L

        /** 改尺寸后最多等程序重画多久。 */
        private const val RESIZE_HOLD_NS = 300_000_000L

        /** 改尺寸后的输出停了这么久，算程序重画完了。 */
        private const val REDRAW_SETTLE_NS = 50_000_000L

        /** [refresh] 时缓冲区不够大、扩了容再试，最多试几次。 */
        private const val SNAPSHOT_ATTEMPTS = 3

        private val asciiStrings = Array(128) { it.toChar().toString() }

        /** 头、视口和上面一行都整行重画时要的字节数，另留些给字素簇和图片。 */
        private fun snapshotCapacity(columns: Int, rows: Int): Int =
            SnapshotLayout.HEADER_SIZE + (rows + 1) * (SnapshotLayout.ROW_HEADER_SIZE + columns * SnapshotLayout.CELL_SIZE) +
                4096

        private fun rgb(bits: Int): Rgb = Rgb((bits and 0xFFFFFF).toUInt())

        private val Rgb.packed: Int get() = (r.toInt() shl 16) or (g.toInt() shl 8) or b.toInt()

        private fun checkThread() {
            if (BuildConfig.DEBUG) {
                check(Looper.myLooper() == Looper.getMainLooper()) { "VTerminal 只能在主线程上用" }
            }
        }
    }
}

sealed class VTerminalError(message: String) : Exception(message) {
    class CreationFailed : VTerminalError("libghostty-vt 建不出终端")
}
