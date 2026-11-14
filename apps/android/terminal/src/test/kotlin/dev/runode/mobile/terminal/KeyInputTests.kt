package dev.runode.mobile.terminal

import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import org.junit.Test

/** 打出的字符换成美式键盘上的键，键和修饰键换成 libghostty 的取值。 */
class KeyInputTests {
    @Test
    fun typedCharactersMapToUsKeys() {
        assertEquals(TerminalKey.Typed(TerminalKey.Space, false), TerminalKey.forTyped(" "))
        assertEquals(TerminalKey.Typed(TerminalKey.Character('a'), false), TerminalKey.forTyped("a"))
        assertEquals(TerminalKey.Typed(TerminalKey.Character('a'), true), TerminalKey.forTyped("A"))
        assertEquals(TerminalKey.Typed(TerminalKey.Character('7'), false), TerminalKey.forTyped("7"))
        assertEquals(TerminalKey.Typed(TerminalKey.Character('1'), true), TerminalKey.forTyped("!"))
        assertEquals(TerminalKey.Typed(TerminalKey.Character('\\'), true), TerminalKey.forTyped("|"))
        assertEquals(TerminalKey.Typed(TerminalKey.Character('`'), true), TerminalKey.forTyped("~"))
        assertEquals(TerminalKey.Typed(TerminalKey.Character(';'), false), TerminalKey.forTyped(";"))
        // 美式键盘上没有的字、多个字符都认不得。
        assertNull(TerminalKey.forTyped("中"))
        assertNull(TerminalKey.forTyped("é"))
        assertNull(TerminalKey.forTyped("ab"))
        assertNull(TerminalKey.forTyped("👍"))
    }

    @Test
    fun typingAddsShiftWhenTheCharacterNeedsIt() {
        val upper = assertNotNull(KeyInput.typing("A", KeyModifiers.control))
        assertEquals(TerminalKey.Character('a'), upper.key)
        assertEquals(KeyModifiers.control + KeyModifiers.shift, upper.modifiers)
        assertEquals(KeyModifiers.shift, upper.consumedModifiers)
        assertEquals("A", upper.text)
        assertEquals('a', upper.unshifted)
        val space = assertNotNull(KeyInput.typing(" "))
        assertEquals(' ', space.unshifted)
        assertEquals(KeyModifiers.none, space.consumedModifiers)
        assertNull(KeyInput.typing("中"))
    }

    @Test
    fun keysMapToGhosttyKeys() {
        assertEquals(GhosttyAbi.KEY_A, TerminalKey.Character('a').ghosttyKey)
        assertEquals(GhosttyAbi.KEY_A + 25, TerminalKey.Character('z').ghosttyKey)
        assertEquals(GhosttyAbi.KEY_DIGIT_0 + 5, TerminalKey.Character('5').ghosttyKey)
        assertEquals(GhosttyAbi.KEY_SLASH, TerminalKey.Character('/').ghosttyKey)
        assertEquals(GhosttyAbi.KEY_UNIDENTIFIED, TerminalKey.Character('A').ghosttyKey)
        assertEquals(GhosttyAbi.KEY_F1 + 11, TerminalKey.Function(12).ghosttyKey)
        assertEquals(GhosttyAbi.KEY_UNIDENTIFIED, TerminalKey.Function(25).ghosttyKey)
        assertEquals(GhosttyAbi.KEY_ARROW_UP, TerminalKey.Up.ghosttyKey)
        assertEquals(GhosttyAbi.KEY_PAGE_DOWN, TerminalKey.PageDown.ghosttyKey)
    }

    /** 右边的 Alt 只在按着 Alt 时另置右边的位。 */
    @Test
    fun modifiersMapToGhosttyMods() {
        val all = KeyModifiers.shift + KeyModifiers.control + KeyModifiers.alt + KeyModifiers.command
        assertEquals(
            GhosttyAbi.MODS_SHIFT or GhosttyAbi.MODS_CTRL or GhosttyAbi.MODS_ALT or GhosttyAbi.MODS_SUPER,
            all.ghosttyMods(rightAlt = false),
        )
        assertEquals(GhosttyAbi.MODS_ALT or GhosttyAbi.MODS_ALT_SIDE, KeyModifiers.alt.ghosttyMods(rightAlt = true))
        assertEquals(GhosttyAbi.MODS_SHIFT, KeyModifiers.shift.ghosttyMods(rightAlt = true))
        assertEquals(KeyModifiers.control, (KeyModifiers.control + KeyModifiers.alt) - KeyModifiers.alt)
    }
}
