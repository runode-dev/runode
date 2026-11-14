package dev.runode.mobile.terminal

import java.text.BreakIterator

/**
 * 按键栏上粘住的 Ctrl：按一下粘住，下一个打的字或按的键带上 Ctrl 发出去，用过就松开。这里只管怎么把它加到输入上，
 * 粘没粘住由 [TerminalView.controlLatched] 记着。
 */
internal object ControlLatch {
    /** 打一段文字得到的输入，以及打完后 Ctrl 还粘不粘着。 */
    data class Typed(val inputs: List<TerminalInput>, val latched: Boolean)

    /** 打出一段文字：换行按回车键发，粘住 Ctrl 时第一个字按 Ctrl 组合键发，其余的字连成一段原样发。 */
    fun type(text: String, latched: Boolean): Typed {
        val inputs = mutableListOf<TerminalInput>()
        var stillLatched = latched
        val pending = StringBuilder()
        fun flush() {
            if (pending.isEmpty()) return
            inputs += TerminalInput.Text(pending.toString())
            pending.setLength(0)
        }
        val clusters = BreakIterator.getCharacterInstance()
        clusters.setText(text)
        var start = clusters.first()
        var end = clusters.next()
        while (end != BreakIterator.DONE) {
            val cluster = text.substring(start, end)
            when {
                cluster == "\n" || cluster == "\r" || cluster == "\r\n" -> {
                    flush()
                    inputs += TerminalInput.Key(KeyInput(key = TerminalKey.Enter))
                }
                stillLatched -> {
                    flush()
                    stillLatched = false
                    val key = KeyInput.typing(cluster.lowercase(), KeyModifiers.control)
                    if (key != null) inputs += TerminalInput.Key(key) else pending.append(cluster)
                }
                else -> pending.append(cluster)
            }
            start = end
            end = clusters.next()
        }
        flush()
        return Typed(inputs, stillLatched)
    }

    /** 粘住时给这次按键加上 Ctrl。 */
    fun press(input: KeyInput, latched: Boolean): KeyInput =
        if (latched) input.copy(modifiers = input.modifiers + KeyModifiers.control) else input
}
