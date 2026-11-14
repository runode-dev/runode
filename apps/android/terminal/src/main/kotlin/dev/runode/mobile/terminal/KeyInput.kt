package dev.runode.mobile.terminal

@JvmInline
value class KeyModifiers(val rawValue: Int) {
    operator fun contains(other: KeyModifiers): Boolean = rawValue and other.rawValue == other.rawValue

    operator fun plus(other: KeyModifiers): KeyModifiers = KeyModifiers(rawValue or other.rawValue)

    operator fun minus(other: KeyModifiers): KeyModifiers = KeyModifiers(rawValue and other.rawValue.inv())

    override fun toString(): String = "KeyModifiers(0x${rawValue.toString(16)})"

    /** 转成 libghostty 的 `GhosttyMods`，alt 在右边时另置右边的位。 */
    internal fun ghosttyMods(rightAlt: Boolean): Int {
        var mods = 0
        if (shift in this) mods = mods or GhosttyAbi.MODS_SHIFT
        if (control in this) mods = mods or GhosttyAbi.MODS_CTRL
        if (alt in this) {
            mods = mods or GhosttyAbi.MODS_ALT
            if (rightAlt) mods = mods or GhosttyAbi.MODS_ALT_SIDE
        }
        if (command in this) mods = mods or GhosttyAbi.MODS_SUPER
        return mods
    }

    companion object {
        val none = KeyModifiers(0)
        val shift = KeyModifiers(1 shl 0)
        val control = KeyModifiers(1 shl 1)
        val alt = KeyModifiers(1 shl 2)
        val command = KeyModifiers(1 shl 3)
    }
}

/** 按下的是哪个键，按美式键盘的物理位置算（和 W3C 的 `code` 一样）。 */
sealed class TerminalKey {
    /** 字母、数字和符号键；带的字符是这个键不按 Shift 时的字符（`a`、`1`、`-` 等）。 */
    data class Character(val character: Char) : TerminalKey()

    data object Space : TerminalKey()

    data object Enter : TerminalKey()

    data object Tab : TerminalKey()

    data object Backspace : TerminalKey()

    data object Escape : TerminalKey()

    data object Insert : TerminalKey()

    data object Delete : TerminalKey()

    data object Home : TerminalKey()

    data object End : TerminalKey()

    data object PageUp : TerminalKey()

    data object PageDown : TerminalKey()

    data object Up : TerminalKey()

    data object Down : TerminalKey()

    data object Left : TerminalKey()

    data object Right : TerminalKey()

    /** F1 到 F24。 */
    data class Function(val number: Int) : TerminalKey()

    /** 美式键盘上打出的键和要不要 Shift，[TerminalKey.forTyped] 的结果。 */
    data class Typed(val key: TerminalKey, val shift: Boolean)

    internal val ghosttyKey: Int
        get() = when (this) {
            Space -> GhosttyAbi.KEY_SPACE
            Enter -> GhosttyAbi.KEY_ENTER
            Tab -> GhosttyAbi.KEY_TAB
            Backspace -> GhosttyAbi.KEY_BACKSPACE
            Escape -> GhosttyAbi.KEY_ESCAPE
            Insert -> GhosttyAbi.KEY_INSERT
            Delete -> GhosttyAbi.KEY_DELETE
            Home -> GhosttyAbi.KEY_HOME
            End -> GhosttyAbi.KEY_END
            PageUp -> GhosttyAbi.KEY_PAGE_UP
            PageDown -> GhosttyAbi.KEY_PAGE_DOWN
            Up -> GhosttyAbi.KEY_ARROW_UP
            Down -> GhosttyAbi.KEY_ARROW_DOWN
            Left -> GhosttyAbi.KEY_ARROW_LEFT
            Right -> GhosttyAbi.KEY_ARROW_RIGHT
            is Function -> if (number in 1..24) GhosttyAbi.KEY_F1 + number - 1 else GhosttyAbi.KEY_UNIDENTIFIED
            is Character -> when (character) {
                in 'a'..'z' -> GhosttyAbi.KEY_A + (character - 'a')
                in '0'..'9' -> GhosttyAbi.KEY_DIGIT_0 + (character - '0')
                '`' -> GhosttyAbi.KEY_BACKQUOTE
                '\\' -> GhosttyAbi.KEY_BACKSLASH
                '[' -> GhosttyAbi.KEY_BRACKET_LEFT
                ']' -> GhosttyAbi.KEY_BRACKET_RIGHT
                ',' -> GhosttyAbi.KEY_COMMA
                '=' -> GhosttyAbi.KEY_EQUAL
                '-' -> GhosttyAbi.KEY_MINUS
                '.' -> GhosttyAbi.KEY_PERIOD
                '\'' -> GhosttyAbi.KEY_QUOTE
                ';' -> GhosttyAbi.KEY_SEMICOLON
                '/' -> GhosttyAbi.KEY_SLASH
                else -> GhosttyAbi.KEY_UNIDENTIFIED
            }
        }

    companion object {
        private val unshifted = mapOf(
            '!' to '1', '@' to '2', '#' to '3', '$' to '4', '%' to '5', '^' to '6', '&' to '7', '*' to '8',
            '(' to '9', ')' to '0', '_' to '-', '+' to '=', '{' to '[', '}' to ']', '|' to '\\', ':' to ';',
            '"' to '\'', '<' to ',', '>' to '.', '?' to '/', '~' to '`',
        )

        /** 美式键盘上打出 [character]（一个字素簇）要按哪个键、要不要 Shift。认不得的字符（中文等）返回空。 */
        fun forTyped(character: String): Typed? {
            if (character == " ") return Typed(Space, false)
            if (character.length != 1) return null
            val c = character[0]
            return when {
                c in 'a'..'z' || c in '0'..'9' -> Typed(Character(c), false)
                c in 'A'..'Z' -> Typed(Character(c.lowercaseChar()), true)
                else -> unshifted[c]?.let { Typed(Character(it), true) }
                    ?: if (c in "-=[]\\;',./`") Typed(Character(c), false) else null
            }
        }
    }
}

/** 一次按键：哪个键、按着哪些修饰键、打出的文字。 */
data class KeyInput(
    val key: TerminalKey,
    val modifiers: KeyModifiers = KeyModifiers.none,
    /** 平台按当前键盘布局和修饰键打出的文字；没有时为空。 */
    val text: String? = null,
    /** 不按修饰键时这个键打出的字符。 */
    val unshifted: Char? = null,
    /** 平台已经用掉、体现在 [text] 里的修饰键（比如打出 `A` 的 Shift）。 */
    val consumedModifiers: KeyModifiers = KeyModifiers.none,
    /** 按的是右边的 Alt/Option。 */
    val rightAlt: Boolean = false,
) {
    companion object {
        /** 打一个字符（一个字素簇），[modifiers] 是另外按着的（比如辅助栏上粘住的 Ctrl）。美式键盘上找不到这个字符时返回空。 */
        fun typing(character: String, modifiers: KeyModifiers = KeyModifiers.none): KeyInput? {
            val (key, shift) = TerminalKey.forTyped(character) ?: return null
            val unshifted = when (key) {
                is TerminalKey.Character -> key.character
                TerminalKey.Space -> ' '
                else -> null
            }
            return KeyInput(
                key = key,
                modifiers = if (shift) modifiers + KeyModifiers.shift else modifiers,
                text = character,
                unshifted = unshifted,
                consumedModifiers = if (shift) KeyModifiers.shift else KeyModifiers.none,
            )
        }
    }
}
