import Foundation

/// agent 在屏幕底部问的带编号的问题：上面一两行问的是什么，下面一列选项。按下去发的是选项的编号键
/// （`needsEnter` 时再按回车），认错了的后果和用户自己按这几个键一样。
public struct AgentPrompt: Hashable, Sendable {
    /// 选项上面有字的几行，从上往下，去掉了行首的空白。可能为空。
    public var question: [String]
    /// 从 1 起连续编号，至少两个。
    public var options: [Option]
    /// 按了编号键还要按回车才选定：Claude 给选项配了预览（画在选项右边的框）时，编号键只把光标挪到那一项。
    public var needsEnter = false

    public struct Option: Hashable, Sendable, Identifiable {
        public var number: Int
        /// 屏幕上的原文，末尾的按键提示（`(esc)` 这种短括号）去掉了。agent 因为一行放不下折到下一行的接上了，
        /// 原文里的换行（Claude 选项的标签和说明之间）留作 `\n`。
        public var text: String
        /// 带着光标记号（`OptionLine.markers`）的那一项，回车选的就是它。
        public var selected: Bool

        public var id: Int { number }

        /// 多选题里的选项原文以复选框开头：`[ ]` 是没勾，`checkMarks` 里的字放在方括号里是勾上了。返回勾没勾和
        /// 去掉复选框、再去掉开头空白后的原文；不以复选框开头的为 `nil`。按 Unicode 标量认，`[✔️]` 这种带变体
        /// 选择符的不算复选框。
        public var checkbox: (checked: Bool, text: String)? {
            let scalars = Array(text.unicodeScalars.prefix(3))
            guard scalars.count == 3, scalars[0] == "[", scalars[2] == "]" else { return nil }
            let checked: Bool
            if scalars[1] == " " {
                checked = false
            } else if Self.checkMarks.contains(scalars[1]) {
                checked = true
            } else {
                return nil
            }
            let rest = Substring(text.unicodeScalars.dropFirst(3))
            return (checked, String(rest.drop { $0.isWhitespace }))
        }

        /// 界面上显示的原文：有复选框时去掉它，复选框另外画。
        public var displayText: String { checkbox?.text ?? text }

        /// `displayText` 的第一行：选项的标签。
        public var label: String {
            String(displayText.prefix { $0 != "\n" })
        }

        /// `displayText` 换行后面的说明（Claude 的选项有），几行并成一行；没有时为 `nil`。
        public var detail: String? {
            guard let newline = displayText.firstIndex(of: "\n") else { return nil }
            return displayText[displayText.index(after: newline)...].replacingOccurrences(of: "\n", with: " ")
        }

        /// 复选框勾上时方括号里的字。
        private static let checkMarks: Set<Unicode.Scalar> = ["x", "X", "✓", "✔", "*"]
    }

    /// 多选题：有选项以复选框开头（`Option.checkbox`）。按编号键切换那一项勾没勾，不是选定它。
    public var isMultiSelect: Bool {
        options.contains { $0.checkbox != nil }
    }

    /// 选 `option` 的键：编号本身，`needsEnter` 时再按回车；多选题不跟回车。
    /// 读屏念「编号：原文」，原文不带复选框。
    public func key(for option: Option) -> QuickKey {
        QuickKey(
            label: String(option.number), key: String(option.number),
            accessibilityLabel: String(localized: "\(option.number)：\(option.displayText)"),
            then: needsEnter && !isMultiSelect ? ["enter"] : [])
    }

    /// 回车会选的那一项：单选题里带光标记号的。多选题没有。
    func isDefault(_ option: Option) -> Bool {
        option.selected && !isMultiSelect
    }

    /// 列表卡片上并排放的选项：多于两项时放第一项和最后一项，两项时就是这两项。
    var compactOptions: [Option] {
        guard options.count > 2, let first = options.first, let last = options.last else { return options }
        return [first, last]
    }

    /// 每个选项都只要按一个数字键。
    static let maxOptions = 9
    /// 选项下面最多容几行别的字（按键提示、agent 的状态栏）；再多就不算贴着屏幕底部，多半是 agent
    /// 输出里的一段普通编号列表。
    static let maxTrailingLines = 2
    /// 问题最多取几行。
    static let maxQuestionLines = 2
    /// 选项末尾的括号里不超过这么多字时当作按键提示去掉。
    static let maxHintLength = 6
    /// 一次问几个问题时 Claude 在问题上面画的标签条（`← ☐ 标题 ☒ 标题 ✔ Submit →`，只问一个时是 `☐ 标题`）
    /// 行首的字。标签只是问题的简称，不算问题。
    static let tabBarLeads: Set<Character> = ["←", "☐", "☒"]

    static func isTabBar(_ text: String) -> Bool {
        text.first.map(tabBarLeads.contains) ?? false
    }

    /// 从屏幕文字（一行一个）里认出提问。认法：从下往上，跳过空行、制表线和至多 `maxTrailingLines`
    /// 行别的字，找到一块编号从 1 起连续、至少两项的选项；选项之间可以夹空行和制表线，缩进到选项正文
    /// 那一列的下一行算这一项的续行。选项块上面至多 `maxQuestionLines` 行有字的行是问题，
    /// 遇到制表线或别的选项为止，标签条（`tabBarLeads`）跳过。行首行尾的框线（`│`）先去掉。选项超过
    /// `maxOptions` 个时认不出。选项右边配了预览时（`previewCorners`），从框上沿那一行起各行只取框那一列左边的
    /// 部分再认，框上沿那一行要落在认出的选项块里才算；从上往下逐个试，都不算时照没有预览认。
    public static func parse(_ lines: [String]) -> AgentPrompt? {
        for (start, column) in previewCorners(lines) {
            let cropped = lines.indices.map { index in
                index >= start ? left(of: lines[index], column: column) ?? lines[index] : lines[index]
            }
            if let prompt = parse(rows: cropped, corner: start) { return prompt }
        }
        return parse(rows: lines, corner: nil)
    }

    /// 按 `parse(_:)` 的认法认 `rows`；给了 `corner` 时它是预览框上沿所在的行，要落在选项块第一项到最后一项之间。
    private static func parse(rows: [String], corner: Int?) -> AgentPrompt? {
        let entries = merge(rows.indices.map { Entry(rows[$0], row: $0) })
        var index = entries.count - 1
        var trailing = 0
        while index >= 0 {
            if case .option = entries[index] { break }
            if case .text = entries[index] {
                trailing += 1
                if trailing > maxTrailingLines { return nil }
            }
            index -= 1
        }
        guard index >= 0, case .option(let last) = entries[index], (2...maxOptions).contains(last.number) else {
            return nil
        }

        var lines: [OptionLine] = []
        var expected = last.number
        while expected > 0 {
            guard index >= 0 else { return nil }
            switch entries[index] {
            case .blank, .rule:
                break
            case .text:
                return nil
            case .option(let line):
                guard line.number == expected else { return nil }
                lines.append(line)
                expected -= 1
            }
            index -= 1
        }
        if let corner, let first = lines.last, let last = lines.first, !(first.row...last.row).contains(corner) {
            return nil
        }

        let blockTop = index + 1
        var question: [String] = []
        scan: while index >= 0, question.count < maxQuestionLines {
            switch entries[index] {
            case .blank: break
            case .rule, .option: break scan
            case .text(let text, _, _) where isTabBar(text): break
            case .text(let text, _, _): question.insert(text, at: 0)
            }
            index -= 1
        }
        let width = blockWidth(entries[ruleAbove(entries, blockTop: blockTop)...])
        let options = lines.reversed().map {
            Option(number: $0.number, text: stripHint(joinRows($0.rows, width: width)), selected: $0.selected)
        }
        return AgentPrompt(question: question, options: options, needsEnter: corner != nil)
    }

    /// Claude 给选项配了预览时把它画在选项右边的框里，框和框下面的说明都在框那一列的右边，选项折行留在左边。
    /// 从上往下列出左边是选项、右边隔着空白画着框上沿（`┌`）的行，各带着它的下标和框从第几列起；预览里画的内框和
    /// 上面输出里的框也会列进来。
    private static func previewCorners(_ lines: [String]) -> [(start: Int, column: Int)] {
        lines.indices.compactMap { index -> (start: Int, column: Int)? in
            let line = lines[index]
            guard let gap = line.range(of: "  ┌") else { return nil }
            var left = line[..<gap.lowerBound]
            let column = cellWidth(left.unicodeScalars) + 2
            while let last = left.last, last.isWhitespace { left.removeLast() }
            guard OptionLine(left) != nil else { return nil }
            return (index, column)
        }
    }

    /// `line` 第 `column` 列左边的部分；不够那么宽、或者那一列落在宽字符中间时为空。
    private static func left(of line: String, column: Int) -> String? {
        let scalars = line.unicodeScalars
        var width = 0
        for index in scalars.indices {
            if width == column { return String(scalars[..<index]) }
            width += cellWidth(scalars[index])
            if width > column { return nil }
        }
        return nil
    }

    /// 选项块（从 `blockTop` 起）上面最近的那条制表线，中间隔着别的选项或者没有时就是 `blockTop`。
    private static func ruleAbove(_ entries: [Entry], blockTop: Int) -> Int {
        for index in stride(from: blockTop - 1, through: 0, by: -1) {
            switch entries[index] {
            case .rule: return index
            case .option: return blockTop
            case .blank, .text: continue
            }
        }
        return blockTop
    }

    /// 估计终端有几列：提问这一块里最宽的制表线或选项行。Claude 在提问上面画一条满宽的分隔线；没有时估小了，
    /// 续行都当作放不下折下来的接上。框的上下沿不算，框里的字到不了那么宽。
    private static func blockWidth(_ entries: ArraySlice<Entry>) -> Int {
        entries.map { entry in
            switch entry {
            case .rule(let width): width
            case .option(let line): line.rows.map(\.end).max() ?? 0
            case .blank, .text: 0
            }
        }.max() ?? 0
    }

    /// 把一项的几行接成原文。agent 折行时西文按词、中日韩文字按字断开：下一行开头那个词（到空白或宽字符为止；
    /// 以宽字符开头时就是这一个字）接在上一行后面会超出 `width` 的，是放不下才折的，接回去，两边都是宽字符
    /// （中日韩文字）时不加空格；放得下却换了行的是原文里的换行，留作 `\n`。
    private static func joinRows(_ rows: [Row], width: Int) -> String {
        var text = ""
        var previous: Row?
        for row in rows {
            if let previous {
                let word = firstWordWidth(row.text)
                if previous.end + 1 + word > width {
                    let wide = { (scalar: Unicode.Scalar?) in scalar.map { cellWidth($0) == 2 } ?? false }
                    if !(wide(previous.text.unicodeScalars.last) && wide(row.text.unicodeScalars.first)) {
                        text += " "
                    }
                } else {
                    text += "\n"
                }
            }
            text += row.text
            previous = row
        }
        return text
    }

    /// 一行开头那个不能断开的词占几列。
    private static func firstWordWidth(_ text: String) -> Int {
        let scalars = text.unicodeScalars
        if let first = scalars.first, cellWidth(first) == 2 { return 2 }
        return scalars.prefix { !$0.properties.isWhitespace && cellWidth($0) == 1 }.count
    }

    /// 字在终端里占几列：East Asian Width 为宽（W）和全角（F）的字占两列，别的算一列。
    static func cellWidth(_ scalar: Unicode.Scalar) -> Int {
        wide.contains { $0.contains(scalar.value) } ? 2 : 1
    }

    /// 占两列的字的码位区间。
    private static let wide: [ClosedRange<UInt32>] = [
        0x1100...0x115F, 0x231A...0x231B, 0x2329...0x232A, 0x23E9...0x23EC, 0x23F0...0x23F0, 0x23F3...0x23F3,
        0x25FD...0x25FE, 0x2614...0x2615, 0x2648...0x2653, 0x267F...0x267F, 0x2693...0x2693, 0x26A1...0x26A1,
        0x26AA...0x26AB, 0x26BD...0x26BE, 0x26C4...0x26C5, 0x26CE...0x26CE, 0x26D4...0x26D4, 0x26EA...0x26EA,
        0x26F2...0x26F3, 0x26F5...0x26F5, 0x26FA...0x26FA, 0x26FD...0x26FD, 0x2705...0x2705, 0x270A...0x270B,
        0x2728...0x2728, 0x274C...0x274C, 0x274E...0x274E, 0x2753...0x2755, 0x2757...0x2757, 0x2795...0x2797,
        0x27B0...0x27B0, 0x27BF...0x27BF, 0x2B1B...0x2B1C, 0x2B50...0x2B50, 0x2B55...0x2B55, 0x2E80...0x303E,
        0x3041...0x33FF, 0x3400...0x4DBF, 0x4E00...0x9FFF, 0xA000...0xA4CF, 0xAC00...0xD7A3, 0xF900...0xFAFF,
        0xFE10...0xFE19, 0xFE30...0xFE6F, 0xFF00...0xFF60, 0xFFE0...0xFFE6, 0x1F004...0x1F004, 0x1F0CF...0x1F0CF,
        0x1F18E...0x1F251, 0x1F300...0x1F64F, 0x1F680...0x1FAFF, 0x20000...0x3FFFD,
    ]

    static func cellWidth<S: Sequence<Unicode.Scalar>>(_ scalars: S) -> Int {
        scalars.reduce(0) { $0 + cellWidth($1) }
    }

    /// 一个提问的问题和各项原文，不记光标停在哪一项。两幅屏幕上是不是同一个提问按它比：屏幕上别处的字
    /// （agent 的状态栏、走着的秒数）变了不算变。
    struct Asked: Hashable, Sendable {
        var question: [String]
        var options: [String]
    }

    var asked: Asked {
        Asked(question: question, options: options.map(\.text))
    }

    /// 屏幕上的一行。
    private enum Entry {
        case blank
        /// 只有制表符（分隔线、框的上下沿），带着占几列；框的上下沿记作 0。
        case rule(Int)
        /// 别的字，带着去掉的行首空白有多宽、字的末尾在第几列。
        case text(String, indent: Int, end: Int)
        case option(OptionLine)

        /// 屏幕第 `row` 行 `raw`。
        init(_ raw: String, row: Int) {
            self.init(raw)
            if case .option(var option) = self {
                option.row = row
                self = .option(option)
            }
        }

        init(_ raw: String) {
            let line = Self.unboxed(raw)
            if line.allSatisfy(\.isWhitespace) {
                self = .blank
            } else if line.unicodeScalars.allSatisfy({ $0.properties.isWhitespace || Self.isBoxDrawing($0) }) {
                let edge = line.drop { $0.isWhitespace }.first
                let corner = edge.map { ["╭", "╰", "┌", "└"].contains($0) } ?? false
                self = .rule(corner ? 0 : AgentPrompt.cellWidth(line.unicodeScalars))
            } else if let option = OptionLine(line) {
                self = .option(option)
            } else {
                let indent = line.prefix { $0.isWhitespace }.count
                self = .text(
                    String(line.dropFirst(indent)), indent: indent, end: AgentPrompt.cellWidth(line.unicodeScalars))
            }
        }

        static func isBoxDrawing(_ scalar: Unicode.Scalar) -> Bool {
            (0x2500...0x257F).contains(scalar.value)
        }

        /// 去掉行尾空白和框两边的竖线；竖线后面的缩进留着，折行靠它认。
        static func unboxed(_ raw: String) -> Substring {
            var line = raw[...]
            while let last = line.last, last.isWhitespace { line.removeLast() }
            let borders: Set<Character> = ["│", "┃", "║"]
            if let last = line.last, borders.contains(last) {
                line.removeLast()
                while let last = line.last, last.isWhitespace { line.removeLast() }
            }
            let indent = line.prefix { $0.isWhitespace }
            if let first = line.dropFirst(indent.count).first, borders.contains(first) {
                line = line.dropFirst(indent.count + 1)
            }
            return line
        }
    }

    /// 一行选项：行首空白，可选的光标记号，一两位数字，`.` 或 `)`，空白，正文。
    private struct OptionLine {
        var number: Int
        /// 选项那一行的正文和接在下面的续行。
        var rows: [Row]
        var selected: Bool
        /// 正文从第几个字开始，折行的缩进要到这里。
        var textColumn: Int
        /// 选项那一行是屏幕的第几行。
        var row = 0

        static let markers: Set<Character> = ["❯", "›", ">", "▶", "●"]

        init?(_ line: Substring) {
            var rest = line.drop { $0.isWhitespace }
            selected = rest.first.map(Self.markers.contains) ?? false
            if selected { rest = rest.dropFirst().drop { $0.isWhitespace } }
            let digits = rest.prefix { $0.isASCII && $0.isNumber }
            guard (1...2).contains(digits.count), let number = Int(digits) else { return nil }
            rest = rest.dropFirst(digits.count)
            guard let dot = rest.first, dot == "." || dot == ")" else { return nil }
            rest = rest.dropFirst()
            let gap = rest.prefix { $0.isWhitespace }
            guard !gap.isEmpty else { return nil }
            rest = rest.dropFirst(gap.count)
            guard !rest.isEmpty else { return nil }
            self.number = number
            rows = [Row(text: String(rest), end: AgentPrompt.cellWidth(line.unicodeScalars))]
            textColumn = line.count - rest.count
        }
    }

    /// 选项的一行正文，带着它的末尾在屏幕的第几列。
    private struct Row {
        var text: String
        var end: Int
    }

    /// 选项下面紧跟着、缩进到正文那一列的字是这一项的续行，收进去。
    private static func merge(_ entries: [Entry]) -> [Entry] {
        var merged: [Entry] = []
        for entry in entries {
            if case .text(let text, let indent, let end) = entry, case .option(var option) = merged.last,
                indent >= option.textColumn
            {
                option.rows.append(Row(text: text, end: end))
                merged[merged.count - 1] = .option(option)
            } else {
                merged.append(entry)
            }
        }
        return merged
    }

    /// 去掉末尾 `(esc)`、`(y)` 这种短括号；去掉后没字了就留着。
    private static func stripHint(_ text: String) -> String {
        guard text.last == ")", let open = text.lastIndex(of: "(") else { return text }
        let inside = text[text.index(after: open)..<text.index(before: text.endIndex)]
        guard inside.count <= maxHintLength, !inside.contains("(") else { return text }
        let kept = text[..<open].trimmingCharacters(in: .whitespaces)
        return kept.isEmpty ? text : kept
    }
}
