package dev.runode.mobile.terminal

import android.view.KeyEvent
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import org.junit.Test

/** 粘住的 Ctrl、硬件键盘的按键和拖动手势怎么变成终端的输入，不经 VT。 */
class TerminalInputRulesTests {
    private fun key(input: TerminalInput): KeyInput = (input as TerminalInput.Key).input

    @Test
    fun plainTextIsSentAsOneRun() {
        val typed = ControlLatch.type("ls -la 中文", latched = false)
        assertEquals(listOf<TerminalInput>(TerminalInput.Text("ls -la 中文")), typed.inputs)
        assertFalse(typed.latched)
    }

    @Test
    fun newlinesBecomeTheEnterKey() {
        val typed = ControlLatch.type("a\nb\r\nc", latched = false)
        assertEquals(
            listOf(
                TerminalInput.Text("a"), TerminalInput.Key(KeyInput(key = TerminalKey.Enter)), TerminalInput.Text("b"),
                TerminalInput.Key(KeyInput(key = TerminalKey.Enter)), TerminalInput.Text("c"),
            ),
            typed.inputs,
        )
    }

    /** 粘住 Ctrl 后只有第一个字带 Ctrl，用过就松开；大写字母也按小写的键发。 */
    @Test
    fun theLatchedControlAppliesToTheFirstCharacterOnly() {
        val typed = ControlLatch.type("Cd", latched = true)
        assertFalse(typed.latched)
        assertEquals(2, typed.inputs.size)
        val first = key(typed.inputs[0])
        assertEquals(TerminalKey.Character('c'), first.key)
        assertEquals(KeyModifiers.control, first.modifiers)
        assertEquals(TerminalInput.Text("d"), typed.inputs[1])
    }

    /** 回车不消耗粘住的 Ctrl；美式键盘上没有的字照样打出去，Ctrl 也松开。 */
    @Test
    fun theLatchSurvivesEnterButNotUnknownCharacters() {
        val enter = ControlLatch.type("\n", latched = true)
        assertTrue(enter.latched)
        assertEquals(KeyModifiers.none, key(enter.inputs.single()).modifiers)

        val chinese = ControlLatch.type("中", latched = true)
        assertFalse(chinese.latched)
        assertEquals(listOf<TerminalInput>(TerminalInput.Text("中")), chinese.inputs)
    }

    @Test
    fun pressingAKeyAddsTheLatchedControl() {
        val up = KeyInput(key = TerminalKey.Up, modifiers = KeyModifiers.shift)
        assertEquals(KeyModifiers.shift + KeyModifiers.control, ControlLatch.press(up, latched = true).modifiers)
        assertEquals(up, ControlLatch.press(up, latched = false))
    }

    /** 美式键盘布局：不按修饰键时打出小写字母，按着 Shift 时打出大写。 */
    private fun letter(c: Char): (Int) -> Int = { meta ->
        if (meta and KeyEvent.META_SHIFT_MASK != 0) c.uppercaseChar().code else c.code
    }

    @Test
    fun controlLettersAreEncodedByKey() {
        val input = assertNotNull(HardwareKeys.keyInput(KeyEvent.KEYCODE_C, KeyEvent.META_CTRL_ON, letter('c')))
        assertEquals(TerminalKey.Character('c'), input.key)
        assertEquals(KeyModifiers.control, input.modifiers)
        assertEquals("c", input.text)
        assertEquals('c', input.unshifted)
        assertEquals(KeyModifiers.none, input.consumedModifiers)
    }

    /** 普通打字交给输入法和文字那条路，不在这里编码；按着 Meta 的留给系统。 */
    @Test
    fun plainTypingAndMetaAreLeftAlone() {
        assertNull(HardwareKeys.keyInput(KeyEvent.KEYCODE_A, 0, letter('a')))
        assertNull(HardwareKeys.keyInput(KeyEvent.KEYCODE_A, KeyEvent.META_SHIFT_ON, letter('a')))
        assertNull(HardwareKeys.keyInput(KeyEvent.KEYCODE_ENTER, 0) { 0 })
        assertNull(HardwareKeys.keyInput(KeyEvent.KEYCODE_C, KeyEvent.META_META_ON or KeyEvent.META_CTRL_ON, letter('c')))
    }

    @Test
    fun specialKeysCarryTheirModifiers() {
        val up = assertNotNull(HardwareKeys.keyInput(KeyEvent.KEYCODE_DPAD_UP, KeyEvent.META_SHIFT_ON) { 0 })
        assertEquals(KeyInput(key = TerminalKey.Up, modifiers = KeyModifiers.shift), up)
        assertEquals(TerminalKey.Escape, HardwareKeys.keyInput(KeyEvent.KEYCODE_ESCAPE, 0) { 0 }?.key)
        assertEquals(TerminalKey.Tab, HardwareKeys.keyInput(KeyEvent.KEYCODE_TAB, 0) { 0 }?.key)
        assertEquals(TerminalKey.Function(5), HardwareKeys.keyInput(KeyEvent.KEYCODE_F5, 0) { 0 }?.key)
        assertEquals(TerminalKey.Delete, HardwareKeys.keyInput(KeyEvent.KEYCODE_FORWARD_DEL, 0) { 0 }?.key)
        val shiftEnter = assertNotNull(HardwareKeys.keyInput(KeyEvent.KEYCODE_ENTER, KeyEvent.META_SHIFT_ON) { 0 })
        assertEquals(KeyInput(key = TerminalKey.Enter, modifiers = KeyModifiers.shift), shiftEnter)
        val ctrlBackspace = assertNotNull(HardwareKeys.keyInput(KeyEvent.KEYCODE_DEL, KeyEvent.META_CTRL_ON) { 0 })
        assertEquals(TerminalKey.Backspace, ctrlBackspace.key)
    }

    @Test
    fun onlyTheRightAltCountsAsRightAlt() {
        val right = KeyEvent.META_ALT_ON or KeyEvent.META_ALT_RIGHT_ON
        assertTrue(assertNotNull(HardwareKeys.keyInput(KeyEvent.KEYCODE_X, right, letter('x'))).rightAlt)
        val both = right or KeyEvent.META_ALT_LEFT_ON
        val input = assertNotNull(HardwareKeys.keyInput(KeyEvent.KEYCODE_X, both, letter('x')))
        assertFalse(input.rightAlt)
        assertEquals(KeyModifiers.alt, input.modifiers)
    }

    /** Alt 打出了别的字时，Alt 算已经用在文字里了。 */
    @Test
    fun altThatChangesTheCharacterIsConsumed() {
        val accent: (Int) -> Int = { meta -> if (meta and KeyEvent.META_ALT_MASK != 0) 'é'.code else 'e'.code }
        val input = assertNotNull(HardwareKeys.keyInput(KeyEvent.KEYCODE_E, KeyEvent.META_ALT_ON or KeyEvent.META_ALT_LEFT_ON, accent))
        assertEquals("é", input.text)
        assertEquals(KeyModifiers.alt, input.consumedModifiers)
    }

    @Test
    fun pasteShortcuts() {
        assertTrue(HardwareKeys.isPaste(KeyEvent.KEYCODE_V, KeyEvent.META_META_ON))
        assertTrue(HardwareKeys.isPaste(KeyEvent.KEYCODE_V, KeyEvent.META_CTRL_ON or KeyEvent.META_SHIFT_ON))
        assertFalse(HardwareKeys.isPaste(KeyEvent.KEYCODE_V, KeyEvent.META_CTRL_ON))
        assertFalse(HardwareKeys.isPaste(KeyEvent.KEYCODE_C, KeyEvent.META_META_ON))
    }

    /** 程序自己管滚动时攒够一行才出一行，零头留到下一次，往回拖时抵掉。 */
    @Test
    fun wheelRowsAccumulateWholeRows() {
        val wheel = WheelAccumulator()
        assertEquals(0, wheel.rows(15f, 20f))
        assertEquals(1, wheel.rows(10f, 20f))
        assertEquals(2, wheel.rows(40f, 20f))
        assertEquals(0, wheel.rows(-10f, 20f))
        assertEquals(-1, wheel.rows(-15f, 20f))
        wheel.reset()
        assertEquals(0, wheel.rows(19f, 20f))
        assertEquals(0, wheel.rows(100f, 0f))
    }

    @Test
    fun momentumSlowsDownOverTime() {
        assertEquals(1000f, Momentum.decayed(1000f, 0f))
        val later = Momentum.decayed(1000f, 0.5f)
        assertTrue(later < 1000f && later > 0f)
        assertEquals(Momentum.decayed(later, 0.5f), Momentum.decayed(1000f, 1f), 0.01f)
        assertTrue(Momentum.decayed(-1000f, 0.5f) < 0f)
    }
}
