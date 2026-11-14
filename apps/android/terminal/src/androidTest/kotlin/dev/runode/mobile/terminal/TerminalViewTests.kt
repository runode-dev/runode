package dev.runode.mobile.terminal

import android.graphics.Bitmap
import android.graphics.Canvas
import android.os.SystemClock
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.View
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import dev.runode.mobile.protocol.GridSize
import dev.runode.mobile.protocol.TermSettings
import java.io.ByteArrayOutputStream
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/** 终端视图怎么报尺寸、接输入法和硬件键盘、粘贴和手势；要编码的输入经真的 libghostty-vt。 */
@RunWith(AndroidJUnit4::class)
class TerminalViewTests {
    private class Recorder : TerminalViewDelegate {
        val fitSizes = mutableListOf<GridSize>()
        val inputs = mutableListOf<TerminalInput>()

        override fun didInput(view: TerminalView, input: TerminalInput) {
            inputs += input
        }

        override fun fitSizeDidChange(view: TerminalView, size: GridSize) {
            fitSizes += size
        }

        override fun didScrollBack(view: TerminalView, scrolledBack: Boolean) = Unit

        override fun keyboardVisible(view: TerminalView, visible: Boolean) = Unit

        override fun dictation(view: TerminalView, press: DictationPress) = Unit
    }

    private val context = InstrumentationRegistry.getInstrumentation().targetContext
    private val size = GridSize(cols = 20u, rows = 5u, cellWidthPx = 8u, cellHeightPx = 16u)

    private fun view(recorder: Recorder? = null): TerminalView =
        TerminalView(context).also { it.delegate = recorder }

    private fun View.layoutAt(width: Int, height: Int) {
        measure(
            View.MeasureSpec.makeMeasureSpec(width, View.MeasureSpec.EXACTLY),
            View.MeasureSpec.makeMeasureSpec(height, View.MeasureSpec.EXACTLY),
        )
        layout(0, 0, width, height)
    }

    private fun TerminalView.inputConnection(): InputConnection = assertNotNull(onCreateInputConnection(EditorInfo()))

    /** 视图模型发给宿主的字节：和它一样按 VT 当前的模式编码。 */
    private fun VTerminal.bytes(inputs: List<TerminalInput>): ByteArray {
        val out = ByteArrayOutputStream()
        for (input in inputs) {
            out.write(
                when (input) {
                    is TerminalInput.Key -> encode(input.input)
                    is TerminalInput.Text -> input.text.encodeToByteArray()
                    is TerminalInput.Paste -> encodePaste(input.text)
                    is TerminalInput.Wheel -> encodeWheel(input.lines, input.column, input.row)
                    is TerminalInput.Click -> encodeClick(input.column, input.row)
                },
            )
        }
        return out.toByteArray()
    }

    /** 键盘动的时候视图的高度一下变好几次，只在停稳以后按最后的大小报一次。 */
    @Test
    fun keyboardMotionReportsTheFitSizeOnceItSettles() = onMain {
        val recorder = Recorder()
        val view = view(recorder)
        view.layoutAt(1080, 1800)
        assertEquals(1, recorder.fitSizes.size)

        view.keyboardWillMove(TerminalView.KeyboardMotion.Opening)
        for (height in listOf(1900, 1200, 1100)) view.layoutAt(1080, height)
        assertEquals(1, recorder.fitSizes.size)
        view.keyboardDidMove()
        assertEquals(2, recorder.fitSizes.size)
        assertEquals(view.fitSize, recorder.fitSizes.last())
    }

    /**
     * 输入法设的选区超出了组字文字：不理它，不越界。组字时删前面的字删不到已经发出去的，也不发退格；组字文字由输入法
     * 自己改。
     */
    @Test
    fun aStaleSelectionLeavesTheCompositionAlone() = onMain {
        val recorder = Recorder()
        val view = view(recorder)
        val connection = view.inputConnection()
        connection.setComposingText("ab", 1)
        connection.setSelection(1, 5)
        assertEquals("ab", connection.getTextBeforeCursor(10, 0))
        connection.deleteSurroundingText(1, 0)
        assertEquals("ab", view.markedText)
        connection.setComposingText("a", 1)
        assertEquals("a", view.markedText)
        assertTrue(recorder.inputs.isEmpty())
    }

    /** 组字到一半换了会话：组字中的文字丢掉，之后输入法再交出来也不发给新的会话。 */
    @Test
    fun switchingSessionsDiscardsTheComposition() = onMain {
        val recorder = Recorder()
        val view = view(recorder)
        val connection = view.inputConnection()
        connection.setComposingText("ni", 1)
        view.terminalDidReset(null, TermSettings.default)
        assertTrue(view.markedText.isEmpty())
        connection.finishComposingText()
        assertTrue(recorder.inputs.isEmpty())
    }

    /** 组字中的文字不发出去，上屏后整段按 UTF-8 发；不组字时退格发退格键。 */
    @Test
    fun committedCompositionIsSentOnce() = onMain {
        VTerminal(size, TermSettings.default).use { vt ->
            val recorder = Recorder()
            val view = view(recorder)
            val connection = view.inputConnection()
            connection.setComposingText("ni", 1)
            connection.setComposingText("nih", 1)
            assertEquals("nih", view.markedText)
            assertTrue(recorder.inputs.isEmpty())
            connection.commitText("你好", 1)
            assertEquals("", view.markedText)
            assertEquals(0, view.document.length)
            assertContentEquals("你好".encodeToByteArray(), vt.bytes(recorder.inputs))

            recorder.inputs.clear()
            connection.commitText("\n", 1)
            connection.deleteSurroundingText(1, 0)
            assertContentEquals(byteArrayOf(0x0D, 0x7F), vt.bytes(recorder.inputs))
        }
    }

    @Test
    fun hardwareControlCSends0x03() = onMain {
        VTerminal(size, TermSettings.default).use { vt ->
            val recorder = Recorder()
            val view = view(recorder)
            val meta = KeyEvent.META_CTRL_ON or KeyEvent.META_CTRL_LEFT_ON
            val event = KeyEvent(0, 0, KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_C, 0, meta)
            assertTrue(view.onKeyDown(KeyEvent.KEYCODE_C, event))
            assertContentEquals(byteArrayOf(0x03), vt.bytes(recorder.inputs))

            // 不按修饰键的字当文字打。
            recorder.inputs.clear()
            view.onKeyDown(KeyEvent.KEYCODE_A, KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_A))
            assertContentEquals("a".encodeToByteArray(), vt.bytes(recorder.inputs))
        }
    }

    /** 按键栏上粘住的 Ctrl 加在下一个打的字上，用过就松开。 */
    @Test
    fun latchedControlAppliesToTheNextCharacter() = onMain {
        VTerminal(size, TermSettings.default).use { vt ->
            val recorder = Recorder()
            val view = view(recorder)
            view.controlLatched = true
            view.typeText("cd")
            assertContentEquals(byteArrayOf(0x03, 'd'.code.toByte()), vt.bytes(recorder.inputs))
            assertTrue(!view.controlLatched)
        }
    }

    @Test
    fun pasteFollowsBracketedPasteMode() = onMain {
        VTerminal(size, TermSettings.default).use { vt ->
            val recorder = Recorder()
            val view = view(recorder)
            view.paste("a\nb")
            assertContentEquals("a\rb".encodeToByteArray(), vt.bytes(recorder.inputs))

            recorder.inputs.clear()
            vt.feed("\u001b[?2004h".encodeToByteArray())
            view.paste("a\nb")
            assertContentEquals("\u001b[200~a\nb\u001b[201~".encodeToByteArray(), vt.bytes(recorder.inputs))
        }
    }

    /** 适配手机时两指张开不缩放：抬手后不报新的网格尺寸。 */
    @Test
    fun pinchingDoesNotZoomWhenFittingThePhone() = onMain {
        val recorder = Recorder()
        val view = view(recorder)
        view.terminalSizeModeDidChange(fitsPhone = true)
        view.layoutAt(1080, 1800)
        val before = recorder.fitSizes.last()
        val down = SystemClock.uptimeMillis()
        val center = 540f
        fun event(time: Long, action: Int, vararg xs: Float): MotionEvent {
            val properties = Array(xs.size) { index ->
                MotionEvent.PointerProperties().apply {
                    id = index
                    toolType = MotionEvent.TOOL_TYPE_FINGER
                }
            }
            val coords = Array(xs.size) { index ->
                MotionEvent.PointerCoords().apply {
                    x = xs[index]
                    y = 900f
                    pressure = 1f
                    this.size = 1f
                }
            }
            return MotionEvent.obtain(
                down, time, action, xs.size, properties, coords, 0, 0, 1f, 1f, 0, 0, InputDevice.SOURCE_TOUCHSCREEN, 0,
            )
        }
        val pointerDown = MotionEvent.ACTION_POINTER_DOWN or (1 shl MotionEvent.ACTION_POINTER_INDEX_SHIFT)
        val pointerUp = MotionEvent.ACTION_POINTER_UP or (1 shl MotionEvent.ACTION_POINTER_INDEX_SHIFT)
        view.dispatchTouchEvent(event(down, MotionEvent.ACTION_DOWN, center - 300))
        view.dispatchTouchEvent(event(down + 10, pointerDown, center - 300, center + 300))
        for (step in 1..10) {
            val spread = 300f + step * 20f
            view.dispatchTouchEvent(event(down + 10 + step * 16L, MotionEvent.ACTION_MOVE, center - spread, center + spread))
        }
        view.dispatchTouchEvent(event(down + 200, pointerUp, center - 500, center + 500))
        view.dispatchTouchEvent(event(down + 210, MotionEvent.ACTION_UP, center - 500))
        assertEquals(listOf(before), recorder.fitSizes)
        assertEquals(before, view.fitSize)
    }

    /** 备用屏上程序不管鼠标时，往下拖看更早的内容换成往上的方向键，攒够一行发一个。 */
    @Test
    fun draggingOnTheAlternateScreenSendsArrows() = onMain {
        VTerminal(size, TermSettings.default).use { vt ->
            val recorder = Recorder()
            val view = view(recorder)
            view.terminalSizeModeDidChange(fitsPhone = true)
            view.terminalDidReset(vt, TermSettings.default)
            vt.feed("\u001b[?1049h".encodeToByteArray())
            view.layoutAt(1080, 1800)
            val rowHeight = view.fitSize.cellHeightPx.toFloat()
            val down = SystemClock.uptimeMillis()
            fun touch(time: Long, action: Int, y: Float) =
                MotionEvent.obtain(down, time, action, 500f, y, 0).also { it.source = InputDevice.SOURCE_TOUCHSCREEN }
            view.dispatchTouchEvent(touch(down, MotionEvent.ACTION_DOWN, 300f))
            // 慢慢拖，松手时不带惯性。
            val distance = rowHeight * 3.5f
            for (step in 1..10) {
                view.dispatchTouchEvent(touch(down + step * 100L, MotionEvent.ACTION_MOVE, 300f + distance * step / 10))
            }
            view.dispatchTouchEvent(touch(down + 1_400, MotionEvent.ACTION_MOVE, 300f + distance))
            view.dispatchTouchEvent(touch(down + 1_500, MotionEvent.ACTION_UP, 300f + distance))
            assertTrue(recorder.inputs.all { it is TerminalInput.Wheel })
            assertContentEquals("\u001b[A\u001b[A\u001b[A".encodeToByteArray(), vt.bytes(recorder.inputs))
        }
    }

    /** 软件 Canvas 上逐行现画：设了背景色的格子画成那个颜色，其余是终端背景。 */
    @Test
    fun cellsAreDrawnWithTheirColors() = onMain {
        VTerminal(size, TermSettings.default).use { vt ->
            val view = view()
            view.terminalSizeModeDidChange(fitsPhone = true)
            vt.feed("\u001b[48;2;255;0;0m  \u001b[0m".encodeToByteArray())
            view.terminalDidReset(vt, TermSettings.default)
            view.layoutAt(600, 400)
            val bitmap = Bitmap.createBitmap(600, 400, Bitmap.Config.ARGB_8888)
            view.draw(Canvas(bitmap))
            // 网格贴着顶边（没有回滚历史，在等宿主改尺寸），左右居中；格宽取整了，取样点放在格子中间。
            val cellWidth = view.fitSize.cellWidthPx.toInt()
            val middle = view.fitSize.cellHeightPx.toInt() / 2
            val left = (600 - 20 * cellWidth) / 2
            assertEquals(0xFFFF0000.toInt(), bitmap.getPixel(left + cellWidth, middle))
            val background = TerminalRenderer.argb(TermSettings.default.background)
            assertEquals(background, bitmap.getPixel(left + cellWidth * 5, middle))
        }
    }

    /**
     * 适配手机、600 × 400 的视图里接上 [vt]：20 列的网格贴着顶边（没有回滚历史，在等宿主改尺寸），左右居中。格宽取整
     * 了，取的点都在格子中间。
     */
    private class Grid(val left: Float, val cellWidth: Float, val cellHeight: Float) {
        fun x(column: Int): Float = left + cellWidth * (column + 0.5f)

        fun y(row: Int): Float = cellHeight * (row + 0.5f)
    }

    /** 屏幕上是 [text] 的 VT 接在视图上跑 [body]；跑完先把 VT 从视图上摘下来再关，排着的刷新不碰关掉的 VT。 */
    private fun selectingView(
        text: String, recorder: Recorder = Recorder(), body: (vt: VTerminal, view: TerminalView, grid: Grid) -> Unit,
    ) {
        VTerminal(size, TermSettings.default).use { vt ->
            val view = view(recorder)
            view.terminalSizeModeDidChange(fitsPhone = true)
            vt.feed(text.encodeToByteArray())
            view.terminalDidReset(vt, TermSettings.default)
            view.layoutAt(600, 400)
            val cellWidth = view.fitSize.cellWidthPx.toFloat()
            try {
                body(vt, view, Grid((600 - 20 * cellWidth) / 2, cellWidth, view.fitSize.cellHeightPx.toFloat()))
            } finally {
                view.terminalDidReset(null, TermSettings.default)
            }
        }
    }

    private fun touch(down: Long, time: Long, action: Int, x: Float, y: Float): MotionEvent =
        MotionEvent.obtain(down, time, action, x, y, 0).also { it.source = InputDevice.SOURCE_TOUCHSCREEN }

    /** 长按选中手指下的词；按住终点柄往右拖六格，选区跟着变长。 */
    @Test
    fun draggingTheEndHandleExtendsTheSelection() = onMain {
        selectingView("hello world foo") { vt, view, grid ->
            assertTrue(view.beginSelection(grid.x(1), grid.y(0)))
            assertEquals("hello", vt.selectionText())
            val handle = assertNotNull(view.handleCenter(SelectionEnd.End))
            val down = SystemClock.uptimeMillis()
            view.dispatchTouchEvent(touch(down, down, MotionEvent.ACTION_DOWN, handle.x, handle.y))
            for (step in 1..6) {
                val x = handle.x + grid.cellWidth * step
                view.dispatchTouchEvent(touch(down, down + step * 16L, MotionEvent.ACTION_MOVE, x, handle.y))
            }
            view.dispatchTouchEvent(touch(down, down + 200, MotionEvent.ACTION_UP, handle.x + grid.cellWidth * 6, handle.y))
            assertEquals("hello world", vt.selectionText())
        }
    }

    /** 长按在空格上不算取到词：不留选区。 */
    @Test
    fun longPressOnBlankSelectsNothing() = onMain {
        selectingView("hello world") { vt, view, grid ->
            assertTrue(!view.beginSelection(grid.x(5), grid.y(0)))
            assertTrue(!view.beginSelection(grid.x(15), grid.y(0)))
            assertTrue(!vt.hasSelection)
        }
    }

    /** 选中时打字照常发出去，选区退出。 */
    @Test
    fun typingEndsTheSelection() = onMain {
        val recorder = Recorder()
        selectingView("hello world", recorder) { vt, view, grid ->
            assertTrue(view.beginSelection(grid.x(1), grid.y(0)))
            view.typeText("a")
            assertTrue(!vt.hasSelection)
            assertNull(view.handleCenter(SelectionEnd.End))
            assertContentEquals("a".encodeToByteArray(), vt.bytes(recorder.inputs))
        }
    }

    /** 选中时程序开着鼠标上报：点在选区里留着选区，点在外面退出选中，两下都不发点击给程序。 */
    @Test
    fun tappingOutsideEndsTheSelectionWithoutClicking() = onMain {
        val recorder = Recorder()
        selectingView("\u001b[?1000hselection world", recorder) { vt, view, grid ->
            assertTrue(view.beginSelection(grid.x(1), grid.y(0)))
            fun tap(x: Float, y: Float) {
                val down = SystemClock.uptimeMillis()
                view.dispatchTouchEvent(touch(down, down, MotionEvent.ACTION_DOWN, x, y))
                view.dispatchTouchEvent(touch(down, down + 50, MotionEvent.ACTION_UP, x, y))
            }
            tap(grid.x(4), grid.y(0))
            assertTrue(vt.hasSelection)
            tap(grid.x(1), grid.y(3))
            assertTrue(!vt.hasSelection)
            assertTrue(recorder.inputs.isEmpty())
        }
    }

    /** 拷贝把选中的文字写进剪贴板，退出选中。 */
    @Test
    fun copyWritesTheClipboardAndEndsTheSelection() = onMain {
        selectingView("hello world") { vt, view, grid ->
            var copied: String? = null
            view.writeClipboard = { copied = it }
            assertTrue(view.beginSelection(grid.x(7), grid.y(0)))
            view.copySelection()
            assertEquals("world", copied)
            assertTrue(!vt.hasSelection)
        }
    }

    /** 选中的格子画成选区底色（深色背景上没配颜色时的默认色），没选中的照旧是终端背景。 */
    @Test
    fun selectedCellsAreDrawnWithTheSelectionColor() = onMain {
        selectingView("hello world") { _, view, grid ->
            assertTrue(view.beginSelection(grid.x(1), grid.y(0)))
            val bitmap = Bitmap.createBitmap(600, 400, Bitmap.Config.ARGB_8888)
            view.draw(Canvas(bitmap))
            assertEquals(TerminalRenderer.argb(SelectionGeometry.ON_DARK), bitmap.getPixel(grid.x(2).toInt(), 1))
            val background = TerminalRenderer.argb(TermSettings.default.background)
            assertEquals(background, bitmap.getPixel(grid.x(7).toInt(), 1))
        }
    }

    /** `⏺` 系统字体里只有彩色 emoji 字形，按前景色画成实心圆：格子中心是前景色，格子的角上是背景。 */
    @Test
    fun recordSymbolIsDrawnInTheForegroundColor() = onMain {
        VTerminal(size, TermSettings.default).use { vt ->
            val view = view()
            view.terminalSizeModeDidChange(fitsPhone = true)
            vt.feed("\u001b[38;2;0;255;0m⏺\u001b[0m".encodeToByteArray())
            view.terminalDidReset(vt, TermSettings.default)
            view.layoutAt(600, 400)
            val bitmap = Bitmap.createBitmap(600, 400, Bitmap.Config.ARGB_8888)
            view.draw(Canvas(bitmap))
            val cellWidth = view.fitSize.cellWidthPx.toInt()
            val cellHeight = view.fitSize.cellHeightPx.toInt()
            val left = (600 - 20 * cellWidth) / 2
            assertEquals(0xFF00FF00.toInt(), bitmap.getPixel(left + cellWidth / 2, cellHeight / 2))
            val background = TerminalRenderer.argb(TermSettings.default.background)
            assertEquals(background, bitmap.getPixel(left + 1, 1))
        }
    }
}
