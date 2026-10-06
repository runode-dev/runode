#if os(iOS)
    import RunodeProtocol
    import UIKit

    /// 终端视图要上层做的事。
    @MainActor
    public protocol TerminalViewDelegate: AnyObject {
        /// 用户打了字、按了键或者粘贴。
        func terminalView(_ view: TerminalView, didInput input: TerminalInput)
        /// 按视图现在的大小，「适配本机屏幕」该要多大的网格。视图大小变了就报一次。
        func terminalView(_ view: TerminalView, fitSizeDidChange size: GridSize)
        /// 视口离开了最底下（在看回滚历史），或者回到了最底下。
        func terminalView(_ view: TerminalView, didScrollBack scrolledBack: Bool)
    }

    /// 终端视图：按宿主给的网格尺寸画（网格比屏幕大时可以缩放、平移），上下拖动看回滚历史，接软键盘
    /// （含输入法的组字）、硬件键盘和键盘上方的辅助栏。
    ///
    /// 结构：自己是第一响应者，负责键盘输入（`UITextInput`）；里面一个 `UIScrollView` 管缩放和平移，
    /// 再里面 `TerminalGridView` 用 CoreText 画网格。网格按字体的自然大小排，缩放靠滚动视图的
    /// `zoomScale`，缩放结束后按新的比例重画，字不发虚。
    public final class TerminalView: UIView, UIScrollViewDelegate, UIGestureRecognizerDelegate {
        public weak var delegate: (any TerminalViewDelegate)?

        /// 正在画的那份 VT，由视图模型给；视图只读它和滚它的视口。
        private var terminal: VTerminal?
        private let scrollView = UIScrollView()
        private let grid = TerminalGridView()
        private let markedLabel = UILabel()
        private lazy var accessoryBar = KeyboardAccessoryBar(owner: self)
        private var refreshScheduled = false
        /// 用户自己捏合缩放过：网格尺寸变了也不再自动按屏幕宽度缩放。
        private var userZoomed = false
        private var lastFitSize: GridSize?
        private var lastScrolledBack = false
        private var scrollbackRemainder: CGFloat = 0
        private var repeatTask: Task<Void, Never>?
        private let bellFeedback = UIImpactFeedbackGenerator(style: .light)
        private var lastBell = ContinuousClock.now - .seconds(1)

        /// 辅助栏上粘住的 Ctrl：下一个打的字按 Ctrl 组合键发。
        var controlLatched = false {
            didSet { accessoryBar.setControlLatched(controlLatched) }
        }

        // 输入法组字中的文字，以及组字里光标的位置（UTF-16 偏移）。
        var markedText = ""
        var markedSelection = NSRange(location: 0, length: 0)
        public weak var inputDelegate: (any UITextInputDelegate)?
        public lazy var tokenizer: any UITextInputTokenizer = UITextInputStringTokenizer(textInput: self)
        public var markedTextStyle: [NSAttributedString.Key: Any]?

        public override init(frame: CGRect) {
            super.init(frame: frame)
            backgroundColor = .black
            scrollView.delegate = self
            scrollView.minimumZoomScale = 0.3
            scrollView.maximumZoomScale = 4
            scrollView.bouncesZoom = true
            scrollView.alwaysBounceVertical = false
            scrollView.contentInsetAdjustmentBehavior = .never
            scrollView.delaysContentTouches = false
            scrollView.keyboardDismissMode = .none
            addSubview(scrollView)
            scrollView.addSubview(grid)

            markedLabel.isHidden = true
            markedLabel.textColor = .white
            markedLabel.backgroundColor = UIColor.darkGray
            grid.addSubview(markedLabel)

            let tap = UITapGestureRecognizer(target: self, action: #selector(handleTap))
            addGestureRecognizer(tap)
            let oneFinger = UIPanGestureRecognizer(target: self, action: #selector(handleScrollback(_:)))
            oneFinger.maximumNumberOfTouches = 1
            oneFinger.delegate = self
            scrollView.addGestureRecognizer(oneFinger)
            let twoFingers = UIPanGestureRecognizer(target: self, action: #selector(handleScrollback(_:)))
            twoFingers.minimumNumberOfTouches = 2
            twoFingers.maximumNumberOfTouches = 2
            twoFingers.delegate = self
            scrollView.addGestureRecognizer(twoFingers)
            isAccessibilityElement = true
            accessibilityLabel = "终端"
        }

        @available(*, unavailable)
        required init?(coder: NSCoder) {
            fatalError("init(coder:) is not supported")
        }

        // MARK: 画

        /// 换一份 VT（重新 `Attach` 后新建的），整屏重画。
        public func show(_ terminal: VTerminal?, settings: TermSettings) {
            self.terminal = terminal
            grid.settings = settings
            grid.screen = ScreenFrame()
            backgroundColor = Self.uiColor(settings.background)
            refreshNow()
        }

        public func updateSettings(_ settings: TermSettings) {
            grid.settings = settings
            backgroundColor = Self.uiColor(settings.background)
            grid.setNeedsDisplay()
            scheduleRefresh()
        }

        /// VT 变了：下一轮主循环再从 render state 抄变了的行，一轮里多块输出只刷新一次。
        public func scheduleRefresh() {
            guard !refreshScheduled else { return }
            refreshScheduled = true
            Task { @MainActor [weak self] in
                guard let self else { return }
                refreshScheduled = false
                refreshNow()
            }
        }

        public func ringBell() {
            // 连着响铃时最多每 200 毫秒震一下。
            let now = ContinuousClock.now
            guard now - lastBell > .milliseconds(200) else { return }
            lastBell = now
            bellFeedback.impactOccurred()
        }

        private func refreshNow() {
            guard let terminal else {
                grid.setNeedsDisplay()
                return
            }
            // 程序在同步输出：留着上一帧，一会儿再看（最多冻结一秒，见 `VTerminal.isRenderHeld`）。
            if terminal.isRenderHeld {
                Task { @MainActor [weak self] in
                    try? await Task.sleep(for: .milliseconds(50))
                    self?.scheduleRefresh()
                }
                return
            }
            let oldCursor = grid.screen.cursor
            let oldSize = CGSize(width: grid.screen.columns, height: grid.screen.rows)
            let change = terminal.refresh(&grid.screen)
            if oldSize != CGSize(width: grid.screen.columns, height: grid.screen.rows) {
                layoutGrid()
            }
            if change.full {
                grid.setNeedsDisplay()
            } else {
                for row in change.rows {
                    grid.setNeedsDisplay(grid.rect(forRow: row))
                }
                if change.cursorChanged {
                    if let oldCursor { grid.setNeedsDisplay(grid.rect(forRow: oldCursor.row)) }
                    grid.invalidateCursor()
                }
            }
            positionMarkedText()
            let scrolledBack = !terminal.scrollbar.atBottom
            if scrolledBack != lastScrolledBack {
                lastScrolledBack = scrolledBack
                delegate?.terminalView(self, didScrollBack: scrolledBack)
            }
        }

        // MARK: 布局、缩放

        public override func layoutSubviews() {
            super.layoutSubviews()
            if scrollView.frame != bounds {
                scrollView.frame = bounds
                applyDefaultZoom()
            }
            centerContent()
            reportFitSize()
        }

        /// 网格的行列数变了：按新的大小排，没手动缩放过时重新按屏幕宽度缩放。
        private func layoutGrid() {
            let zoom = scrollView.zoomScale
            scrollView.zoomScale = 1
            grid.frame = CGRect(origin: .zero, size: grid.gridSize)
            scrollView.contentSize = grid.frame.size
            if userZoomed {
                scrollView.zoomScale = zoom
            } else {
                applyDefaultZoom()
            }
            updateRasterScale()
            centerContent()
        }

        /// 默认的缩放：网格比屏幕宽时缩到正好一屏宽，但字不小于 5 点，更宽的就横着拖。
        private func applyDefaultZoom() {
            guard !userZoomed, grid.gridSize.width > 0, bounds.width > 0 else { return }
            let minimum = 5 / grid.font.size
            let fitWidth = bounds.width / grid.gridSize.width
            scrollView.minimumZoomScale = min(minimum, fitWidth, 1)
            scrollView.zoomScale = max(min(fitWidth, 1), minimum)
            updateRasterScale()
        }

        public func viewForZooming(in scrollView: UIScrollView) -> UIView? {
            grid
        }

        public func scrollViewDidZoom(_ scrollView: UIScrollView) {
            centerContent()
        }

        public func scrollViewWillBeginZooming(_ scrollView: UIScrollView, with view: UIView?) {
            userZoomed = true
        }

        public func scrollViewDidEndZooming(_ scrollView: UIScrollView, with view: UIView?, atScale scale: CGFloat) {
            updateRasterScale()
        }

        /// 按当前缩放比例重画，放大后字不发虚。位图的边长和总像素有上限，免得超大的网格吃光内存。
        private func updateRasterScale() {
            let screenScale = window?.screen.scale ?? traitCollection.displayScale
            var scale = max(scrollView.zoomScale, 0.1) * screenScale
            let size = grid.bounds.size
            if size.width > 0, size.height > 0 {
                scale = min(scale, 8192 / size.width, 8192 / size.height, sqrt(24_000_000 / (size.width * size.height)))
            }
            scale = max(scale, 1)
            if abs(grid.contentScaleFactor - scale) > 0.01 {
                grid.contentScaleFactor = scale
                grid.setNeedsDisplay()
            }
        }

        /// 网格比屏幕窄时左右居中；竖直方向贴着顶。
        private func centerContent() {
            let horizontal = max(0, (scrollView.bounds.width - scrollView.contentSize.width) / 2)
            scrollView.contentInset = UIEdgeInsets(top: 0, left: horizontal, bottom: 0, right: 0)
        }

        /// 「适配本机屏幕」要的网格：按 1 倍缩放下的字体，正好铺满视图。
        public var fitSize: GridSize {
            Self.gridSize(
                fitting: bounds.size, scale: window?.screen.scale ?? traitCollection.displayScale, font: grid.font)
        }

        /// 用终端的默认字体铺满 `size`（点）的网格；新开会话时按它定尺寸。
        public static func gridSize(fitting size: CGSize, scale: CGFloat) -> GridSize {
            gridSize(fitting: size, scale: scale, font: TerminalFont(size: 13))
        }

        static func gridSize(fitting size: CGSize, scale: CGFloat, font: TerminalFont) -> GridSize {
            let columns = max(10, Int(size.width / font.cellWidth))
            let rows = max(4, Int(size.height / font.cellHeight))
            return GridSize(
                cols: UInt16(min(columns, 500)), rows: UInt16(min(rows, 300)),
                cellWidthPx: UInt16((font.cellWidth * scale).rounded()),
                cellHeightPx: UInt16((font.cellHeight * scale).rounded()))
        }

        private func reportFitSize() {
            guard bounds.width > 0, bounds.height > 0 else { return }
            let size = fitSize
            guard size != lastFitSize else { return }
            lastFitSize = size
            delegate?.terminalView(self, fitSizeDidChange: size)
        }

        /// 适配屏幕以后网格正好铺满：回到 1 倍缩放，以后照常按屏幕宽度自动缩放。
        public func resetZoom() {
            userZoomed = false
            scrollView.setZoomScale(1, animated: false)
            applyDefaultZoom()
            centerContent()
        }

        /// 把光标所在的位置滚进可见区域（键盘弹出、打字时）。
        public func revealCursor() {
            guard let rect = grid.cursorRect else { return }
            scrollView.scrollRectToVisible(grid.convert(rect, to: scrollView).insetBy(dx: -8, dy: -8), animated: false)
        }

        // MARK: 回滚历史

        /// 一个手指上下拖：网格竖直方向放得下时才看回滚历史（放不下时让滚动视图拖网格）；两个手指上下拖
        /// 总是看回滚历史。
        public override func gestureRecognizerShouldBegin(_ gestureRecognizer: UIGestureRecognizer) -> Bool {
            guard let pan = gestureRecognizer as? UIPanGestureRecognizer, pan.view === scrollView else {
                return super.gestureRecognizerShouldBegin(gestureRecognizer)
            }
            let velocity = pan.velocity(in: scrollView)
            guard abs(velocity.y) > abs(velocity.x) else { return false }
            if pan.maximumNumberOfTouches == 1 {
                return scrollView.contentSize.height <= scrollView.bounds.height + 1
            }
            return true
        }

        public func gestureRecognizer(
            _ gestureRecognizer: UIGestureRecognizer,
            shouldRecognizeSimultaneouslyWith otherGestureRecognizer: UIGestureRecognizer
        ) -> Bool {
            true
        }

        @objc private func handleScrollback(_ pan: UIPanGestureRecognizer) {
            guard let terminal else { return }
            let rowHeight = grid.font.cellHeight * scrollView.zoomScale
            switch pan.state {
            case .began:
                scrollbackRemainder = 0
            case .changed:
                scrollbackRemainder += pan.translation(in: self).y
                pan.setTranslation(.zero, in: self)
                let rows = Int(scrollbackRemainder / rowHeight)
                guard rows != 0 else { return }
                scrollbackRemainder -= CGFloat(rows) * rowHeight
                // 往下拖看更早的内容：视口往上滚。
                terminal.scroll(by: -rows)
                refreshNow()
            default:
                break
            }
        }

        /// 回到最底下，跟着新输出走。
        public func scrollToBottom() {
            terminal?.scrollToBottom()
            refreshNow()
        }

        // MARK: 焦点

        public override var canBecomeFirstResponder: Bool { true }

        public override var inputAccessoryView: UIView? { accessoryBar }

        @discardableResult
        public override func becomeFirstResponder() -> Bool {
            let became = super.becomeFirstResponder()
            grid.hasKeyboardFocus = isFirstResponder
            return became
        }

        @discardableResult
        public override func resignFirstResponder() -> Bool {
            let resigned = super.resignFirstResponder()
            grid.hasKeyboardFocus = isFirstResponder
            return resigned
        }

        @objc private func handleTap() {
            if !isFirstResponder {
                becomeFirstResponder()
            }
            revealCursor()
        }

        // MARK: 输入

        /// 发一份输入给上层；打字时回到最底下、把光标露出来。
        func emit(_ input: TerminalInput) {
            if lastScrolledBack {
                terminal?.scrollToBottom()
                scheduleRefresh()
            }
            delegate?.terminalView(self, didInput: input)
            revealCursor()
        }

        /// 打出一段文字：换行按回车键发，粘住 Ctrl 时第一个字按 Ctrl 组合键发。
        func typeText(_ text: String) {
            var pending = ""
            for character in text {
                if character == "\n" || character == "\r" {
                    flushText(&pending)
                    emit(.key(KeyInput(key: .enter)))
                } else if controlLatched {
                    flushText(&pending)
                    controlLatched = false
                    if let key = KeyInput.typing(Character(character.lowercased()), modifiers: .control) {
                        emit(.key(key))
                    } else {
                        pending.append(character)
                    }
                } else {
                    pending.append(character)
                }
            }
            flushText(&pending)
        }

        private func flushText(_ pending: inout String) {
            guard !pending.isEmpty else { return }
            emit(.text(pending))
            pending = ""
        }

        /// 辅助栏或硬件键盘按的一个键；粘住的 Ctrl 加在上面。
        func press(_ key: TerminalKey, modifiers: KeyModifiers = []) {
            var modifiers = modifiers
            if controlLatched {
                modifiers.insert(.control)
                controlLatched = false
            }
            emit(.key(KeyInput(key: key, modifiers: modifiers)))
        }

        public override func canPerformAction(_ action: Selector, withSender sender: Any?) -> Bool {
            if action == #selector(paste(_:)) {
                return UIPasteboard.general.hasStrings
            }
            return super.canPerformAction(action, withSender: sender)
        }

        public override func paste(_ sender: Any?) {
            guard let text = UIPasteboard.general.string, !text.isEmpty else { return }
            emit(.paste(text))
        }

        // MARK: 硬件键盘

        public override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
            // 输入法正在组字时按键都归输入法。
            guard markedText.isEmpty else {
                super.pressesBegan(presses, with: event)
                return
            }
            var unhandled = Set<UIPress>()
            for press in presses {
                if let key = press.key, let input = Self.keyInput(for: key) {
                    emit(.key(input))
                    startRepeating(input)
                } else {
                    unhandled.insert(press)
                }
            }
            if !unhandled.isEmpty {
                super.pressesBegan(unhandled, with: event)
            }
        }

        public override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
            stopRepeating()
            super.pressesEnded(presses, with: event)
        }

        public override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
            stopRepeating()
            super.pressesCancelled(presses, with: event)
        }

        /// 按住不放时连发：0.4 秒后每 50 毫秒一次。
        private func startRepeating(_ input: KeyInput) {
            repeatTask?.cancel()
            repeatTask = Task { @MainActor [weak self] in
                try? await Task.sleep(for: .milliseconds(400))
                while !Task.isCancelled {
                    self?.emit(.key(input))
                    try? await Task.sleep(for: .milliseconds(50))
                }
            }
        }

        private func stopRepeating() {
            repeatTask?.cancel()
            repeatTask = nil
        }

        /// 硬件键盘上要自己编码的键：功能键、方向键，以及按着 Ctrl 或 Option 的组合；普通打字交给系统，
        /// 经输入法走 `insertText`。按着 Command 的留给系统（⌘V 粘贴等）。
        static func keyInput(for key: UIKey) -> KeyInput? {
            let flags = key.modifierFlags
            if flags.contains(.command) { return nil }
            var modifiers: KeyModifiers = []
            if flags.contains(.shift) { modifiers.insert(.shift) }
            if flags.contains(.control) { modifiers.insert(.control) }
            if flags.contains(.alternate) { modifiers.insert(.alt) }
            let special: TerminalKey? =
                switch key.keyCode {
                case .keyboardEscape: .escape
                case .keyboardUpArrow: .up
                case .keyboardDownArrow: .down
                case .keyboardLeftArrow: .left
                case .keyboardRightArrow: .right
                case .keyboardHome: .home
                case .keyboardEnd: .end
                case .keyboardPageUp: .pageUp
                case .keyboardPageDown: .pageDown
                case .keyboardDeleteForward: .delete
                case .keyboardInsert: .insert
                case .keyboardTab: .tab
                case .keyboardF1: .function(1)
                case .keyboardF2: .function(2)
                case .keyboardF3: .function(3)
                case .keyboardF4: .function(4)
                case .keyboardF5: .function(5)
                case .keyboardF6: .function(6)
                case .keyboardF7: .function(7)
                case .keyboardF8: .function(8)
                case .keyboardF9: .function(9)
                case .keyboardF10: .function(10)
                case .keyboardF11: .function(11)
                case .keyboardF12: .function(12)
                default: nil
                }
            if let special {
                return KeyInput(key: special, modifiers: modifiers)
            }
            guard modifiers.contains(.control) || modifiers.contains(.alt) else { return nil }
            switch key.keyCode {
            case .keyboardReturnOrEnter: return KeyInput(key: .enter, modifiers: modifiers)
            case .keyboardDeleteOrBackspace: return KeyInput(key: .backspace, modifiers: modifiers)
            case .keyboardSpacebar: return KeyInput(key: .space, modifiers: modifiers, unshifted: " ")
            default: break
            }
            guard let base = key.charactersIgnoringModifiers.lowercased().first,
                let (physical, _) = TerminalKey.forTyped(base)
            else { return nil }
            let typed = key.characters
            // Option 打出了别的字（比如 ⌥E 的 ´）：这个修饰键已经体现在文字里了。
            let consumed: KeyModifiers =
                modifiers.contains(.alt) && typed != key.charactersIgnoringModifiers ? .alt : []
            return KeyInput(
                key: physical, modifiers: modifiers, text: typed.isEmpty ? nil : typed, unshifted: base,
                consumedModifiers: consumed)
        }

        // MARK: 工具

        static func uiColor(_ rgb: Rgb) -> UIColor {
            UIColor(red: CGFloat(rgb.r) / 255, green: CGFloat(rgb.g) / 255, blue: CGFloat(rgb.b) / 255, alpha: 1)
        }

        /// 组字中的文字画在光标处，带下划线。
        func positionMarkedText() {
            guard !markedText.isEmpty, let rect = grid.cursorRect else {
                markedLabel.isHidden = true
                return
            }
            markedLabel.isHidden = false
            markedLabel.attributedText = NSAttributedString(
                string: markedText,
                attributes: [
                    .font: UIFont.monospacedSystemFont(ofSize: grid.font.size, weight: .regular),
                    .underlineStyle: NSUnderlineStyle.single.rawValue,
                    .foregroundColor: Self.uiColor(grid.screen.foreground),
                ])
            markedLabel.backgroundColor = Self.uiColor(grid.screen.background)
            markedLabel.sizeToFit()
            markedLabel.frame.origin = rect.origin
            markedLabel.frame.size.height = rect.height
        }

        /// 光标在本视图坐标系里的矩形，输入法的候选框按它摆。
        var cursorRectInSelf: CGRect {
            guard let rect = grid.cursorRect else { return CGRect(x: 0, y: 0, width: 1, height: 20) }
            return grid.convert(rect, to: self)
        }
    }
#endif

#if os(iOS)
    extension TerminalView: TerminalDisplay {
        public func terminalDidReset(_ terminal: VTerminal?, settings: TermSettings) {
            show(terminal, settings: settings)
        }

        public func terminalContentDidChange() {
            scheduleRefresh()
        }

        public func terminalSettingsDidChange(_ settings: TermSettings) {
            updateSettings(settings)
        }

        public func terminalDidRingBell() {
            ringBell()
        }

        public func terminalWillFitScreen() {
            resetZoom()
        }
    }
#endif
