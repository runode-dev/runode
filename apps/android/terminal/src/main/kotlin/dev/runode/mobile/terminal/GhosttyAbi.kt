package dev.runode.mobile.terminal

/**
 * Kotlin 这边换算按键、修饰键、光标样式和鼠标事件要用的 libghostty 取值，照抄它的头文件，免得每次编码都过一趟
 * JNI 去问。和 C 那边一致由 [signature] 和 [VtJni.nativeGhosttyConstants] 相等守着。字母、数字、F 键各自是连续的
 * 一段，签名里核对了两头。
 */
internal object GhosttyAbi {
    const val KEY_UNIDENTIFIED = 0
    const val KEY_A = 20
    const val KEY_DIGIT_0 = 6
    const val KEY_F1 = 121
    const val KEY_SPACE = 63
    const val KEY_ENTER = 58
    const val KEY_TAB = 64
    const val KEY_BACKSPACE = 53
    const val KEY_ESCAPE = 120
    const val KEY_INSERT = 72
    const val KEY_DELETE = 68
    const val KEY_HOME = 71
    const val KEY_END = 69
    const val KEY_PAGE_UP = 74
    const val KEY_PAGE_DOWN = 73
    const val KEY_ARROW_UP = 78
    const val KEY_ARROW_DOWN = 75
    const val KEY_ARROW_LEFT = 76
    const val KEY_ARROW_RIGHT = 77
    const val KEY_BACKQUOTE = 1
    const val KEY_BACKSLASH = 2
    const val KEY_BRACKET_LEFT = 3
    const val KEY_BRACKET_RIGHT = 4
    const val KEY_COMMA = 5
    const val KEY_EQUAL = 16
    const val KEY_MINUS = 46
    const val KEY_PERIOD = 47
    const val KEY_QUOTE = 48
    const val KEY_SEMICOLON = 49
    const val KEY_SLASH = 50

    const val MODS_SHIFT = 1 shl 0
    const val MODS_CTRL = 1 shl 1
    const val MODS_ALT = 1 shl 2
    const val MODS_SUPER = 1 shl 3
    /** 按着的 Alt 在右边；只在 [MODS_ALT] 也置着时有意义。 */
    const val MODS_ALT_SIDE = 1 shl 8

    const val OPTION_AS_ALT_FALSE = 0
    const val OPTION_AS_ALT_TRUE = 1
    const val OPTION_AS_ALT_LEFT = 2
    const val OPTION_AS_ALT_RIGHT = 3

    const val CURSOR_STYLE_BLOCK = 1
    const val CURSOR_STYLE_BLOCK_HOLLOW = 3
    const val CURSOR_STYLE_BAR = 0
    const val CURSOR_STYLE_UNDERLINE = 2

    const val MOUSE_ACTION_PRESS = 0
    const val MOUSE_ACTION_RELEASE = 1
    const val MOUSE_BUTTON_LEFT = 1
    const val MOUSE_BUTTON_FOUR = 4
    const val MOUSE_BUTTON_FIVE = 5

    /** 和 C 那边的 `ghostty_constants` 逐项相等。 */
    val signature: IntArray
        get() = intArrayOf(
            KEY_UNIDENTIFIED, KEY_A, KEY_A + 25, KEY_DIGIT_0, KEY_DIGIT_0 + 9, KEY_F1, KEY_F1 + 23,
            KEY_SPACE, KEY_ENTER, KEY_TAB, KEY_BACKSPACE, KEY_ESCAPE, KEY_INSERT, KEY_DELETE, KEY_HOME, KEY_END,
            KEY_PAGE_UP, KEY_PAGE_DOWN, KEY_ARROW_UP, KEY_ARROW_DOWN, KEY_ARROW_LEFT, KEY_ARROW_RIGHT,
            KEY_BACKQUOTE, KEY_BACKSLASH, KEY_BRACKET_LEFT, KEY_BRACKET_RIGHT, KEY_COMMA, KEY_EQUAL, KEY_MINUS,
            KEY_PERIOD, KEY_QUOTE, KEY_SEMICOLON, KEY_SLASH,
            MODS_SHIFT, MODS_CTRL, MODS_ALT, MODS_SUPER, MODS_ALT_SIDE,
            OPTION_AS_ALT_FALSE, OPTION_AS_ALT_TRUE, OPTION_AS_ALT_LEFT, OPTION_AS_ALT_RIGHT,
            CURSOR_STYLE_BLOCK, CURSOR_STYLE_BLOCK_HOLLOW, CURSOR_STYLE_BAR, CURSOR_STYLE_UNDERLINE,
            MOUSE_ACTION_PRESS, MOUSE_ACTION_RELEASE, MOUSE_BUTTON_LEFT, MOUSE_BUTTON_FOUR, MOUSE_BUTTON_FIVE,
        )
}
