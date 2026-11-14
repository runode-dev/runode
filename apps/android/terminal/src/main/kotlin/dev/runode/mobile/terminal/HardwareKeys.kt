package dev.runode.mobile.terminal

import android.view.KeyCharacterMap
import android.view.KeyEvent

/** 硬件键盘的按键怎么变成终端的输入。只用 [KeyEvent] 的常量，不碰事件对象，取字符交给调用方。 */
internal object HardwareKeys {
    private const val CTRL = KeyEvent.META_CTRL_MASK
    private const val ALT = KeyEvent.META_ALT_MASK

    /**
     * 要自己编码的键：功能键、方向键，以及按着 Ctrl 或 Alt 的组合；普通打字返回空，交给调用方当文字打。按着
     * Meta 的留给系统。[unicodeChar] 按给的修饰键状态取这个键打出的字符（[KeyEvent.getUnicodeChar]），没有时为 0。
     */
    fun keyInput(keyCode: Int, metaState: Int, unicodeChar: (Int) -> Int): KeyInput? {
        if (metaState and KeyEvent.META_META_MASK != 0) return null
        var modifiers = KeyModifiers.none
        if (metaState and KeyEvent.META_SHIFT_MASK != 0) modifiers += KeyModifiers.shift
        if (metaState and CTRL != 0) modifiers += KeyModifiers.control
        val alt = metaState and ALT != 0
        if (alt) modifiers += KeyModifiers.alt
        // 只按着右边的 Alt 时才算右 Alt，按 `TermSettings.optionAsAlt` 区分两边。
        val rightAlt = alt && metaState and KeyEvent.META_ALT_RIGHT_ON != 0 && metaState and KeyEvent.META_ALT_LEFT_ON == 0
        val special: TerminalKey? = when (keyCode) {
            KeyEvent.KEYCODE_ESCAPE -> TerminalKey.Escape
            KeyEvent.KEYCODE_DPAD_UP -> TerminalKey.Up
            KeyEvent.KEYCODE_DPAD_DOWN -> TerminalKey.Down
            KeyEvent.KEYCODE_DPAD_LEFT -> TerminalKey.Left
            KeyEvent.KEYCODE_DPAD_RIGHT -> TerminalKey.Right
            KeyEvent.KEYCODE_MOVE_HOME -> TerminalKey.Home
            KeyEvent.KEYCODE_MOVE_END -> TerminalKey.End
            KeyEvent.KEYCODE_PAGE_UP -> TerminalKey.PageUp
            KeyEvent.KEYCODE_PAGE_DOWN -> TerminalKey.PageDown
            KeyEvent.KEYCODE_FORWARD_DEL -> TerminalKey.Delete
            KeyEvent.KEYCODE_INSERT -> TerminalKey.Insert
            KeyEvent.KEYCODE_TAB -> TerminalKey.Tab
            in KeyEvent.KEYCODE_F1..KeyEvent.KEYCODE_F12 -> TerminalKey.Function(keyCode - KeyEvent.KEYCODE_F1 + 1)
            else -> null
        }
        if (special != null) return KeyInput(key = special, modifiers = modifiers, rightAlt = rightAlt)
        val enter = keyCode == KeyEvent.KEYCODE_ENTER || keyCode == KeyEvent.KEYCODE_NUMPAD_ENTER
        // Shift+回车带着 Shift 按 VT 的模式编码，程序据此分出换行和提交；当文字打的话成了不带修饰键的回车。
        if (enter && modifiers == KeyModifiers.shift) return KeyInput(key = TerminalKey.Enter, modifiers = modifiers)
        if (KeyModifiers.control !in modifiers && !alt) return null
        when {
            enter -> return KeyInput(key = TerminalKey.Enter, modifiers = modifiers, rightAlt = rightAlt)
            keyCode == KeyEvent.KEYCODE_DEL ->
                return KeyInput(key = TerminalKey.Backspace, modifiers = modifiers, rightAlt = rightAlt)
            keyCode == KeyEvent.KEYCODE_SPACE ->
                return KeyInput(key = TerminalKey.Space, modifiers = modifiers, unshifted = ' ', rightAlt = rightAlt)
        }
        val base = character(unicodeChar(0))?.lowercase() ?: return null
        val physical = TerminalKey.forTyped(base)?.key ?: return null
        val typed = character(unicodeChar(metaState and CTRL.inv()))
        val withoutAlt = character(unicodeChar(metaState and (CTRL or ALT).inv()))
        // Alt 打出了别的字（有的布局上 Alt+E 是 é）：这个修饰键已经体现在文字里了。
        val consumed = if (alt && typed != null && typed != withoutAlt) KeyModifiers.alt else KeyModifiers.none
        return KeyInput(
            key = physical, modifiers = modifiers, text = typed, unshifted = base.singleOrNull(),
            consumedModifiers = consumed, rightAlt = rightAlt,
        )
    }

    /** 粘贴的快捷键：Meta+V，以及终端里常用的 Ctrl+Shift+V（Ctrl+V 要留给程序）。 */
    fun isPaste(keyCode: Int, metaState: Int): Boolean {
        if (keyCode != KeyEvent.KEYCODE_V) return false
        if (metaState and KeyEvent.META_META_MASK != 0) return true
        return metaState and CTRL != 0 && metaState and KeyEvent.META_SHIFT_MASK != 0
    }

    /** 字符码换成文字；没有字，或者是死键打出的附加符号时为空。 */
    private fun character(unicode: Int): String? {
        if (unicode == 0 || unicode and KeyCharacterMap.COMBINING_ACCENT != 0) return null
        return String(Character.toChars(unicode))
    }
}
