package dev.runode.mobile.features.model

import dev.runode.mobile.features.R

/**
 * agent 在屏幕底部问的带编号的问题：上面一两行问的是什么，下面一列选项。按下去发的是选项的编号键，认错了的后果和
 * 通用的数字键一样。
 */
data class AgentPrompt(
    /** 选项上面有字的几行，从上往下，去掉了行首的空白。可能为空。 */
    val question: List<String>,
    /** 从 1 起连续编号，至少两个。 */
    val options: List<Option>,
) {
    data class Option(
        val number: Int,
        /** 屏幕上的原文：折到下一行的接上了，末尾的按键提示（`(esc)` 这种短括号）去掉了。 */
        val text: String,
        /** 带着光标记号（[OptionLine.markers]）的那一项，回车选的就是它。 */
        val selected: Boolean,
    ) {
        val id: Int get() = number

        /** 选这一项的键：编号本身，读屏念「编号：原文」。 */
        val key: QuickKey
            get() = QuickKey(number.toString(), number.toString(), UiText.res(R.string.prompt_option, number, text))
    }

    /**
     * 一个提问的问题和各项原文，不记光标停在哪一项。两幅屏幕上是不是同一个提问按它比：屏幕上别处的字（agent 的状态栏、
     * 走着的秒数）变了不算变。
     */
    data class Asked(val question: List<String>, val options: List<String>)

    val asked: Asked get() = Asked(question, options.map { it.text })

    /** 挑出的两项：多于两项时是第一项和最后一项，两项时就是这两项。 */
    val compactOptions: List<Option>
        get() = if (options.size > 2) listOf(options.first(), options.last()) else options

    /** 屏幕上的一行。 */
    private sealed class Entry {
        data object Blank : Entry()

        /** 只有制表符（分隔线、框的上下沿）。 */
        data object Rule : Entry()

        /** 别的字，带着去掉的行首空白有多宽。 */
        data class Text(val text: String, val indent: Int) : Entry()

        data class Option(val line: OptionLine) : Entry()

        companion object {
            fun of(raw: String): Entry {
                val line = unboxed(raw)
                return when {
                    line.all { it.isWhitespace() } -> Blank
                    line.all { it.isWhitespace() || isBoxDrawing(it) } -> Rule
                    else -> OptionLine.of(line)?.let(::Option) ?: run {
                        val indent = line.takeWhile { it.isWhitespace() }.length
                        Text(line.substring(indent), indent)
                    }
                }
            }

            fun isBoxDrawing(c: Char): Boolean = c.code in 0x2500..0x257F

            private val borders = setOf('│', '┃', '║')

            /** 去掉行尾空白和框两边的竖线；竖线后面的缩进留着，折行靠它认。 */
            fun unboxed(raw: String): String {
                var line = raw.trimEnd()
                if (line.lastOrNull() in borders) line = line.dropLast(1).trimEnd()
                val indent = line.takeWhile { it.isWhitespace() }.length
                if (line.getOrNull(indent) in borders) line = line.substring(indent + 1)
                return line
            }
        }
    }

    /** 一行选项：行首空白，可选的光标记号，一两位数字，`.` 或 `)`，空白，正文。 */
    private data class OptionLine(
        val number: Int,
        val text: String,
        val selected: Boolean,
        /** 正文从第几个字开始，折行的缩进要到这里。 */
        val textColumn: Int,
    ) {
        companion object {
            val markers = setOf('❯', '›', '>', '▶', '●')

            fun of(line: String): OptionLine? {
                var rest = line.trimStart()
                val selected = rest.firstOrNull() in markers
                if (selected) rest = rest.drop(1).trimStart()
                val digits = rest.takeWhile { it in '0'..'9' }
                if (digits.length !in 1..2) return null
                val number = digits.toInt()
                rest = rest.drop(digits.length)
                if (rest.firstOrNull().let { it != '.' && it != ')' }) return null
                rest = rest.drop(1)
                val gap = rest.takeWhile { it.isWhitespace() }
                if (gap.isEmpty()) return null
                rest = rest.drop(gap.length)
                if (rest.isEmpty()) return null
                return OptionLine(number, rest, selected, line.length - rest.length)
            }
        }
    }

    companion object {
        /** 每个选项都只要按一个数字键。 */
        internal const val maxOptions = 9

        /** 选项下面最多容几行别的字（按键提示、agent 的状态栏）；再多就不算贴着屏幕底部，多半是 agent 输出里的一段普通编号列表。 */
        internal const val maxTrailingLines = 2

        /** 问题最多取几行。 */
        internal const val maxQuestionLines = 2

        /** 选项末尾的括号里不超过这么多字时当作按键提示去掉。 */
        internal const val maxHintLength = 6

        /**
         * 从屏幕文字（一行一个）里认出提问。认法：从下往上，跳过空行、制表线和至多 [maxTrailingLines] 行别的字，找到一块
         * 编号从 1 起连续、至少两项的选项；选项之间可以夹空行和制表线，缩进到选项正文那一列的下一行算折行，接到上一项后面。
         * 选项块上面至多 [maxQuestionLines] 行有字的行是问题，遇到制表线或别的选项为止。行首行尾的框线（`│`）先去掉。
         * 选项超过 [maxOptions] 个时认不出。
         */
        fun parse(lines: List<String>): AgentPrompt? {
            val entries = merge(lines.map(Entry::of))
            var index = entries.size - 1
            var trailing = 0
            while (index >= 0) {
                val entry = entries[index]
                if (entry is Entry.Option) break
                if (entry is Entry.Text) {
                    trailing += 1
                    if (trailing > maxTrailingLines) return null
                }
                index -= 1
            }
            val last = (entries.getOrNull(index) as? Entry.Option)?.line ?: return null
            if (last.number !in 2..maxOptions) return null

            val options = mutableListOf<Option>()
            var expected = last.number
            while (expected > 0) {
                if (index < 0) return null
                when (val entry = entries[index]) {
                    Entry.Blank, Entry.Rule -> {}
                    is Entry.Text -> return null
                    is Entry.Option -> {
                        if (entry.line.number != expected) return null
                        options += Option(entry.line.number, stripHint(entry.line.text), entry.line.selected)
                        expected -= 1
                    }
                }
                index -= 1
            }

            val question = ArrayDeque<String>()
            scan@ while (index >= 0 && question.size < maxQuestionLines) {
                when (val entry = entries[index]) {
                    Entry.Blank -> {}
                    Entry.Rule, is Entry.Option -> break@scan
                    is Entry.Text -> question.addFirst(entry.text)
                }
                index -= 1
            }
            return AgentPrompt(question.toList(), options.reversed())
        }

        /** 选项下面紧跟着、缩进到正文那一列的字是这一项折下来的，接上去。 */
        private fun merge(entries: List<Entry>): List<Entry> {
            val merged = mutableListOf<Entry>()
            for (entry in entries) {
                val previous = merged.lastOrNull()
                if (entry is Entry.Text && previous is Entry.Option && entry.indent >= previous.line.textColumn) {
                    merged[merged.size - 1] = Entry.Option(previous.line.copy(text = previous.line.text + " " + entry.text))
                } else {
                    merged += entry
                }
            }
            return merged
        }

        /** 去掉末尾 `(esc)`、`(y)` 这种短括号；去掉后没字了就留着。 */
        private fun stripHint(text: String): String {
            if (!text.endsWith(")")) return text
            val open = text.lastIndexOf('(')
            if (open < 0) return text
            val inside = text.substring(open + 1, text.length - 1)
            if (inside.codePointCount(0, inside.length) > maxHintLength || inside.contains('(')) return text
            val kept = text.substring(0, open).trim()
            return kept.ifEmpty { text }
        }
    }
}
