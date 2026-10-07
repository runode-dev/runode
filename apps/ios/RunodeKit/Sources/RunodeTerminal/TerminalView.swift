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
        /// 终端拿到或交出了键盘焦点（软键盘弹出、收起）。
        func terminalView(_ view: TerminalView, keyboardVisible visible: Bool)
    }

    /// 终端视图：按宿主给的网格尺寸画（网格比屏幕大时缩放到不小于可读字号，超出的横向平移；有输出或
    /// 打字时视口跟着光标），上下拖动看回滚历史，接软键盘（含输入法的组字）、硬件键盘和键盘上方的
    /// 辅助栏。字号跟着系统的动态字体走。网格比视图矮时贴着底边放，上面空出来的是终端背景色。
    ///
    /// 结构：自己是第一响应者，负责键盘输入（`UITextInput`）；里面一个 `UIScrollView` 管缩放和平移，
    /// 再里面一个和网格一样大的容器，容器里 `TerminalGridView` 用 CoreText 画网格。网格按字体的自然
    /// 大小排，缩放靠滚动视图的 `zoomScale`（缩放的是容器），缩放结束后按新的比例重画，字不发虚。
    /// 平滑滚动错开不足一行时，网格在容器里往下挪，容器顶上露出另画的视口上面那一行，底下多出的被
    /// 容器裁掉；挪的是现成的位图，不用每帧重画。
    public final class TerminalView: UIView, UIScrollViewDelegate, UIGestureRecognizerDelegate, TerminalDisplay {
        public weak var delegate: (any TerminalViewDelegate)?

        /// 正在画的那份 VT，由视图模型给；视图只读它和滚它的视口。
        private var terminal: VTerminal?
        private let scrollView = UIScrollView()
        private let grid = TerminalGridView()
        /// 缩放的对象：装着网格和视口上面那一行，平滑滚动错开时裁掉露出网格的部分。
        private let gridContainer = UIView()
        /// 视口上面那一行（`ScreenFrame.above`），平滑滚动错开时从顶上露出来。
        private let aboveGrid = TerminalGridView()
        private let markedLabel = UILabel()
        private lazy var accessoryBar = KeyboardAccessoryBar(owner: self)
        /// 软键盘收着时界面底部那条按键栏，由 `makeRestingKeyBar` 给出去。
        private weak var restingBar: KeyboardAccessoryBar?
        private var refreshScheduled = false
        /// 用户自己捏合缩放过：网格尺寸变了也不再自动按屏幕宽度缩放。
        private var userZoomed = false
        private var lastFitSize: GridSize?
        private var lastScrolledBack = false
        private var scrollbackRemainder: CGFloat = 0
        /// 松手后接着滚的惯性：速度（点每秒，往下拖为正）和驱动它的显示刷新。
        private var momentumVelocity: CGFloat = 0
        private var momentumLink: CADisplayLink?
        /// 拖动开始时手指所在的格子，程序自己管滚动时滚轮事件报在这里。
        private var scrollAnchor = (column: 0, row: 0)
        private var repeatTask: Task<Void, Never>?
        private let bellFeedback = UIImpactFeedbackGenerator(style: .light)
        private var lastBell = ContinuousClock.now - .seconds(1)
        /// 用户最近一次自己拖动、缩放网格的时刻：之后一会儿不自动跟着光标走，免得和手指抢。
        private var lastUserScroll = ContinuousClock.now - .seconds(10)
        /// 按手机屏幕决定尺寸（网格铺满视图、不缩放）；为假时跟随电脑，按可读字号缩放。
        private var fitsPhone = false

        /// 视图顶上被叠着的东西（断线横幅）挡住的高度。网格上方的空白不够时，在滚动区顶上让出这段，
        /// 网格停在底部、被挡的几行往上拖就看得到；不改网格的尺寸，免得断线、重连时让宿主多改两次尺寸。
        public var topObstruction: CGFloat = 0 {
            didSet {
                guard abs(topObstruction - oldValue) > 0.5 else { return }
                centerContent(stickToBottom: true)
            }
        }

        /// 用户在设置里定的字号（点）；为空时跟随系统的动态字体。
        public var fontSizeOverride: CGFloat? {
            didSet {
                guard fontSizeOverride != oldValue else { return }
                contentSizeDidChange()
            }
        }

        /// 程序响铃时震一下。
        public var bellHaptics = true

        /// 默认字号（13 点）和可读的最小字号（9 点），都按动态字体放大缩小；用户定了字号时默认字号用它，
        /// 可读的最小字号不超过它。
        static func fontSizes(for traits: UITraitCollection, override: CGFloat? = nil) -> (
            base: CGFloat, readable: CGFloat
        ) {
            let metrics = UIFontMetrics(forTextStyle: .body)
            let base = override ?? min(max(metrics.scaledValue(for: 13, compatibleWith: traits), 11), 28)
            let readable = min(max(metrics.scaledValue(for: 9, compatibleWith: traits), 9), 20)
            return (base, min(readable, base))
        }

        /// 辅助栏上粘住的 Ctrl：下一个打的字按 Ctrl 组合键发。
        var controlLatched = false {
            didSet {
                accessoryBar.setControlLatched(controlLatched)
                restingBar?.setControlLatched(controlLatched)
            }
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
            scrollView.addSubview(gridContainer)
            gridContainer.addSubview(aboveGrid)
            gridContainer.addSubview(grid)
            aboveGrid.isHidden = true

            markedLabel.isHidden = true
            markedLabel.textColor = .white
            markedLabel.backgroundColor = UIColor.darkGray
            grid.addSubview(markedLabel)

            let tap = UITapGestureRecognizer(target: self, action: #selector(handleTap(_:)))
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
            // 手指一按下就停住惯性，和系统的滚动视图一样；不认成别的手势，也不拦触摸。
            let touchDown = UILongPressGestureRecognizer(target: self, action: #selector(handleTouchDown(_:)))
            touchDown.minimumPressDuration = 0
            touchDown.cancelsTouchesInView = false
            touchDown.delegate = self
            scrollView.addGestureRecognizer(touchDown)
            isAccessibilityElement = true
            accessibilityLabel = "终端"
            accessibilityHint = "轻点两下打开键盘"
            accessibilityTraits = [.allowsDirectInteraction, .updatesFrequently]
            setFont(TerminalFont(size: Self.fontSizes(for: traitCollection, override: fontSizeOverride).base))
            registerForTraitChanges([UITraitPreferredContentSizeCategory.self]) { (view: TerminalView, _) in
                view.contentSizeDidChange()
            }
        }

        /// 动态字体或设置里的字号改了：换字号，重新排网格、算「适配手机」的尺寸。
        private func contentSizeDidChange() {
            setFont(TerminalFont(size: Self.fontSizes(for: traitCollection, override: fontSizeOverride).base))
            userZoomed = false
            layoutGrid()
            reportFitSize()
        }

        private func setFont(_ font: TerminalFont) {
            grid.font = font
            aboveGrid.font = font
            grid.setNeedsDisplay()
            aboveGrid.setNeedsDisplay()
        }

        /// VoiceOver 读屏幕底部几行有字的内容。
        public override var accessibilityValue: String? {
            get { grid.screen.lines.filter { !$0.isEmpty }.suffix(5).joined(separator: "\n") }
            set {}
        }

        @available(*, unavailable)
        required init?(coder: NSCoder) {
            fatalError("init(coder:) is not supported")
        }

        // MARK: 画

        /// 换一份 VT（重新 `Attach` 后新建的），整屏重画。
        public func terminalDidReset(_ terminal: VTerminal?, settings: TermSettings) {
            stopMomentum()
            self.terminal = terminal
            grid.settings = settings
            aboveGrid.settings = settings
            grid.screen = ScreenFrame()
            backgroundColor = Self.uiColor(settings.background)
            refreshNow()
        }

        public func terminalSettingsDidChange(_ settings: TermSettings) {
            grid.settings = settings
            aboveGrid.settings = settings
            backgroundColor = Self.uiColor(settings.background)
            grid.setNeedsDisplay()
            aboveGrid.setNeedsDisplay()
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

        public func terminalContentDidChange() {
            scheduleRefresh()
        }

        public func terminalDidRingBell() {
            guard bellHaptics else { return }
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
            if change.full || change.aboveChanged {
                updateAboveRow()
            }
            applyScrollOffset()
            positionMarkedText()
            let scrolledBack = !terminal.viewportAtBottom
            if scrolledBack != lastScrolledBack {
                lastScrolledBack = scrolledBack
                delegate?.terminalView(self, didScrollBack: scrolledBack)
            }
            followCursor()
        }

        /// 有输出时视口跟着光标走（网格比屏幕宽、横着拖的时候尤其要）；用户正在拖、刚拖过，或者在看
        /// 回滚历史时不动。
        private func followCursor() {
            guard !lastScrolledBack, !scrollView.isDragging, !scrollView.isDecelerating, !scrollView.isZooming,
                ContinuousClock.now - lastUserScroll > .seconds(2)
            else { return }
            revealCursor()
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
            gridContainer.frame = CGRect(origin: .zero, size: grid.gridSize)
            grid.frame = gridContainer.bounds
            applyScrollOffset()
            scrollView.contentSize = gridContainer.frame.size
            if userZoomed {
                scrollView.zoomScale = zoom
            } else {
                applyDefaultZoom()
            }
            updateRasterScale()
            centerContent()
        }

        /// 默认的缩放：适配手机时不缩放（网格本来就按视图排）；跟随电脑时网格比屏幕宽就缩到正好一屏宽，
        /// 但字不小于可读字号，再宽的横着拖。捏合也不能缩到可读字号以下。
        private func applyDefaultZoom() {
            guard grid.gridSize.width > 0, bounds.width > 0 else { return }
            let sizes = Self.fontSizes(for: traitCollection, override: fontSizeOverride)
            let readable = min(sizes.readable / grid.font.size, 1)
            scrollView.minimumZoomScale = readable
            guard !userZoomed else { return }
            let fitWidth = bounds.width / grid.gridSize.width
            scrollView.zoomScale = fitsPhone ? 1 : max(min(fitWidth, 1), readable)
            updateRasterScale()
        }

        public func viewForZooming(in scrollView: UIScrollView) -> UIView? {
            gridContainer
        }

        public func scrollViewDidZoom(_ scrollView: UIScrollView) {
            centerContent()
        }

        public func scrollViewWillBeginZooming(_ scrollView: UIScrollView, with view: UIView?) {
            userZoomed = true
            lastUserScroll = .now
        }

        public func scrollViewWillBeginDragging(_ scrollView: UIScrollView) {
            lastUserScroll = .now
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
                aboveGrid.contentScaleFactor = scale
                grid.setNeedsDisplay()
                aboveGrid.setNeedsDisplay()
            }
        }

        /// 把视口上面那一行抄成只有一行、没有光标的一屏，交给 `aboveGrid` 画。
        private func updateAboveRow() {
            var row = ScreenFrame()
            row.columns = grid.screen.columns
            row.rows = grid.screen.above.isEmpty ? 0 : 1
            row.cells = grid.screen.above.isEmpty ? [] : [grid.screen.above]
            row.background = grid.screen.background
            row.foreground = grid.screen.foreground
            row.cursorColor = grid.screen.cursorColor
            aboveGrid.screen = row
            aboveGrid.setNeedsDisplay()
        }

        /// 平滑滚动的错开量：网格在容器里往下挪不足一行，视口上面那一行跟在它上面。只挪位置，不重画。
        private func applyScrollOffset() {
            let rowHeight = grid.font.cellHeight
            let shift = CGFloat(grid.screen.scrollOffset) * rowHeight
            let width = gridContainer.bounds.width
            if grid.frame.minY != shift {
                grid.frame.origin.y = shift
            }
            aboveGrid.frame = CGRect(x: 0, y: shift - rowHeight, width: width, height: rowHeight)
            aboveGrid.isHidden = shift == 0
            // 不错开时不裁，组字的文字超出网格右边也看得见。
            gridContainer.clipsToBounds = shift != 0
        }

        /// 网格比屏幕窄时左右居中；比屏幕矮时贴着底边（靠近键盘和底栏，新输出在那里），上面空着的地方
        /// 露出视图的背景，也就是终端的背景色。
        private func centerContent(stickToBottom: Bool = false) {
            let horizontal = max(0, (scrollView.bounds.width - scrollView.contentSize.width) / 2)
            let slack = scrollView.bounds.height - scrollView.contentSize.height
            let top = max(0, slack, topObstruction)
            let inset = UIEdgeInsets(top: top, left: horizontal, bottom: 0, right: 0)
            if scrollView.contentInset != inset {
                scrollView.contentInset = inset
            }
            // 放得下的方向上没有可滚的：停在正好露出整个网格的位置。顶上让出了地方、网格放不下时：整个
            // 网格往下挪出横幅的高度后光标还看得见（光标下面多半是空行），就这样挪；否则停在最底下。
            var offset = scrollView.contentOffset
            if scrollView.contentSize.height + top <= scrollView.bounds.height + 0.5 {
                offset.y = -top
            } else if stickToBottom {
                let cursorBottom = grid.cursorRect.map { grid.convert($0, to: scrollView).maxY } ?? .infinity
                offset.y =
                    cursorBottom + top <= scrollView.bounds.height
                    ? -top : scrollView.contentSize.height - scrollView.bounds.height
            }
            if horizontal > 0 { offset.x = -horizontal }
            if offset != scrollView.contentOffset, !scrollView.isZooming {
                scrollView.contentOffset = offset
            }
        }

        /// 「适配本机屏幕」要的网格：按 1 倍缩放下的字体，正好铺满视图。
        public var fitSize: GridSize {
            Self.gridSize(
                fitting: bounds.size, scale: window?.screen.scale ?? traitCollection.displayScale, font: grid.font)
        }

        /// 用终端的默认字号（按 `contentSize` 这档动态字体，或者用户定的 `fontSize`）铺满 `size`（点）的
        /// 网格；新开会话时按它定尺寸。
        public static func gridSize(
            fitting size: CGSize, scale: CGFloat, contentSize: UIContentSizeCategory = .large,
            fontSize: CGFloat? = nil
        ) -> GridSize {
            let traits = UITraitCollection(preferredContentSizeCategory: contentSize)
            let base = fontSizes(for: traits, override: fontSize).base
            return gridSize(fitting: size, scale: scale, font: TerminalFont(size: base))
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

        /// 尺寸方式换了：丢掉手动的缩放，按新的方式重新定默认缩放。
        public func terminalSizeModeDidChange(fitsPhone fits: Bool) {
            fitsPhone = fits
            userZoomed = false
            applyDefaultZoom()
            centerContent()
        }

        /// 唤起键盘，把光标露出来。
        public func showKeyboard() {
            if !isFirstResponder {
                becomeFirstResponder()
            }
            revealCursor()
        }

        public func terminalShowKeyboard() {
            showKeyboard()
        }

        /// 软键盘收着时放在界面底部的按键栏：和键盘上方的辅助栏一样的键，按了直接发给这个终端，最后
        /// 一个键打开软键盘。不用先弹软键盘就能按 Esc、Ctrl、方向键。
        public func makeRestingKeyBar() -> UIView {
            let bar = KeyboardAccessoryBar(owner: self, resting: true)
            bar.setControlLatched(controlLatched)
            restingBar = bar
            return bar
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
            switch pan.state {
            case .began:
                stopMomentum()
                scrollbackRemainder = 0
                let point = pan.location(in: grid)
                scrollAnchor = (
                    column: Int(point.x / max(grid.font.cellWidth, 1)), row: Int(point.y / max(grid.font.cellHeight, 1))
                )
            case .changed:
                let distance = pan.translation(in: self).y
                pan.setTranslation(.zero, in: self)
                scrollContent(by: distance)
            case .ended:
                startMomentum(velocity: pan.velocity(in: self).y)
            default:
                break
            }
        }

        @objc private func handleTouchDown(_ press: UILongPressGestureRecognizer) {
            if press.state == .began { stopMomentum() }
        }

        /// 手指（或惯性）往下挪了 `distance` 点，往下拖看更早的内容：回滚历史跟着手指按像素滚；程序自己
        /// 管滚动时（全屏的 agent 界面、开着备用滚动的分页器）只能按整行，攒够一行给它发一个滚轮。返回假
        /// 表示回滚历史已经到头、挪不动了。
        @discardableResult
        private func scrollContent(by distance: CGFloat) -> Bool {
            guard let terminal else { return false }
            let rowHeight = grid.font.cellHeight * scrollView.zoomScale
            guard rowHeight > 0 else { return false }
            guard terminal.programScrolls else {
                guard terminal.scrollSmoothly(lines: Double(distance / rowHeight)) else { return false }
                refreshNow()
                return true
            }
            scrollbackRemainder += distance
            let rows = Int(scrollbackRemainder / rowHeight)
            guard rows != 0 else { return true }
            scrollbackRemainder -= CGFloat(rows) * rowHeight
            delegate?.terminalView(
                self, didInput: .wheel(lines: -rows, column: scrollAnchor.column, row: scrollAnchor.row))
            return true
        }

        /// 松手时还在快速拖：按系统滚动视图的减速率接着滚，滚到头或者慢下来就停。
        private func startMomentum(velocity: CGFloat) {
            stopMomentum()
            guard abs(velocity) > 200 else { return }
            momentumVelocity = velocity
            let link = CADisplayLink(target: self, selector: #selector(stepMomentum(_:)))
            link.add(to: .main, forMode: .common)
            momentumLink = link
        }

        @objc private func stepMomentum(_ link: CADisplayLink) {
            let elapsed = min(link.targetTimestamp - link.timestamp, 0.05)
            momentumVelocity *= pow(UIScrollView.DecelerationRate.normal.rawValue, elapsed * 1000)
            guard abs(momentumVelocity) > 30, scrollContent(by: momentumVelocity * elapsed) else {
                stopMomentum()
                return
            }
        }

        private func stopMomentum() {
            momentumLink?.invalidate()
            momentumLink = nil
            momentumVelocity = 0
        }

        /// 回到最底下，跟着新输出走。
        public func scrollToBottom() {
            stopMomentum()
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
            delegate?.terminalView(self, keyboardVisible: isFirstResponder)
            return became
        }

        @discardableResult
        public override func resignFirstResponder() -> Bool {
            let resigned = super.resignFirstResponder()
            grid.hasKeyboardFocus = isFirstResponder
            delegate?.terminalView(self, keyboardVisible: isFirstResponder)
            return resigned
        }

        /// 轻点：程序开着鼠标上报时当成一次点击发给它（全屏 agent 界面里「跳到底部」这类按钮）。只有点在
        /// 光标那行的上一行及以下（输入框和它下面的状态栏）才弹键盘，点上面的对话、输出不弹，键盘开着时
        /// 收起；看不到光标时，程序管鼠标就只点击，不管就照旧弹键盘。
        @objc private func handleTap(_ tap: UITapGestureRecognizer) {
            let point = tap.location(in: grid)
            let row = Int((point.y / grid.font.cellHeight).rounded(.down))
            let tracking = terminal?.mouseTracking == true
            if tracking, grid.bounds.contains(point) {
                let column = Int(point.x / grid.font.cellWidth)
                delegate?.terminalView(self, didInput: .click(column: column, row: row))
            }
            let atInput = grid.screen.cursor.map { row >= $0.row - 1 } ?? !tracking
            if atInput {
                showKeyboard()
            } else if isFirstResponder {
                resignFirstResponder()
            }
        }

        /// VoiceOver 下轻点两下：直接打开键盘，不按位置判断。
        public override func accessibilityActivate() -> Bool {
            showKeyboard()
            return true
        }

        // MARK: 输入

        /// 发一份输入给上层；打字时回到最底下、把光标露出来。
        func emit(_ input: TerminalInput) {
            stopMomentum()
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
            // F1 到 F12 的 HID 用法码是连着的。
            let function = key.keyCode.rawValue - UIKeyboardHIDUsage.keyboardF1.rawValue + 1
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
                default: (1...12).contains(function) ? .function(function) : nil
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
