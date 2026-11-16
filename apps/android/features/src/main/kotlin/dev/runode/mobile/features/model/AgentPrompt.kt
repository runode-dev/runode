package dev.runode.mobile.features.model

import dev.runode.mobile.features.R

/**
 * agent 在屏幕底部问的带编号的问题：上面一两行问的是什么，下面一列选项。按下去发的是选项的编号键（[needsEnter] 时
 * 再按回车），认错了的后果和用户自己按这几个键一样。
 */
data class AgentPrompt(
    /** 选项上面有字的几行，从上往下，去掉了行首的空白。可能为空。 */
    val question: List<String>,
    /** 从 1 起连续编号，至少两个。 */
    val options: List<Option>,
    /** 按了编号键还要按回车才选定：Claude 给选项配了预览（画在选项右边的框）时，编号键只把光标挪到那一项。 */
    val needsEnter: Boolean = false,
) {
    data class Option(
        val number: Int,
        /**
         * 屏幕上的原文，末尾的按键提示（`(esc)` 这种短括号）去掉了。agent 因为一行放不下折到下一行的接上了，原文里的换行
         * （Claude 选项的标签和说明之间）留作 `\n`。
         */
        val text: String,
        /** 带着光标记号（[OptionLine.markers]）的那一项，回车选的就是它。 */
        val selected: Boolean,
    ) {
        val id: Int get() = number

        /**
         * 多选题里的选项原文以复选框开头：`[ ]` 是没勾，[checkMarks] 里的字放在方括号里是勾上了。不以复选框开头的为 null。
         */
        val checkbox: Checkbox?
            get() {
                if (text.length < 2 || text[0] != '[') return null
                val mark = text.codePointAt(1)
                val close = 1 + Character.charCount(mark)
                if (text.getOrNull(close) != ']') return null
                val checked = when (mark) {
                    ' '.code -> false
                    in checkMarks -> true
                    else -> return null
                }
                return Checkbox(checked, text.substring(close + 1).trimStart())
            }

        /** 显示和读屏用的字：有复选框时去掉它。 */
        val shownText: String get() = checkbox?.text ?: text

        /** [shownText] 的第一行：选项的标签。 */
        val label: String get() = shownText.substringBefore('\n')

        /** [shownText] 换行后面的说明（Claude 的选项有），几行并成一行；没有时为 null。 */
        val detail: String?
            get() = shownText.indexOf('\n').takeIf { it >= 0 }?.let { shownText.substring(it + 1).replace('\n', ' ') }
    }

    /**
     * 选 [option] 的键：编号本身，[needsEnter] 时跟着按回车；多选题不跟回车。读屏念
     * 「编号：[Option.shownText]」。
     */
    fun key(option: Option): QuickKey {
        val number = option.number.toString()
        val keys = if (needsEnter && !isMultiSelect) listOf(number, "enter") else listOf(number)
        return QuickKey(number, number, UiText.res(R.string.prompt_option, option.number, option.shownText), keys = keys)
    }

    /** 选项开头的复选框：勾没勾，和去掉复选框、再去掉开头空白后的原文。 */
    data class Checkbox(val checked: Boolean, val text: String)

    /** 多选题：有选项以复选框开头（[Option.checkbox]）。按编号键切换那一项勾没勾，不是选定它，所以没有默认答案。 */
    val isMultiSelect: Boolean get() = options.any { it.checkbox != null }

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

        /** 只有制表符（分隔线、框的上下沿），带着占几列；框的上下沿记作 0。 */
        data class Rule(val width: Int) : Entry()

        /** 别的字，带着去掉的行首空白有多宽、字的末尾在第几列。 */
        data class Text(val text: String, val indent: Int, val end: Int) : Entry()

        data class Option(val line: OptionLine) : Entry()

        companion object {
            /** 屏幕第 `row` 行 `raw`。 */
            fun at(raw: String, row: Int): Entry =
                when (val entry = of(raw)) {
                    is Option -> Option(entry.line.copy(row = row))
                    else -> entry
                }

            fun of(raw: String): Entry {
                val line = unboxed(raw)
                return when {
                    line.all { it.isWhitespace() } -> Blank
                    line.all { it.isWhitespace() || isBoxDrawing(it) } -> {
                        val corner = line.trimStart().firstOrNull() in setOf('╭', '╰', '┌', '└')
                        Rule(if (corner) 0 else widthOf(line))
                    }
                    else -> OptionLine.of(line)?.let(::Option) ?: run {
                        val indent = line.takeWhile { it.isWhitespace() }.length
                        Text(line.substring(indent), indent, widthOf(line))
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
        /** 选项那一行的正文和接在下面的续行。 */
        val rows: List<Row>,
        val selected: Boolean,
        /** 正文从第几个字开始，折行的缩进要到这里。 */
        val textColumn: Int,
        /** 选项那一行是屏幕的第几行。 */
        val row: Int = 0,
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
                return OptionLine(number, listOf(Row(rest, widthOf(line))), selected, line.length - rest.length)
            }
        }
    }

    /** 选项的一行正文，带着它的末尾在屏幕的第几列。 */
    private data class Row(val text: String, val end: Int)

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
         * 一次问几个问题时 Claude 在问题上面画的标签条（`← ☐ 标题 ☒ 标题 ✔ Submit →`，只问一个时是 `☐ 标题`）行首的字。
         * 标签只是问题的简称，不算问题。
         */
        internal val tabBarLeads: Set<Char> = setOf('←', '☐', '☒')

        private fun isTabBar(text: String): Boolean = text.firstOrNull() in tabBarLeads

        /** 复选框勾上时方括号里的字，按 code point。 */
        internal val checkMarks: Set<Int> = setOf('x', 'X', '✓', '✔', '*').mapTo(HashSet()) { it.code }

        /**
         * 从屏幕文字（一行一个）里认出提问。认法：从下往上，跳过空行、制表线和至多 [maxTrailingLines] 行别的字，找到一块
         * 编号从 1 起连续、至少两项的选项；选项之间可以夹空行和制表线，缩进到选项正文那一列的下一行算这一项的续行。
         * 选项块上面至多 [maxQuestionLines] 行有字的行是问题，遇到制表线或别的选项为止，标签条（[tabBarLeads]）跳过。
         * 行首行尾的框线（`│`）先去掉。选项超过 [maxOptions] 个时认不出。
         * 选项右边配了预览时（[previewCorners]），从框上沿那一行起各行只取框那一列左边的部分再认，框上沿那一行要落在
         * 认出的选项块里才算；从上往下逐个试，都不算时照没有预览认。
         */
        fun parse(lines: List<String>): AgentPrompt? {
            for ((start, column) in previewCorners(lines)) {
                val cropped = lines.mapIndexed { index, row ->
                    if (index >= start) splitAtColumn(row, column)?.first ?: row else row
                }
                parseRows(cropped, start)?.let { return it }
            }
            return parseRows(lines, null)
        }

        /** 按 [parse] 的认法认 `rows`；给了 `corner` 时它是预览框上沿所在的行，要落在选项块第一项到最后一项之间。 */
        private fun parseRows(rows: List<String>, corner: Int?): AgentPrompt? {
            val entries = merge(rows.mapIndexed { row, raw -> Entry.at(raw, row) })
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

            val lines = mutableListOf<OptionLine>()
            var expected = last.number
            while (expected > 0) {
                if (index < 0) return null
                when (val entry = entries[index]) {
                    Entry.Blank, is Entry.Rule -> {}
                    is Entry.Text -> return null
                    is Entry.Option -> {
                        if (entry.line.number != expected) return null
                        lines += entry.line
                        expected -= 1
                    }
                }
                index -= 1
            }
            if (corner != null && corner !in lines.last().row..lines.first().row) return null

            val blockTop = index + 1
            val question = ArrayDeque<String>()
            scan@ while (index >= 0 && question.size < maxQuestionLines) {
                when (val entry = entries[index]) {
                    Entry.Blank -> {}
                    is Entry.Rule, is Entry.Option -> break@scan
                    is Entry.Text -> if (!isTabBar(entry.text)) question.addFirst(entry.text)
                }
                index -= 1
            }
            val width = blockWidth(entries.subList(ruleAbove(entries, blockTop), entries.size))
            val options = lines.reversed().map { Option(it.number, stripHint(joinRows(it.rows, width)), it.selected) }
            return AgentPrompt(question.toList(), options, needsEnter = corner != null)
        }

        /**
         * Claude 给选项配了预览时把它画在选项右边的框里，框和框下面的说明都在框那一列的右边，选项折行留在左边。从上往下
         * 列出左边是选项、右边隔着空白画着框上沿（`┌`）的行，各带着它的下标和框从第几列起；预览里画的内框和上面输出里的框
         * 也会列进来。
         */
        private fun previewCorners(lines: List<String>): List<Pair<Int, Int>> =
            lines.indices.mapNotNull { index ->
                val row = lines[index]
                val gap = row.indexOf("  ┌")
                if (gap < 0) return@mapNotNull null
                val left = row.substring(0, gap + 2)
                if (OptionLine.of(left.trimEnd()) == null) null else index to widthOf(left)
            }

        /** 把 `row` 在第 `column` 列切成两段；不够那么宽、或者那一列落在宽字符中间时为 null。 */
        private fun splitAtColumn(row: String, column: Int): Pair<String, String>? {
            var width = 0
            var index = 0
            while (index < row.length) {
                if (width == column) return row.substring(0, index) to row.substring(index)
                val codePoint = row.codePointAt(index)
                width += cellWidth(codePoint)
                if (width > column) return null
                index += Character.charCount(codePoint)
            }
            return null
        }

        /** 选项块（从 `blockTop` 起）上面最近的那条制表线，中间隔着别的选项或者没有时就是 `blockTop`。 */
        private fun ruleAbove(entries: List<Entry>, blockTop: Int): Int {
            for (index in blockTop - 1 downTo 0) {
                when (entries[index]) {
                    is Entry.Rule -> return index
                    is Entry.Option -> return blockTop
                    Entry.Blank, is Entry.Text -> {}
                }
            }
            return blockTop
        }

        /**
         * 估计终端有几列：提问这一块里最宽的制表线或选项行。Claude 在提问上面画一条满宽的分隔线；没有时估小了，续行都当作
         * 放不下折下来的接上。框的上下沿不算，框里的字到不了那么宽。
         */
        private fun blockWidth(entries: List<Entry>): Int =
            entries.maxOfOrNull { entry ->
                when (entry) {
                    is Entry.Rule -> entry.width
                    is Entry.Option -> entry.line.rows.maxOf { it.end }
                    Entry.Blank, is Entry.Text -> 0
                }
            } ?: 0

        /**
         * 把一项的几行接成原文。agent 折行时西文按词、中日韩文字按字断开：下一行开头那个词（到空白或宽字符为止；以宽字符
         * 开头时就是这一个字）接在上一行后面会超出 `width` 的，是放不下才折的，接回去，两边都是宽字符（中日韩文字）时
         * 不加空格；放得下却换了行的是原文里的换行，留作 `\n`。
         */
        private fun joinRows(rows: List<Row>, width: Int): String {
            val text = StringBuilder()
            var previous: Row? = null
            for (row in rows) {
                if (previous != null) {
                    val word = firstWordWidth(row.text)
                    if (previous.end + 1 + word > width) {
                        val last = previous.text.codePointBefore(previous.text.length)
                        val first = row.text.codePointAt(0)
                        if (!(cellWidth(last) == 2 && cellWidth(first) == 2)) text.append(' ')
                    } else {
                        text.append('\n')
                    }
                }
                text.append(row.text)
                previous = row
            }
            return text.toString()
        }

        /** 一行开头那个不能断开的词占几列。 */
        private fun firstWordWidth(text: String): Int {
            if (text.isEmpty()) return 0
            if (cellWidth(text.codePointAt(0)) == 2) return 2
            var width = 0
            var index = 0
            while (index < text.length) {
                val codePoint = text.codePointAt(index)
                if (Character.isWhitespace(codePoint) || Character.isSpaceChar(codePoint) || cellWidth(codePoint) == 2) break
                width += 1
                index += Character.charCount(codePoint)
            }
            return width
        }

        /** 字在终端里占几列：East Asian Width 为宽（W）和全角（F）的字占两列，别的算一列。 */
        private fun cellWidth(codePoint: Int): Int = if (wide.any { codePoint in it }) 2 else 1

        /** 占两列的字的码位区间。 */
        private val wide: List<IntRange> = listOf(
            0x1100..0x115F, 0x231A..0x231B, 0x2329..0x232A, 0x23E9..0x23EC, 0x23F0..0x23F0, 0x23F3..0x23F3,
            0x25FD..0x25FE, 0x2614..0x2615, 0x2648..0x2653, 0x267F..0x267F, 0x2693..0x2693, 0x26A1..0x26A1,
            0x26AA..0x26AB, 0x26BD..0x26BE, 0x26C4..0x26C5, 0x26CE..0x26CE, 0x26D4..0x26D4, 0x26EA..0x26EA,
            0x26F2..0x26F3, 0x26F5..0x26F5, 0x26FA..0x26FA, 0x26FD..0x26FD, 0x2705..0x2705, 0x270A..0x270B,
            0x2728..0x2728, 0x274C..0x274C, 0x274E..0x274E, 0x2753..0x2755, 0x2757..0x2757, 0x2795..0x2797,
            0x27B0..0x27B0, 0x27BF..0x27BF, 0x2B1B..0x2B1C, 0x2B50..0x2B50, 0x2B55..0x2B55, 0x2E80..0x303E,
            0x3041..0x33FF, 0x3400..0x4DBF, 0x4E00..0x9FFF, 0xA000..0xA4CF, 0xAC00..0xD7A3, 0xF900..0xFAFF,
            0xFE10..0xFE19, 0xFE30..0xFE6F, 0xFF00..0xFF60, 0xFFE0..0xFFE6, 0x1F004..0x1F004, 0x1F0CF..0x1F0CF,
            0x1F18E..0x1F251, 0x1F300..0x1F64F, 0x1F680..0x1FAFF, 0x20000..0x3FFFD,
        )

        private fun widthOf(text: String): Int {
            var width = 0
            var index = 0
            while (index < text.length) {
                val codePoint = text.codePointAt(index)
                width += cellWidth(codePoint)
                index += Character.charCount(codePoint)
            }
            return width
        }

        /** 选项下面紧跟着、缩进到正文那一列的字是这一项的续行，收进去。 */
        private fun merge(entries: List<Entry>): List<Entry> {
            val merged = mutableListOf<Entry>()
            for (entry in entries) {
                val previous = merged.lastOrNull()
                if (entry is Entry.Text && previous is Entry.Option && entry.indent >= previous.line.textColumn) {
                    merged[merged.size - 1] =
                        Entry.Option(previous.line.copy(rows = previous.line.rows + Row(entry.text, entry.end)))
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
