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

        public func deleteBackward() {
            press(.backspace)
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
            set {
                guard let range = newValue as? TerminalTextRange else { return }
                markedSelection = NSRange(location: range.from.offset, length: range.to.offset - range.from.offset)
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

        public func replace(_ range: UITextRange, withText text: String) {
            insertText(text)
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
