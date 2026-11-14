package dev.runode.mobile.terminal

import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.runode.mobile.protocol.GridSize
import dev.runode.mobile.protocol.Rgb
import dev.runode.mobile.protocol.TermSettings
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/** 喂字节后读出的屏幕、改尺寸、换主题和输入编码，都经真的 libghostty-vt。 */
@RunWith(AndroidJUnit4::class)
class VTerminalTests {
    private val size = GridSize(cols = 20u, rows = 5u, cellWidthPx = 8u, cellHeightPx = 16u)

    private fun terminal() = VTerminal(size, TermSettings.default)

    private fun VTerminal.feed(text: String) = feed(text.encodeToByteArray())

    private fun VTerminal.frame(): ScreenFrame = ScreenFrame().also { refresh(it) }

    @Test
    fun feedingBytesShowsText() = onMain {
        terminal().use { vt ->
            vt.feed("hello\r\n中文 ok")
            val lines = vt.screenLines()
            assertEquals(5, lines.size)
            assertEquals("hello", lines[0])
            assertEquals("中文 ok", lines[1])
        }
    }

    @Test
    fun wideCharactersTakeTwoCells() = onMain {
        terminal().use { vt ->
            vt.feed("中a")
            val frame = vt.frame()
            assertEquals("中", frame.cells[0][0].text)
            assertEquals(CellWidth.Wide, frame.cells[0][0].width)
            assertEquals(CellWidth.SpacerTail, frame.cells[0][1].width)
            assertEquals("a", frame.cells[0][2].text)
        }
    }

    /** 多码点的字素簇（带肤色的 emoji、组合附加符号）整个留在一个格子里，补充平面的字也读得对。 */
    @Test
    fun graphemeClustersStayInOneCell() = onMain {
        terminal().use { vt ->
            vt.feed("\u001b[?2027h👍🏽 é 😀")
            val row = vt.frame().cells[0]
            assertEquals("👍🏽", row[0].text)
            assertEquals("é", row[3].text)
            assertEquals("😀", row[5].text)
        }
    }

    @Test
    fun stylesAndColorsAreRead() = onMain {
        terminal().use { vt ->
            vt.feed("\u001b[1;3;4;31;44mX\u001b[0m\u001b[7mY")
            val frame = vt.frame()
            val x = frame.cells[0][0]
            assertTrue(CellAttributes.bold in x.attributes)
            assertTrue(CellAttributes.italic in x.attributes)
            assertEquals(UnderlineStyle.Single, x.underline)
            // 调色板 1 和 4 是默认主题里的红、蓝。
            assertEquals(Rgb(0xFF1261u), x.foreground)
            assertEquals(Rgb(0x2D9AFFu), x.background)
            assertTrue(CellAttributes.inverse in frame.cells[0][1].attributes)
            assertEquals(TermSettings.default.background, frame.background)
        }
    }

    /** 每个 SGR 属性只读出它自己那一个，下划线的几种样式各是各的。 */
    @Test
    fun eachAttributeIsReadAlone() = onMain {
        val cases = listOf(
            1 to CellAttributes.bold, 2 to CellAttributes.faint, 3 to CellAttributes.italic,
            5 to CellAttributes.blink, 7 to CellAttributes.inverse, 8 to CellAttributes.invisible,
            9 to CellAttributes.strikethrough,
        )
        for ((sgr, attribute) in cases) {
            terminal().use { vt ->
                vt.feed("\u001b[${sgr}mX")
                val cell = vt.frame().cells[0][0]
                assertEquals(attribute, cell.attributes, "SGR $sgr")
                assertEquals(UnderlineStyle.None, cell.underline, "SGR $sgr")
            }
        }
    }

    @Test
    fun underlineStylesAreRead() = onMain {
        val cases = listOf(
            "4" to UnderlineStyle.Single, "4:2" to UnderlineStyle.Double, "4:3" to UnderlineStyle.Curly,
            "4:4" to UnderlineStyle.Dotted, "4:5" to UnderlineStyle.Dashed,
        )
        for ((sgr, underline) in cases) {
            terminal().use { vt ->
                vt.feed("\u001b[${sgr}mX")
                val cell = vt.frame().cells[0][0]
                assertEquals(underline, cell.underline, "SGR $sgr")
                assertEquals(CellAttributes.none, cell.attributes, "SGR $sgr")
            }
        }
    }

    /** 不带样式的字和空白格不读颜色；只有背景色的格子（带背景色擦掉的）仍读得到背景。 */
    @Test
    fun plainAndBackgroundOnlyCellsAreRead() = onMain {
        terminal().use { vt ->
            vt.feed("ab\r\n\u001b[44m\u001b[K\u001b[0mc\u001b[48;2;1;2;3m\u001b[X")
            val frame = vt.frame()
            assertEquals(
                ScreenCell(
                    text = "a", foreground = null, background = null, attributes = CellAttributes.none,
                    underline = UnderlineStyle.None, width = CellWidth.Narrow,
                ),
                frame.cells[0][0],
            )
            assertEquals(ScreenCell.blank, frame.cells[0][2])
            assertEquals("c", frame.cells[1][0].text)
            assertNull(frame.cells[1][0].background)
            assertEquals("", frame.cells[1][1].text)
            assertEquals(Rgb(1u, 2u, 3u), frame.cells[1][1].background)
            assertEquals(Rgb(0x2D9AFFu), frame.cells[1][5].background)
            assertNull(frame.cells[1][5].foreground)
        }
    }

    @Test
    fun onlyChangedRowsAreReported() = onMain {
        terminal().use { vt ->
            val frame = ScreenFrame()
            assertTrue(vt.refresh(frame).full)
            vt.feed("\u001b[3;1Hthird")
            val change = vt.refresh(frame)
            assertFalse(change.full)
            assertTrue(2 in change.rows)
            // 光标原来所在的第一行也会标脏，最后一行没动过。
            assertFalse(4 in change.rows)
            assertEquals("third", frame.lines[2])
            // 没再喂东西时什么都没变。
            assertTrue(vt.refresh(frame).isEmpty)
        }
    }

    /** 一帧比缓冲区大时扩容重来，读出的是同一帧，不丢行。 */
    @Test
    fun largeFramesGrowTheBuffer() = onMain {
        VTerminal(GridSize(cols = 300u, rows = 120u, cellWidthPx = 8u, cellHeightPx = 16u), TermSettings.default)
            .use { vt ->
                val combining = "é".repeat(300)
                vt.feed(("$combining\r\n").repeat(119))
                val frame = ScreenFrame()
                assertTrue(vt.refresh(frame).full)
                assertEquals(120, frame.rows)
                assertEquals(combining, frame.lines[0])
                assertEquals(combining, frame.lines[118])
            }
    }

    @Test
    fun cursorFollowsOutput() = onMain {
        terminal().use { vt ->
            vt.feed("ab")
            val frame = vt.frame()
            assertEquals(2, frame.cursor?.column)
            assertEquals(0, frame.cursor?.row)
            vt.feed("\u001b[?25l")
            vt.refresh(frame)
            assertNull(frame.cursor)
        }
    }

    @Test
    fun resizeChangesTheGrid() = onMain {
        terminal().use { vt ->
            vt.resize(GridSize(cols = 30u, rows = 8u, cellWidthPx = 8u, cellHeightPx = 16u))
            val frame = vt.frame()
            assertEquals(30, frame.columns)
            assertEquals(8, frame.rows)
        }
    }

    @Test
    fun resizeHoldsRenderingUntilTheRedrawSettles() {
        val vt = valueOnMain { terminal() }
        try {
            onMain {
                vt.feed("$ ")
                assertFalse(vt.isRenderHeld)
                vt.resize(GridSize(cols = 30u, rows = 8u, cellWidthPx = 8u, cellHeightPx = 16u))
                // 程序的重画还没到。
                assertTrue(vt.isRenderHeld)
                vt.feed("\r$ ")
                assertTrue(vt.isRenderHeld)
            }
            Thread.sleep(80)
            onMain {
                assertFalse(vt.isRenderHeld)
                // 程序不重画时最多等 300 毫秒。
                vt.resize(size)
            }
            Thread.sleep(350)
            onMain { assertFalse(vt.isRenderHeld) }
        } finally {
            onMain { vt.close() }
        }
    }

    /** 程序用同步输出（mode 2026）冻结屏幕时先别画，解冻后接着画。 */
    @Test
    fun synchronizedOutputHoldsRendering() = onMain {
        terminal().use { vt ->
            vt.feed("\u001b[?2026h")
            assertTrue(vt.isRenderHeld)
            vt.feed("\u001b[?2026l")
            assertFalse(vt.isRenderHeld)
        }
    }

    @Test
    fun themeChangesDefaultColors() = onMain {
        terminal().use { vt ->
            val settings = TermSettings.default.copy(
                background = Rgb(1u, 2u, 3u),
                palette = listOf(TermSettings.PaletteEntry(1u, Rgb(9u, 9u, 9u))),
            )
            vt.applyTheme(settings)
            vt.feed("\u001b[31mR\u001b[32mG")
            val frame = vt.frame()
            assertEquals(Rgb(1u, 2u, 3u), frame.background)
            assertEquals(Rgb(9u, 9u, 9u), frame.cells[0][0].foreground)
            // 换主题前先把调色板重置回内置的：没在新主题里的 2 号色不是旧主题的绿。
            assertNotEquals(Rgb(0x2AD947u), frame.cells[0][1].foreground)
        }
    }

    @Test
    fun scrollbackKeepsHistory() = onMain {
        terminal().use { vt ->
            for (line in 1..20) vt.feed("line $line\r\n")
            assertTrue(vt.scrollbar.total > 5)
            assertTrue(vt.scrollbar.atBottom)
            vt.scrollSmoothly(3.0)
            assertFalse(vt.scrollbar.atBottom)
            // 屏幕最后是 17 到 20 行和光标所在的空行，往上滚三行后顶上是第 14 行。
            assertEquals("line 14", vt.screenLines().first())
            vt.scrollToBottom()
            assertTrue(vt.scrollbar.atBottom)
        }
    }

    /** 回滚的字节上限取宿主给的和 4 MiB 中小的那个，换主题时也一样。 */
    @Test
    fun scrollbackBytesAreCappedOnThePhone() = onMain {
        var settings = TermSettings.default
        VTerminal(size, settings).use { vt ->
            assertEquals(4L shl 20, vt.scrollbackMaxBytes)
            settings = settings.copy(scrollbackLimit = 1uL shl 20)
            vt.applyTheme(settings)
            assertEquals(1L shl 20, vt.scrollbackMaxBytes)
            settings = settings.copy(scrollbackLimit = 64uL shl 20)
            vt.applyTheme(settings)
            assertEquals(4L shl 20, vt.scrollbackMaxBytes)
        }
    }

    @Test
    fun smoothScrollKeepsTheFractionAndTheRowAbove() = onMain {
        terminal().use { vt ->
            for (line in 1..20) vt.feed("line $line\r\n")
            val frame = vt.frame()
            assertEquals(0.0, frame.scrollOffset)
            // 往回看一行半：视口挪一行，再错开半行，露出的是视口上面的第 15 行。
            assertTrue(vt.scrollSmoothly(1.5))
            val change = vt.refresh(frame)
            assertTrue(change.aboveChanged)
            assertEquals("line 16", frame.lines.first())
            assertEquals(0.5, frame.scrollOffset)
            assertEquals("line 15", ScreenFrame.text(frame.above))
            assertFalse(vt.viewportAtBottom)
            // 往下滚过头：回到底部，不会错开成负的；再往下挪不动。
            assertTrue(vt.scrollSmoothly(-3.0))
            vt.refresh(frame)
            assertEquals(0.0, frame.scrollOffset)
            assertTrue(vt.viewportAtBottom)
            assertFalse(vt.scrollSmoothly(-1.0))
            // 滚到历史最顶上：上面没有行了，也就不错开。
            vt.scrollSmoothly(100.5)
            vt.refresh(frame)
            assertTrue(frame.above.isEmpty())
            assertEquals(0.0, frame.scrollOffset)
            assertFalse(vt.scrollSmoothly(0.5))
        }
    }

    @Test
    fun wheelGoesToProgramsThatTrackTheMouse() = onMain {
        terminal().use { vt ->
            assertFalse(vt.programScrolls)
            assertTrue(vt.encodeWheel(-1, 2, 3).isEmpty())
            // 开 SGR 格式的鼠标上报：往上滚是 64 号键，往下是 65，坐标从 1 数。
            vt.feed("\u001b[?1000h\u001b[?1006h")
            assertTrue(vt.programScrolls)
            assertContentEquals("\u001b[<64;3;4M\u001b[<64;3;4M".encodeToByteArray(), vt.encodeWheel(-2, 2, 3))
            assertContentEquals("\u001b[<65;20;1M".encodeToByteArray(), vt.encodeWheel(1, 99, 0))
        }
    }

    @Test
    fun clickGoesToProgramsThatTrackTheMouse() = onMain {
        terminal().use { vt ->
            assertTrue(vt.encodeClick(2, 3).isEmpty())
            // SGR 格式：左键按下是 M 结尾，松开是 m 结尾。
            vt.feed("\u001b[?1000h\u001b[?1006h")
            assertContentEquals("\u001b[<0;3;4M\u001b[<0;3;4m".encodeToByteArray(), vt.encodeClick(2, 3))
        }
    }

    @Test
    fun wheelOnAlternateScreenSendsArrows() = onMain {
        terminal().use { vt ->
            vt.feed("\u001b[?1049h")
            assertTrue(vt.programScrolls)
            assertContentEquals("\u001b[B\u001b[B".encodeToByteArray(), vt.encodeWheel(2, 0, 0))
            // 程序关了备用滚动：滚的是 VT 自己的视口，不发东西。
            vt.feed("\u001b[?1007l")
            assertFalse(vt.programScrolls)
            assertTrue(vt.encodeWheel(-1, 0, 0).isEmpty())
        }
    }

    @Test
    fun keysFollowTerminalModes() = onMain {
        terminal().use { vt ->
            assertContentEquals("\u001b[A".encodeToByteArray(), vt.encode(KeyInput(TerminalKey.Up)))
            // 应用光标键模式（DECCKM）下方向键换成 SS3。
            vt.feed("\u001b[?1h")
            assertContentEquals("\u001bOA".encodeToByteArray(), vt.encode(KeyInput(TerminalKey.Up)))
            assertContentEquals(byteArrayOf(0x1B), vt.encode(KeyInput(TerminalKey.Escape)))
            assertContentEquals(byteArrayOf(0x0D), vt.encode(KeyInput(TerminalKey.Enter)))
            assertContentEquals(byteArrayOf(0x09), vt.encode(KeyInput(TerminalKey.Tab)))
            assertContentEquals(byteArrayOf(0x7F), vt.encode(KeyInput(TerminalKey.Backspace)))
        }
    }

    /** 硬件键盘的 Shift+回车带着 Shift 编码，和桌面一样程序分得出它和回车；开着 Kitty 键盘协议时按协议编。 */
    @Test
    fun shiftEnterFollowsTheKeyboardProtocol() = onMain {
        terminal().use { vt ->
            val shiftEnter = KeyInput(TerminalKey.Enter, modifiers = KeyModifiers.shift)
            assertFalse(vt.encode(shiftEnter).contentEquals(vt.encode(KeyInput(TerminalKey.Enter))))
            vt.feed("\u001b[>1u")
            assertContentEquals("\u001b[13;2u".encodeToByteArray(), vt.encode(shiftEnter))
            assertContentEquals(byteArrayOf(0x0D), vt.encode(KeyInput(TerminalKey.Enter)))
        }
    }

    @Test
    fun controlCharacters() = onMain {
        terminal().use { vt ->
            val ctrlC = assertNotNull(KeyInput.typing("c", KeyModifiers.control))
            assertContentEquals(byteArrayOf(0x03), vt.encode(ctrlC))
            val plain = assertNotNull(KeyInput.typing("|"))
            assertContentEquals("|".encodeToByteArray(), vt.encode(plain))
        }
    }

    @Test
    fun pasteIsBracketedWhenAsked() = onMain {
        terminal().use { vt ->
            assertContentEquals("a\rb".encodeToByteArray(), vt.encodePaste("a\nb"))
            vt.feed("\u001b[?2004h")
            assertTrue(vt.bracketedPaste)
            assertContentEquals("\u001b[200~a\nb\u001b[201~".encodeToByteArray(), vt.encodePaste("a\nb"))
        }
    }

    /** 关掉以后不能再用，重复关不出事。 */
    @Test
    fun closedTerminalsRefuseWork() = onMain {
        val vt = terminal()
        vt.close()
        vt.close()
        assertFailsWith<IllegalStateException> { vt.feed("x") }
    }

    /** debug 构建里在别的线程上用会被拦下。 */
    @Test
    fun offMainThreadUseIsCaughtInDebugBuilds() {
        if (!BuildConfig.DEBUG) return
        assertFailsWith<IllegalStateException> { terminal() }
    }
}
