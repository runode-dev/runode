#if os(iOS)
    import UIKit

    /// 输入法组字用的文本位置：组字文字里的 UTF-16 偏移。
    final class TerminalTextPosition: UITextPosition {
        let offset: Int

        init(_ offset: Int) {
            self.offset = offset
        }
    }

    final class TerminalTextRange: UITextRange {
        let from: TerminalTextPosition
        let to: TerminalTextPosition

        init(_ from: Int, _ to: Int) {
            self.from = TerminalTextPosition(min(from, to))
            self.to = TerminalTextPosition(max(from, to))
        }

        override var start: UITextPosition { from }
        override var end: UITextPosition { to }
        override var isEmpty: Bool { from.offset == to.offset }
    }

    /// 终端没有可以编辑的文档：已经发出去的字归程序管。这里的「文档」只有输入法正在组的那段文字，
    /// 上屏（`insertText`、`unmarkText`）时整段发给程序，退格在组字时归输入法、不组字时发退格键。
    extension TerminalView: UITextInput {
        // MARK: 键盘的样子

        public var autocorrectionType: UITextAutocorrectionType {
            get { .no }
            set {}
        }
        public var autocapitalizationType: UITextAutocapitalizationType {
            get { .none }
            set {}
        }
        public var spellCheckingType: UITextSpellCheckingType {
            get { .no }
            set {}
        }
        public var smartQuotesType: UITextSmartQuotesType {
            get { .no }
            set {}
        }
        public var smartDashesType: UITextSmartDashesType {
            get { .no }
            set {}
        }
        public var smartInsertDeleteType: UITextSmartInsertDeleteType {
            get { .no }
            set {}
        }
        public var inlinePredictionType: UITextInlinePredictionType {
            get { .no }
            set {}
        }
        public var keyboardAppearance: UIKeyboardAppearance {
            get { .dark }
            set {}
        }

        // MARK: UIKeyInput

        /// 永远说有字，退格键才总会调到 `deleteBackward`。
        public var hasText: Bool { true }

        public func insertText(_ text: String) {
            let hadMarked = !markedText.isEmpty
            if hadMarked {
                inputDelegate?.textWillChange(self)
                markedText = ""
                markedSelection = NSRange(location: 0, length: 0)
                inputDelegate?.textDidChange(self)
                positionMarkedText()
            }
            typeText(text)
        }

        /// 组字时退格一般由输入法自己改组字文字（`setMarkedText`）；真调到这里时只删组字文字里的：它还没发给
        /// 程序，发退格键会删掉已经上屏的字。
        public func deleteBackward() {
            guard !markedText.isEmpty else { return press(.backspace) }
            let text = markedText as NSString
            let range: NSRange
            if markedSelection.length > 0 {
                range = markedSelection
            } else if markedSelection.location > 0 {
                range = text.rangeOfComposedCharacterSequence(at: min(markedSelection.location, text.length) - 1)
            } else {
                return
            }
            editMarkedText(range, with: "")
        }

        /// 把组字文字里的 `range` 换成 `text`，光标放在换上的字后面，组字接着组。
        private func editMarkedText(_ range: NSRange, with text: String) {
            inputDelegate?.textWillChange(self)
            markedText = (markedText as NSString).replacingCharacters(in: range, with: text)
            markedSelection = NSRange(location: range.location + (text as NSString).length, length: 0)
            inputDelegate?.textDidChange(self)
            positionMarkedText()
        }

        // MARK: 组字

        public func setMarkedText(_ markedText: String?, selectedRange: NSRange) {
            self.markedText = markedText ?? ""
            let length = (self.markedText as NSString).length
            markedSelection = NSRange(
                location: min(selectedRange.location, length),
                length: min(selectedRange.length, max(0, length - min(selectedRange.location, length))))
            positionMarkedText()
        }

        public func unmarkText() {
            guard !markedText.isEmpty else { return }
            let text = markedText
            markedText = ""
            markedSelection = NSRange(location: 0, length: 0)
            positionMarkedText()
            typeText(text)
        }

        public var markedTextRange: UITextRange? {
            markedText.isEmpty ? nil : TerminalTextRange(0, (markedText as NSString).length)
        }

        public var selectedTextRange: UITextRange? {
            get {
                TerminalTextRange(markedSelection.location, markedSelection.location + markedSelection.length)
            }
            // 系统给的偏移可能是照旧的组字文字算的，按现在的长度夹住：`deleteBackward` 拿 `markedSelection`
            // 直接当替换范围，越界会抛 NSRangeException。
            set {
                guard let range = newValue as? TerminalTextRange else { return }
                let from = min(max(range.from.offset, 0), documentLength)
                let to = min(max(range.to.offset, 0), documentLength)
                markedSelection = NSRange(location: from, length: to - from)
            }
        }

        // MARK: 文档

        private var documentLength: Int { (markedText as NSString).length }

        public func text(in range: UITextRange) -> String? {
            guard let range = range as? TerminalTextRange else { return nil }
            let from = min(range.from.offset, documentLength)
            let to = min(range.to.offset, documentLength)
            return (markedText as NSString).substring(with: NSRange(location: from, length: to - from))
        }

        /// 组字时改的是组字文字里的那一段，组字接着组；不组字时文档是空的，换上的字就是输入。
        public func replace(_ range: UITextRange, withText text: String) {
            guard !markedText.isEmpty, let range = range as? TerminalTextRange else { return insertText(text) }
            let from = min(range.from.offset, documentLength)
            let to = min(range.to.offset, documentLength)
            editMarkedText(NSRange(location: from, length: to - from), with: text)
        }

        public var beginningOfDocument: UITextPosition { TerminalTextPosition(0) }

        public var endOfDocument: UITextPosition { TerminalTextPosition(documentLength) }

        public func textRange(from fromPosition: UITextPosition, to toPosition: UITextPosition) -> UITextRange? {
            guard let from = fromPosition as? TerminalTextPosition, let to = toPosition as? TerminalTextPosition else {
                return nil
            }
            return TerminalTextRange(from.offset, to.offset)
        }

        public func position(from position: UITextPosition, offset: Int) -> UITextPosition? {
            guard let position = position as? TerminalTextPosition else { return nil }
            let target = position.offset + offset
            guard target >= 0, target <= documentLength else { return nil }
            return TerminalTextPosition(target)
        }

        public func position(
            from position: UITextPosition, in direction: UITextLayoutDirection, offset: Int
        ) -> UITextPosition? {
            switch direction {
            case .left, .up: self.position(from: position, offset: -offset)
            default: self.position(from: position, offset: offset)
            }
        }

        public func compare(_ position: UITextPosition, to other: UITextPosition) -> ComparisonResult {
            let left = (position as? TerminalTextPosition)?.offset ?? 0
            let right = (other as? TerminalTextPosition)?.offset ?? 0
            return left < right ? .orderedAscending : left > right ? .orderedDescending : .orderedSame
        }

        public func offset(from: UITextPosition, to toPosition: UITextPosition) -> Int {
            ((toPosition as? TerminalTextPosition)?.offset ?? 0) - ((from as? TerminalTextPosition)?.offset ?? 0)
        }

        public func position(within range: UITextRange, farthestIn direction: UITextLayoutDirection) -> UITextPosition? {
            switch direction {
            case .left, .up: range.start
            default: range.end
            }
        }

        public func characterRange(
            byExtending position: UITextPosition, in direction: UITextLayoutDirection
        ) -> UITextRange? {
            guard let position = position as? TerminalTextPosition else { return nil }
            switch direction {
            case .left, .up: return TerminalTextRange(0, position.offset)
            default: return TerminalTextRange(position.offset, documentLength)
            }
        }

        public func baseWritingDirection(
            for position: UITextPosition, in direction: UITextStorageDirection
        ) -> NSWritingDirection {
            .leftToRight
        }

        public func setBaseWritingDirection(_ writingDirection: NSWritingDirection, for range: UITextRange) {}

        // MARK: 几何：候选框摆在光标处

        public func firstRect(for range: UITextRange) -> CGRect {
            cursorRectInSelf
        }

        public func caretRect(for position: UITextPosition) -> CGRect {
            let rect = cursorRectInSelf
            return CGRect(x: rect.minX, y: rect.minY, width: 2, height: rect.height)
        }

        public func selectionRects(for range: UITextRange) -> [UITextSelectionRect] {
            []
        }

        public func closestPosition(to point: CGPoint) -> UITextPosition? {
            endOfDocument
        }

        public func closestPosition(to point: CGPoint, within range: UITextRange) -> UITextPosition? {
            range.end
        }

        public func characterRange(at point: CGPoint) -> UITextRange? {
            nil
        }
    }
#endif
