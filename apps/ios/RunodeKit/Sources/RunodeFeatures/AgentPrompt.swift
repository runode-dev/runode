import Foundation

/// agent 在屏幕底部问的带编号的问题：上面一两行问的是什么，下面一列选项。按下去发的是选项的编号键，
/// 认错了的后果和通用的数字键一样。
public struct AgentPrompt: Hashable, Sendable {
    /// 选项上面有字的几行，从上往下，去掉了行首的空白。可能为空。
    public var question: [String]
    /// 从 1 起连续编号，至少两个。
    public var options: [Option]

    public struct Option: Hashable, Sendable, Identifiable {
        public var number: Int
        /// 屏幕上的原文：折到下一行的接上了，末尾的按键提示（`(esc)` 这种短括号）去掉了。
        public var text: String
        /// 带着光标记号（`OptionLine.markers`）的那一项，回车选的就是它。
        public var selected: Bool

        public var id: Int { number }

        /// 选这一项的键：编号本身，读屏念「编号：原文」。
        public var key: QuickKey {
            QuickKey(label: String(number), key: String(number), accessibilityLabel: String(localized: "\(number)：\(text)"))
        }
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

    /// 从屏幕文字（一行一个）里认出提问。认法：从下往上，跳过空行、制表线和至多 `maxTrailingLines`
    /// 行别的字，找到一块编号从 1 起连续、至少两项的选项；选项之间可以夹空行和制表线，缩进到选项正文
    /// 那一列的下一行算折行，接到上一项后面。选项块上面至多 `maxQuestionLines` 行有字的行是问题，
    /// 遇到制表线或别的选项为止。行首行尾的框线（`│`）先去掉。选项超过 `maxOptions` 个时认不出。
    public static func parse(_ lines: [String]) -> AgentPrompt? {
        let entries = merge(lines.map(Entry.init))
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

        var options: [Option] = []
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
                options.append(Option(number: line.number, text: stripHint(line.text), selected: line.selected))
                expected -= 1
            }
            index -= 1
        }

        var question: [String] = []
        scan: while index >= 0, question.count < maxQuestionLines {
            switch entries[index] {
            case .blank: break
            case .rule, .option: break scan
            case .text(let text, _): question.insert(text, at: 0)
            }
            index -= 1
        }
        return AgentPrompt(question: question, options: options.reversed())
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
        /// 只有制表符（分隔线、框的上下沿）。
        case rule
        /// 别的字，带着去掉的行首空白有多宽。
        case text(String, indent: Int)
        case option(OptionLine)

        init(_ raw: String) {
            let line = Self.unboxed(raw)
            if line.allSatisfy(\.isWhitespace) {
                self = .blank
            } else if line.unicodeScalars.allSatisfy({ $0.properties.isWhitespace || Self.isBoxDrawing($0) }) {
                self = .rule
            } else if let option = OptionLine(line) {
                self = .option(option)
            } else {
                let indent = line.prefix { $0.isWhitespace }.count
                self = .text(String(line.dropFirst(indent)), indent: indent)
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
        var text: String
        var selected: Bool
        /// 正文从第几个字开始，折行的缩进要到这里。
        var textColumn: Int

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
            text = String(rest)
            textColumn = line.count - rest.count
        }
    }

    /// 选项下面紧跟着、缩进到正文那一列的字是这一项折下来的，接上去。
    private static func merge(_ entries: [Entry]) -> [Entry] {
        var merged: [Entry] = []
        for entry in entries {
            if case .text(let text, let indent) = entry, case .option(var option) = merged.last,
                indent >= option.textColumn
            {
                option.text += " " + text
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
