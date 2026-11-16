#if os(iOS)
    import RunodeProtocol
    import UIKit
    import UniformTypeIdentifiers

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
        /// 辅助栏上的麦克风键按下、松开或者滑出去放开。
        func terminalView(_ view: TerminalView, dictation press: DictationPress)
        /// 辅助栏上的图片键按下了：用户要挑图片或视频发给这个终端。
        func terminalViewRequestsMedia(_ view: TerminalView)
        /// 粘贴时剪贴板上没有文字、有图片：`data` 是那张图片的文件内容。
        func terminalView(_ view: TerminalView, didPasteImage data: Data)
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
    public final class TerminalView: UIView, UIScrollViewDelegate, UIGestureRecognizerDelegate,
        @preconcurrency UIEditMenuInteractionDelegate, TerminalDisplay
    {
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
        private lazy var accessoryBar: KeyboardAccessoryBar = {
            let bar = KeyboardAccessoryBar(owner: self)
            bar.palette = keyBarPalette
            return bar
        }()
        /// 软键盘收着时界面底部那条按键栏，由 `makeRestingKeyBar` 给出去。
        private weak var restingBar: KeyboardAccessoryBar?
        /// 两条按键栏的配色。
        public var keyBarPalette: KeyBarPalette? {
            didSet {
                restingBar?.palette = keyBarPalette
                accessoryBar.palette = keyBarPalette
            }
        }
        /// 软键盘用深色还是浅色。
        public var keyboardIsDark = true {
            didSet {
                guard keyboardIsDark != oldValue else { return }
                if isFirstResponder { reloadInputViews() }
            }
        }
        private var refreshScheduled = false
        /// 下一次刷新最早在这时候，见 `scheduleRefresh`。
        private var nextRefresh = ContinuousClock.now
        /// 图片动画到点画下一帧的计时，每次刷新按新的到期时间换掉。
        private var imageFrame: Task<Void, Never>?
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
        /// 硬件键盘上按着的 Option 键（左、右）：`UIKey` 的修饰键标志分不出是哪一边。
        private var heldOptions: Set<UIKeyboardHIDUsage> = []
        private let bellFeedback = UIImpactFeedbackGenerator(style: .light)
        private var lastBell = ContinuousClock.now - .seconds(1)
        /// 用户最近一次自己拖动、缩放网格的时刻：之后一会儿不自动跟着光标走，免得和手指抢。
        private var lastUserScroll = ContinuousClock.now - .seconds(10)
        /// 按手机屏幕决定尺寸（网格铺满视图、不缩放）；为假时跟随电脑，按可读字号缩放。
        private var fitsPhone = false
        /// 软键盘正在弹出或收起；停稳了为空。这时键盘、辅助栏和底部的按键栏先后让位，视图的高度一下变好
        /// 几次，等停稳了再报「适配手机」的尺寸，宿主只改一次。
        private var keyboardMotion: KeyboardMotion?
        private var keyboardSettle: Task<Void, Never>?
        private lazy var tap = UITapGestureRecognizer(target: self, action: #selector(handleTap(_:)))
        /// 一个、两个手指上下拖看回滚历史的两个手势。
        private var scrollbackPans: [UIPanGestureRecognizer] = []

        /// 在选中：画拖柄、弹「拷贝」菜单。选区本身只存在 VT 里，`refreshNow` 每次对账。
        private(set) var selecting = false
        /// 正在拖的是选区的哪一端；没在拖时为空。
        private var selectionDrag: SelectionEnd?
        /// 拖选区时手指（拖柄时加上 `handleGrab`）最后在哪，本视图坐标：网格自动滚动、平移后照它重新取格。
        private var selectionFinger = CGPoint.zero
        /// 上次拖到的格子和拖完后被拖的是哪一端。
        private var lastDragTarget: (cell: GridCell, end: SelectionEnd)?
        /// 按住拖柄时手指到拖柄所指的格子边、行中线的偏移（网格坐标）。
        private var handleGrab = CGVector.zero
        /// 长按选中时手指所在的那格，手指离开它之前不算拖。
        private var longPressOrigin: GridCell?
        /// 拖柄拖到可见区域外时驱动自动滚动、平移的显示刷新。
        private var selectionScrollLink: CADisplayLink?
        private let startHandle = SelectionHandleView(end: .start)
        private let endHandle = SelectionHandleView(end: .end)
        private lazy var editMenu = UIEditMenuInteraction(delegate: self)
        /// 没有选区时「粘贴」菜单指着的位置，本视图坐标。
        private var menuPoint: CGPoint?
        private let selectFeedback = UIImpactFeedbackGenerator(style: .medium)
        /// 拷贝写进、粘贴读出的剪贴板。
        var pasteboard: UIPasteboard = .general

        private enum KeyboardMotion {
            case opening, closing
        }

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

        /// 能打字：会话结束了时为假，点终端不弹键盘，开着的键盘收起来。
        public var acceptsInput = true {
            didSet { if !acceptsInput, isFirstResponder { resignFirstResponder() } }
        }

        /// 默认字号和可读的最小字号，都按动态字体放大缩小；用户定了字号时默认字号用它，可读的最小字号
        /// 不超过它。
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

        /// 正在录音：两条按键栏上的麦克风键显示成按下去的样子。
        public var dictating = false {
            didSet {
                accessoryBar.setDictating(dictating)
                restingBar?.setDictating(dictating)
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
            scrollView.maximumZoomScale = Self.maximumZoom
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
            scrollbackPans = [oneFinger, twoFingers]
            let longPress = UILongPressGestureRecognizer(target: self, action: #selector(handleLongPress(_:)))
            longPress.minimumPressDuration = 0.4
            longPress.delegate = self
            scrollView.addGestureRecognizer(longPress)
            // 拖柄是本视图自己的子视图、盖在滚动视图上面，不跟着缩放变粗。
            for handle in [startHandle, endHandle] {
                handle.onPan = { [weak self] handle, pan in self?.dragHandle(handle, pan) }
                addSubview(handle)
            }
            addInteraction(editMenu)
            // 手指一按下就停住惯性，和系统的滚动视图一样；不认成别的手势，也不拦触摸。
            let touchDown = UILongPressGestureRecognizer(target: self, action: #selector(handleTouchDown(_:)))
            touchDown.minimumPressDuration = 0
            touchDown.cancelsTouchesInView = false
            touchDown.delegate = self
            scrollView.addGestureRecognizer(touchDown)
            isAccessibilityElement = true
            accessibilityLabel = String(localized: "终端")
            accessibilityHint = String(localized: "轻点两下打开键盘")
            accessibilityTraits = [.allowsDirectInteraction, .updatesFrequently]
            setFont(TerminalFont(size: Self.fontSizes(for: traitCollection, override: fontSizeOverride).base))
            registerForTraitChanges([UITraitPreferredContentSizeCategory.self]) { (view: TerminalView, _) in
                view.contentSizeDidChange()
            }
            let center = NotificationCenter.default
            center.addObserver(
                self, selector: #selector(keyboardWillShow), name: UIResponder.keyboardWillShowNotification, object: nil)
            center.addObserver(
                self, selector: #selector(keyboardWillHide), name: UIResponder.keyboardWillHideNotification, object: nil)
            for name in [UIResponder.keyboardDidShowNotification, UIResponder.keyboardDidHideNotification] {
                center.addObserver(self, selector: #selector(keyboardDidMove), name: name, object: nil)
            }
        }

        @objc private func keyboardWillShow() {
            keyboardWillMove(.opening)
        }

        @objc private func keyboardWillHide() {
            keyboardWillMove(.closing)
        }

        /// 键盘要动了。自己开关键盘时在拿、交焦点之前就先调一次：SwiftUI 比这里先收到键盘的通知，先改了
        /// 视图的大小。
        private func keyboardWillMove(_ motion: KeyboardMotion) {
            keyboardMotion = motion
            keyboardSettle?.cancel()
            // 等不到键盘停稳的通知时（拿焦点没拿成之类）一秒后照常报。
            keyboardSettle = Task { @MainActor [weak self] in
                try? await Task.sleep(for: .seconds(1))
                if !Task.isCancelled { self?.keyboardDidMove() }
            }
        }

        /// 键盘停稳了：下一次布局按停稳后的大小报尺寸（收起键盘后底部的地方要等这时才全让出来）。
        @objc private func keyboardDidMove() {
            keyboardSettle?.cancel()
            keyboardMotion = nil
            setNeedsLayout()
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
            // 按住的键、组字中的文字是给原来那份 VT 的：视图换了会话或者重新连上时不接着连发、不上屏。
            stopRepeating()
            discardMarkedText()
            endSelection()
            self.terminal = terminal
            grid.imageSource = { [weak terminal] id, generation in terminal?.image(id: id, generation: generation) }
            aboveGrid.imageSource = grid.imageSource
            grid.settings = settings
            aboveGrid.settings = settings
            grid.screen = ScreenFrame()
            grid.pruneImages()
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

        /// VT 变了：合并一轮里的多块输出，下一轮主循环再从 render state 抄变了的行。刷屏时两次刷新至少
        /// 隔一帧，抄和画得慢时按它们耗时的两倍拉开，让抄和画最多占一半主线程，空档有上限；离上次刷新
        /// 已经够久时（打字回显）马上刷。
        public func scheduleRefresh() {
            guard !refreshScheduled else { return }
            refreshScheduled = true
            Task { @MainActor [weak self] in
                guard let wait = self.map({ $0.nextRefresh - .now }) else { return }
                if wait > .zero { try? await Task.sleep(for: wait) }
                guard let self else { return }
                refreshScheduled = false
                let started = ContinuousClock.now
                refreshNow()
                let now = ContinuousClock.now
                let cost = now - started + grid.drawTime
                grid.drawTime = .zero
                nextRefresh = now + min(max(.milliseconds(16), cost * 2), .milliseconds(250))
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
            // 程序在同步输出、改尺寸后还在重画：留着上一帧，一会儿再看（见 `VTerminal.isRenderHeld`）。
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
            if change.imagesChanged {
                grid.pruneImages()
            }
            scheduleImageFrame(grid.screen.nextImageFrame)
            if change.full || change.imagesChanged {
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
            if change.full || change.aboveChanged || change.imagesChanged {
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
            // 选区在 VT 里没了（切到备用屏等）就退出选中；没在选中却还有选区就清掉。
            if selecting, !terminal.hasSelection {
                endSelection()
            } else if !selecting, terminal.hasSelection {
                terminal.clearSelection()
                scheduleRefresh()
            }
            layoutSelectionHandles()
        }

        /// 有在播的图片动画时到点再刷新一次；动画停了就不再刷。
        private func scheduleImageFrame(_ due: Duration?) {
            imageFrame?.cancel()
            imageFrame = due.map { due in
                Task { @MainActor [weak self] in
                    try? await Task.sleep(for: max(due, .milliseconds(8)))
                    guard !Task.isCancelled else { return }
                    self?.scheduleRefresh()
                }
            }
        }

        /// 有输出时视口跟着光标走（网格比屏幕宽、横着拖的时候尤其要）；用户正在拖、刚拖过，或者在看
        /// 回滚历史时不动。
        private func followCursor() {
            guard !lastScrolledBack, !scrollView.isDragging, !scrollView.isDecelerating, !scrollView.isZooming,
                selectionDrag == nil, ContinuousClock.now - lastUserScroll > .seconds(2)
            else { return }
            revealCursor()
        }

        // MARK: 布局、缩放

        public override func layoutSubviews() {
            super.layoutSubviews()
            let resized = scrollView.frame != bounds
            if resized {
                scrollView.frame = bounds
                applyDefaultZoom()
            }
            centerContent(stickToBottom: resized)
            reportFitSize()
            layoutSelectionHandles()
        }

        /// 网格的行列数变了：按新的大小排，没手动缩放过时重新按屏幕宽度缩放。
        private func layoutGrid() {
            let zoom = scrollView.zoomScale
            scrollView.zoomScale = 1
            gridContainer.frame = CGRect(origin: .zero, size: grid.gridSize)
            grid.frame = gridContainer.bounds
            applyScrollOffset()
            scrollView.contentSize = gridContainer.frame.size
            if userZoomed && !fitsPhone {
                scrollView.zoomScale = zoom
            } else {
                applyDefaultZoom()
            }
            updateRasterScale()
            centerContent()
        }

        /// 默认的缩放：适配手机时固定不缩放、也不能捏合（网格本来就按视图排）；跟随电脑时网格比屏幕宽就缩到正好一屏宽，
        /// 但字不小于可读字号，再宽的横着拖。捏合也不能缩到可读字号以下。
        private func applyDefaultZoom() {
            if fitsPhone {
                scrollView.minimumZoomScale = 1
                scrollView.maximumZoomScale = 1
                scrollView.zoomScale = 1
                updateRasterScale()
                return
            }
            guard grid.gridSize.width > 0, bounds.width > 0 else { return }
            let sizes = Self.fontSizes(for: traitCollection, override: fontSizeOverride)
            let readable = min(sizes.readable / grid.font.size, 1)
            scrollView.minimumZoomScale = readable
            scrollView.maximumZoomScale = Self.maximumZoom
            guard !userZoomed else { return }
            let fitWidth = bounds.width / grid.gridSize.width
            scrollView.zoomScale = max(min(fitWidth, 1), readable)
            updateRasterScale()
        }

        public func viewForZooming(in scrollView: UIScrollView) -> UIView? {
            gridContainer
        }

        public func scrollViewDidZoom(_ scrollView: UIScrollView) {
            centerContent()
            layoutSelectionHandles()
        }

        public func scrollViewDidScroll(_ scrollView: UIScrollView) {
            layoutSelectionHandles()
        }

        public func scrollViewWillBeginZooming(_ scrollView: UIScrollView, with view: UIView?) {
            userZoomed = true
            lastUserScroll = .now
        }

        public func scrollViewWillBeginDragging(_ scrollView: UIScrollView) {
            lastUserScroll = .now
            editMenu.dismissMenu()
        }

        public func scrollViewDidEndZooming(_ scrollView: UIScrollView, with view: UIView?, atScale scale: CGFloat) {
            updateRasterScale()
        }

        /// 按当前缩放比例重画，放大后字不发虚。位图的边长和总像素有上限，免得超大的网格吃光内存。
        private func updateRasterScale() {
            let screenScale = window?.screen.scale ?? traitCollection.displayScale
            // 平常不低于 1，缩小着看也不糊；网格大到连 1 都超过上限时照上限再往下压，宁可糊一些。
            var scale = max(max(scrollView.zoomScale, 0.1) * screenScale, 1)
            let size = grid.bounds.size
            if size.width > 0, size.height > 0 {
                scale = min(scale, 8192 / size.width, 8192 / size.height, sqrt(24_000_000 / (size.width * size.height)))
            }
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
            row.selected = grid.screen.above.isEmpty ? [] : [grid.screen.aboveSelected]
            row.background = grid.screen.background
            row.foreground = grid.screen.foreground
            row.cursorColor = grid.screen.cursorColor
            // 这一行在视口上面一行，图片的纵坐标跟着下移一行；只露出这一行的那部分由视图边界裁掉。
            row.images = grid.screen.above.isEmpty ? [] : grid.screen.images.map { image in
                var image = image
                image.y += 1
                return image
            }
            aboveGrid.screen = row
            aboveGrid.pruneImages()
            aboveGrid.setNeedsDisplay()
        }

        /// 平滑滚动的错开量：网格在容器里往下挪不足一行，视口上面那一行跟在它上面。只挪位置，不重画。
        private func applyScrollOffset() {
            let rowHeight = grid.font.cellHeight
            // 取整到屏幕像素：网格和上面那一行在半个像素上各自取整时，中间会漏出一条背景色的缝。
            let pixels = traitCollection.displayScale * scrollView.zoomScale
            let shift = (CGFloat(grid.screen.scrollOffset) * rowHeight * pixels).rounded() / pixels
            let width = gridContainer.bounds.width
            if grid.frame.minY != shift {
                grid.frame.origin.y = shift
            }
            aboveGrid.frame = CGRect(x: 0, y: shift - rowHeight, width: width, height: rowHeight)
            aboveGrid.isHidden = shift == 0
            // 不错开时不裁，组字的文字超出网格右边也看得见。
            gridContainer.clipsToBounds = shift != 0
        }

        /// 网格比屏幕窄时左右居中；比屏幕矮出整行时贴着底边，上面空着的地方露出终端的背景色，不足一行的
        /// 零头留在底下。适配手机时多出的整行只是在等宿主改尺寸：上面有回滚历史时照样贴底边；没有历史、
        /// 或键盘正在弹出（`keyboardMotion`）时贴着顶边，免得网格先往下跳又跳回来。
        private func centerContent(stickToBottom: Bool = false) {
            let horizontal = max(0, (scrollView.bounds.width - scrollView.contentSize.width) / 2)
            let rowHeight = grid.font.cellHeight * scrollView.zoomScale
            let waitsForRows = fitsPhone && (grid.screen.above.isEmpty || keyboardMotion == .opening)
            var slack = waitsForRows ? 0 : scrollView.bounds.height - scrollView.contentSize.height
            if rowHeight > 0 { slack = (slack / rowHeight).rounded(.down) * rowHeight }
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

        /// 适配手机时网格左右至少各空出的宽度（点）：字不贴着屏幕的圆角边，`centerContent` 把余下的宽度
        /// 平分到两边。
        static let fitMargin: CGFloat = 4

        /// 跟随电脑时捏合能放大到的倍数。
        static let maximumZoom: CGFloat = 4

        static func gridSize(fitting size: CGSize, scale: CGFloat, font: TerminalFont) -> GridSize {
            let columns = max(10, Int((size.width - 2 * fitMargin) / font.cellWidth))
            let rows = max(4, Int(size.height / font.cellHeight))
            return GridSize(
                cols: UInt16(min(columns, 500)), rows: UInt16(min(rows, 300)),
                cellWidthPx: UInt16((font.cellWidth * scale).rounded()),
                cellHeightPx: UInt16((font.cellHeight * scale).rounded()))
        }

        private func reportFitSize() {
            guard bounds.width > 0, bounds.height > 0, keyboardMotion == nil else { return }
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
            guard acceptsInput else { return }
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
            bar.palette = keyBarPalette
            bar.setControlLatched(controlLatched)
            bar.setDictating(dictating)
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
                editMenu.dismissMenu()
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
            // ProMotion 屏上跟系统的滚动视图一样按 120Hz 走（应用开了 CADisableMinimumFrameDurationOnPhone）。
            link.preferredFrameRateRange = CAFrameRateRange(minimum: 80, maximum: 120, preferred: 120)
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

        public override var canBecomeFirstResponder: Bool { acceptsInput }

        public override var inputAccessoryView: UIView? { accessoryBar }

        @discardableResult
        public override func becomeFirstResponder() -> Bool {
            if !isFirstResponder { keyboardWillMove(.opening) }
            let became = super.becomeFirstResponder()
            grid.hasKeyboardFocus = isFirstResponder
            delegate?.terminalView(self, keyboardVisible: isFirstResponder)
            return became
        }

        @discardableResult
        public override func resignFirstResponder() -> Bool {
            if isFirstResponder { keyboardWillMove(.closing) }
            // 松开键的事件可能送到别处去了：不再是输入焦点就不连发。
            stopRepeating()
            let resigned = super.resignFirstResponder()
            grid.hasKeyboardFocus = isFirstResponder
            delegate?.terminalView(self, keyboardVisible: isFirstResponder)
            return resigned
        }

        /// 轻点：程序开着鼠标上报时当成一次点击发给它（全屏 agent 界面里「跳到底部」这类按钮）。只有点在
        /// 光标那行的上一行及以下（输入框和它下面的状态栏）才弹键盘，点上面的对话、输出不弹，键盘开着时
        /// 收起；看不到光标时，程序管鼠标就只点击，不管就照旧弹键盘。选中时只管选区：点在选区或拖柄上
        /// 重新弹菜单，点在别处退出选中，都不点击、不开关键盘。
        @objc private func handleTap(_ tap: UITapGestureRecognizer) {
            handleTap(at: tap.location(in: grid))
        }

        /// 轻点网格上的 `point`（网格坐标）。
        func handleTap(at point: CGPoint) {
            if selecting {
                if selectionContains(point) || handle(at: grid.convert(point, to: self)) != nil {
                    showEditMenu(at: nil)
                } else {
                    endSelection()
                }
                return
            }
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

        // MARK: 选区

        /// 长按：取到词就进入选中，手指不抬接着拖等于拖终点柄，抬手弹菜单；取不到词只弹「粘贴」。程序开着
        /// 鼠标上报时也在本地选，不发给程序。
        @objc private func handleLongPress(_ press: UILongPressGestureRecognizer) {
            switch press.state {
            case .began:
                let point = press.location(in: grid)
                guard beginSelection(at: point) else {
                    showEditMenu(at: press.location(in: self))
                    return
                }
                selectFeedback.impactOccurred()
                longPressOrigin = fingerCell(point)
            case .changed:
                guard selecting, selectionDrag != nil else { return }
                let point = press.location(in: grid)
                // 手指离开按下的那格之前不拖，刚选中的词不被手指的抖动缩短。
                if let origin = longPressOrigin {
                    guard fingerCell(point) != origin else { return }
                    longPressOrigin = nil
                }
                dragSelection(to: point)
            case .ended, .cancelled, .failed:
                longPressOrigin = nil
                finishSelectionDrag()
            default:
                break
            }
        }

        /// `point`（网格坐标）落在的那一格，不按格子边取整。
        private func fingerCell(_ point: CGPoint) -> GridCell {
            let width = max(grid.font.cellWidth, 1)
            let height = max(grid.font.cellHeight, 1)
            return GridCell(column: Int((point.x / width).rounded(.down)), row: Int((point.y / height).rounded(.down)))
        }

        /// 选中网格上 `point`（网格坐标）所在的词，进入选中，接着拖的是终点柄。点在网格外、格子空着、取到
        /// 的只有空白时不进入，原来的选区也退出，返回假。
        @discardableResult
        func beginSelection(at point: CGPoint) -> Bool {
            guard let terminal, grid.bounds.contains(point), grid.font.cellWidth > 0, grid.font.cellHeight > 0 else {
                endSelection()
                return false
            }
            let column = Int(point.x / grid.font.cellWidth)
            let row = Int(point.y / grid.font.cellHeight)
            guard terminal.selectWord(column: column, row: row) else {
                endSelection()
                return false
            }
            guard let text = terminal.selectionText(), !text.allSatisfy(\.isWhitespace) else {
                terminal.clearSelection()
                endSelection()
                scheduleRefresh()
                return false
            }
            stopMomentum()
            editMenu.dismissMenu()
            selecting = true
            selectionDrag = .end
            lastDragTarget = nil
            selectionFinger = grid.convert(point, to: self)
            // 关了再开，取消这一按里已经在跟的滚动和轻点：之后手指的移动只拖选区，抬手也不算轻点。
            for recognizer in [scrollView.panGestureRecognizer, tap] + scrollbackPans {
                recognizer.isEnabled = false
                recognizer.isEnabled = true
            }
            scheduleRefresh()
            return true
        }

        /// 正在拖的那一端拖到 `point`（网格坐标）；越出可见区域时开始自动滚动、平移。
        func dragSelection(to point: CGPoint) {
            guard selecting, selectionDrag != nil else { return }
            editMenu.dismissMenu()
            selectionFinger = grid.convert(point, to: self)
            if extendSelectionToFinger() { scheduleRefresh() }
            updateSelectionScroll()
        }

        /// 拖完了：停下自动滚动，弹菜单。
        private func finishSelectionDrag() {
            stopSelectionScroll()
            guard selecting, selectionDrag != nil else { return }
            selectionDrag = nil
            lastDragTarget = nil
            refreshNow()
            showEditMenu(at: nil)
        }

        /// 退出选中：清掉 VT 里的选区，收起拖柄和菜单。
        func endSelection() {
            guard selecting else { return }
            selecting = false
            selectionDrag = nil
            lastDragTarget = nil
            longPressOrigin = nil
            stopSelectionScroll()
            editMenu.dismissMenu()
            terminal?.clearSelection()
            layoutSelectionHandles()
            scheduleRefresh()
        }

        private func dragHandle(_ handle: SelectionHandleView, _ pan: UIPanGestureRecognizer) {
            switch pan.state {
            case .began:
                let ends = selecting ? terminal?.selectionBounds : nil
                guard let cell = handle.end == .start ? ends?.start : ends?.end else { return }
                stopMomentum()
                editMenu.dismissMenu()
                selectionDrag = handle.end
                lastDragTarget = nil
                let anchor = handleAnchor(handle.end, at: cell)
                let finger = pan.location(in: grid)
                handleGrab = CGVector(dx: anchor.x - finger.x, dy: (anchor.top + anchor.bottom) / 2 - finger.y)
            case .changed:
                guard selectionDrag != nil else { return }
                let finger = pan.location(in: grid)
                dragSelection(to: CGPoint(x: finger.x + handleGrab.dx, y: finger.y + handleGrab.dy))
            case .ended, .cancelled, .failed:
                finishSelectionDrag()
            default:
                break
            }
        }

        /// 两个拖柄离得近、按住的范围叠在一起时，交给离手指近的那个。
        public override func hitTest(_ point: CGPoint, with event: UIEvent?) -> UIView? {
            let hit = super.hitTest(point, with: event)
            guard hit != nil, selecting, let end = handle(at: point) else { return hit }
            return end == .start ? startHandle : endHandle
        }

        /// 本视图坐标里的 `point` 按在哪个拖柄上。
        private func handle(at point: CGPoint) -> SelectionEnd? {
            SelectionGeometry.hitHandle(
                point, start: startHandle.isHidden ? nil : startHandle.center,
                end: endHandle.isHidden ? nil : endHandle.center, radius: SelectionHandleView.minimumTouchSize / 2)
        }

        /// 两个拖柄摆到选区两端；那一端不在视口里、或在滚动视图外面时不画。
        private func layoutSelectionHandles() {
            let ends = selecting ? terminal?.selectionBounds : nil
            for handle in [startHandle, endHandle] {
                guard let cell = handle.end == .start ? ends?.start : ends?.end else {
                    handle.isHidden = true
                    continue
                }
                let anchor = handleAnchor(handle.end, at: cell)
                let top = grid.convert(CGPoint(x: anchor.x, y: anchor.top), to: self)
                let bottom = grid.convert(CGPoint(x: anchor.x, y: anchor.bottom), to: self)
                handle.place(x: top.x, top: top.y, bottom: bottom.y)
                let middle = CGPoint(x: top.x, y: (top.y + bottom.y) / 2)
                handle.isHidden = !scrollView.frame.insetBy(dx: -1, dy: -1).contains(middle)
            }
        }

        private func handleCell(_ end: SelectionEnd, at point: CGPoint) -> GridCell {
            SelectionGeometry.handleCell(
                end, x: point.x, y: point.y, cellWidth: grid.font.cellWidth, cellHeight: grid.font.cellHeight,
                columns: grid.screen.columns, rows: grid.screen.rows)
        }

        private func handleAnchor(_ end: SelectionEnd, at cell: GridCell) -> SelectionGeometry.HandleAnchor {
            SelectionGeometry.handleAnchor(
                end, at: cell, in: grid.screen.cells, cellWidth: grid.font.cellWidth, cellHeight: grid.font.cellHeight)
        }

        /// 网格上的 `point`（网格坐标）落在选中的格子里。
        private func selectionContains(_ point: CGPoint) -> Bool {
            guard grid.font.cellWidth > 0, grid.font.cellHeight > 0, point.x >= 0, point.y >= 0 else { return false }
            let row = Int(point.y / grid.font.cellHeight)
            let column = Int(point.x / grid.font.cellWidth)
            guard grid.screen.selected.indices.contains(row), let range = grid.screen.selected[row] else { return false }
            return range.contains(column)
        }

        /// 网格露在滚动视图里的部分（网格坐标）。
        private var visibleGridRect: CGRect {
            grid.bounds.intersection(grid.convert(scrollView.bounds, from: scrollView))
        }

        /// 按手指最后的位置（夹进可见区域）挪正在拖的那一端，返回挪了没有。落到的格子和被拖的那一端都和
        /// 上次一样时不再挪；网格滚过、平移过之后同一格已经是别的内容，`force` 为真照样挪。
        @discardableResult
        private func extendSelectionToFinger(force: Bool = false) -> Bool {
            guard let terminal, let moving = selectionDrag else { return false }
            let visible = visibleGridRect
            guard !visible.isEmpty else { return false }
            var point = grid.convert(selectionFinger, from: self)
            point.x = min(max(point.x, visible.minX), visible.maxX)
            point.y = min(max(point.y, visible.minY), visible.maxY - 0.01)
            let cell = handleCell(moving, at: point)
            if !force, let last = lastDragTarget, last.cell == cell, last.end == moving { return false }
            let moved = terminal.extendSelection(moving: moving, column: cell.column, row: cell.row)
            selectionDrag = moved
            lastDragTarget = (cell, moved)
            return true
        }

        /// 手指越过它就开始自动滚动、平移的范围（网格坐标）：可见区域，但哪条边离滚动视图的边不到
        /// `SelectionGeometry.edgeBand` 时，那条边退到滚动视图的边往里 `edgeBand` 处。
        private func selectionScrollBounds(_ visible: CGRect) -> CGRect {
            let view = grid.convert(scrollView.bounds, from: scrollView)
            let band = SelectionGeometry.edgeBand / max(scrollView.zoomScale, 0.01)
            func span(_ low: CGFloat, _ high: CGFloat, _ viewLow: CGFloat, _ viewHigh: CGFloat) -> (CGFloat, CGFloat) {
                let start = max(low, viewLow + band)
                let end = min(high, viewHigh - band)
                return start <= end ? (start, end) : ((low + high) / 2, (low + high) / 2)
            }
            let (minX, maxX) = span(visible.minX, visible.maxX, view.minX, view.maxX)
            let (minY, maxY) = span(visible.minY, visible.maxY, view.minY, view.maxY)
            return CGRect(x: minX, y: minY, width: maxX - minX, height: maxY - minY)
        }

        /// 手指越出 `selectionScrollBounds` 时，网格在那个方向还有没露出来的部分就平移网格（`pan`，滚动视图
        /// 的点每秒）；纵向已经到了网格边上、手指又越出可见区域时滚回滚历史（`lines`，行每秒，为正看更早的内容）。
        private func selectionEdgeMotion() -> (pan: CGVector, lines: Double) {
            let visible = visibleGridRect
            guard let terminal, selectionDrag != nil, !visible.isEmpty else { return (.zero, 0) }
            let edges = selectionScrollBounds(visible)
            let point = grid.convert(selectionFinger, from: self)
            let zoom = scrollView.zoomScale
            var pan = CGVector.zero
            var lines = 0.0
            let columns = SelectionGeometry.edgeRate(
                point.x, low: edges.minX, high: edges.maxX, cell: grid.font.cellWidth)
            let moreSideways = columns < 0 ? visible.minX > grid.bounds.minX + 0.5 : visible.maxX < grid.bounds.maxX - 0.5
            if columns != 0, moreSideways {
                pan.dx = CGFloat(columns) * grid.font.cellWidth * zoom
            }
            let rows = SelectionGeometry.edgeRate(
                point.y, low: edges.minY, high: edges.maxY, cell: grid.font.cellHeight)
            let moreVertically = rows < 0 ? visible.minY > grid.bounds.minY + 0.5 : visible.maxY < grid.bounds.maxY - 0.5
            if rows != 0, moreVertically {
                pan.dy = CGFloat(rows) * grid.font.cellHeight * zoom
            } else if !terminal.programScrolls {
                // shortcut: 程序自己管滚动时不自动滚，选区只能在一屏之内；要跨屏选全屏程序的输出时改成按
                // 滚轮事件让程序滚
                // 滚回滚历史只看可见区域本身、不加触发带：首行、末行的行中线离视图边不到一条触发带宽。
                lines = SelectionGeometry.autoscrollRate(
                    y: point.y, top: visible.minY, bottom: visible.maxY, cellHeight: grid.font.cellHeight)
            }
            return (pan, lines)
        }

        private func updateSelectionScroll() {
            let motion = selectionEdgeMotion()
            guard motion.pan != .zero || motion.lines != 0 else {
                stopSelectionScroll()
                return
            }
            guard selectionScrollLink == nil else { return }
            let link = CADisplayLink(target: self, selector: #selector(stepSelectionScroll(_:)))
            link.preferredFrameRateRange = CAFrameRateRange(minimum: 80, maximum: 120, preferred: 120)
            link.add(to: .main, forMode: .common)
            selectionScrollLink = link
        }

        @objc private func stepSelectionScroll(_ link: CADisplayLink) {
            let motion = selectionEdgeMotion()
            guard let terminal, motion.pan != .zero || motion.lines != 0 else {
                stopSelectionScroll()
                return
            }
            let elapsed = min(link.targetTimestamp - link.timestamp, 0.05)
            var moved = false
            if motion.pan != .zero {
                moved = panGrid(by: CGVector(dx: motion.pan.dx * elapsed, dy: motion.pan.dy * elapsed))
            }
            if motion.lines != 0 {
                moved = terminal.scrollSmoothly(lines: motion.lines * elapsed) || moved
            }
            // 滚到了头、挪不动时不重读重画，等手指回来。
            guard moved else { return }
            extendSelectionToFinger(force: true)
            refreshNow()
        }

        private func stopSelectionScroll() {
            selectionScrollLink?.invalidate()
            selectionScrollLink = nil
        }

        /// 平移网格 `delta`（滚动视图的点），不越过能滚到的范围；返回挪了没有。
        @discardableResult
        private func panGrid(by delta: CGVector) -> Bool {
            let inset = scrollView.contentInset
            let content = scrollView.contentSize
            let size = scrollView.bounds.size
            var offset = scrollView.contentOffset
            offset.x = min(
                max(offset.x + delta.dx, -inset.left), max(-inset.left, content.width + inset.right - size.width))
            offset.y = min(
                max(offset.y + delta.dy, -inset.top), max(-inset.top, content.height + inset.bottom - size.height))
            guard offset != scrollView.contentOffset else { return false }
            lastUserScroll = .now
            scrollView.contentOffset = offset
            return true
        }

        /// 弹编辑菜单：选中时指着选区；没在选中时指着 `point`（本视图坐标），只有「粘贴」，不能粘贴就不弹。
        private func showEditMenu(at point: CGPoint?) {
            if !selecting {
                guard point != nil, canPerformAction(#selector(paste(_:)), withSender: nil) else { return }
            }
            menuPoint = point
            let target = menuTargetRect
            guard !target.isNull, !target.isEmpty || !selecting else { return }
            editMenu.presentEditMenu(
                with: UIEditMenuConfiguration(identifier: nil, sourcePoint: CGPoint(x: target.midX, y: target.minY)))
        }

        /// 菜单指着的矩形（本视图坐标）：选区露在滚动视图里的部分的外接矩形；没在选中时是 `menuPoint`。
        private var menuTargetRect: CGRect {
            guard selecting else {
                return menuPoint.map { CGRect(origin: $0, size: .zero) } ?? .null
            }
            let cellWidth = grid.font.cellWidth
            let cellHeight = grid.font.cellHeight
            var rect = CGRect.null
            for (row, range) in grid.screen.selected.enumerated() {
                guard let range else { continue }
                rect = rect.union(
                    CGRect(
                        x: CGFloat(range.lowerBound) * cellWidth, y: CGFloat(row) * cellHeight,
                        width: CGFloat(range.count) * cellWidth, height: cellHeight))
            }
            guard !rect.isNull else { return .null }
            return grid.convert(rect, to: self).intersection(scrollView.frame)
        }

        public func editMenuInteraction(
            _ interaction: UIEditMenuInteraction, menuFor configuration: UIEditMenuConfiguration,
            suggestedActions: [UIMenuElement]
        ) -> UIMenu? {
            let actions = editMenuActions()
            return actions.isEmpty ? nil : UIMenu(children: actions)
        }

        public func editMenuInteraction(
            _ interaction: UIEditMenuInteraction, targetRectFor configuration: UIEditMenuConfiguration
        ) -> CGRect {
            menuTargetRect
        }

        /// 菜单里的「拷贝」「粘贴」「全选」，按 `canPerformAction` 取能用的。
        private func editMenuActions() -> [UIAction] {
            let items: [(Selector, String, (TerminalView) -> Void)] = [
                (#selector(copy(_:)), String(localized: "拷贝"), { $0.copy(nil) }),
                (#selector(paste(_:)), String(localized: "粘贴"), { $0.paste(nil) }),
                (#selector(selectAll(_:)), String(localized: "全选"), { $0.selectAll(nil) }),
            ]
            return items.compactMap { action, title, perform in
                guard canPerformAction(action, withSender: nil) else { return nil }
                return UIAction(title: title) { [weak self] _ in
                    guard let self else { return }
                    perform(self)
                }
            }
        }

        // MARK: 输入

        /// 发一份输入给上层；打字时回到最底下、把光标露出来。
        func emit(_ input: TerminalInput) {
            endSelection()
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
            switch action {
            case #selector(paste(_:)): acceptsInput && (pasteboard.hasStrings || pasteboard.hasImages)
            case #selector(copy(_:)): terminal?.hasSelection == true
            case #selector(selectAll(_:)): selecting
            default: super.canPerformAction(action, withSender: sender)
            }
        }

        /// 剪贴板上有文字时粘贴文字；没有文字、有图片时把图片交给代理。
        public override func paste(_ sender: Any?) {
            if let text = pasteboard.string, !text.isEmpty {
                emit(.paste(text))
            } else if acceptsInput, let image = pastedImage() {
                endSelection()
                delegate?.terminalView(self, didPasteImage: image)
            }
        }

        /// 剪贴板上那张图片的文件内容：有 PNG 或 JPEG 的原始数据时用它，没有时把图片编成 PNG。
        private func pastedImage() -> Data? {
            guard pasteboard.hasImages else { return nil }
            for type in [UTType.png, UTType.jpeg] {
                if let data = pasteboard.data(forPasteboardType: type.identifier), !data.isEmpty { return data }
            }
            return pasteboard.image?.pngData()
        }

        /// 把一段文字当作用户粘贴的发出去。
        public func pasteText(_ text: String) {
            guard !text.isEmpty else { return }
            emit(.paste(text))
        }

        /// 选中的文字写进剪贴板，退出选中。
        public override func copy(_ sender: Any?) {
            guard let text = terminal?.selectionText(), !text.isEmpty else {
                endSelection()
                return
            }
            pasteboard.string = text
            endSelection()
        }

        /// 选中整屏连同回滚历史，菜单接着留着。
        public override func selectAll(_ sender: Any?) {
            guard selecting, let terminal, terminal.selectAll() else { return }
            refreshNow()
            showEditMenu(at: nil)
        }

        // MARK: 硬件键盘

        public override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
            trackOptions(presses, down: true)
            // 输入法正在组字时按键都归输入法。
            guard markedText.isEmpty else {
                super.pressesBegan(presses, with: event)
                return
            }
            var unhandled = Set<UIPress>()
            for press in presses {
                if let key = press.key, var input = Self.keyInput(for: key) {
                    // 辅助栏上粘住的 Ctrl 加在这个键上，连发的也是加了 Ctrl 的。
                    if controlLatched {
                        input.modifiers.insert(.control)
                        controlLatched = false
                    }
                    if input.modifiers.contains(.alt) {
                        input.rightAlt =
                            heldOptions.contains(.keyboardRightAlt) && !heldOptions.contains(.keyboardLeftAlt)
                    }
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
            trackOptions(presses, down: false)
            stopRepeating()
            super.pressesEnded(presses, with: event)
        }

        public override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
            trackOptions(presses, down: false)
            stopRepeating()
            super.pressesCancelled(presses, with: event)
        }

        /// 记下按下、松开的 Option 键。别的键按下时修饰键标志里没有 Option，说明松开时没收到事件（焦点被
        /// 拿走了），记着的都作废。
        private func trackOptions(_ presses: Set<UIPress>, down: Bool) {
            for press in presses {
                guard let key = press.key else { continue }
                switch key.keyCode {
                case .keyboardLeftAlt, .keyboardRightAlt:
                    if down { heldOptions.insert(key.keyCode) } else { heldOptions.remove(key.keyCode) }
                default:
                    if down, !key.modifierFlags.contains(.alternate) { heldOptions.removeAll() }
                }
            }
        }

        /// 按住不放时连发：0.4 秒后每 50 毫秒一次。
        private func startRepeating(_ input: KeyInput) {
            repeatTask?.cancel()
            repeatTask = Task { @MainActor [weak self] in
                try? await Task.sleep(for: .milliseconds(400))
                while !Task.isCancelled {
                    // 视图没了就停，不空转。
                    guard let self else { return }
                    self.emit(.key(input))
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
            // Shift+回车带着 Shift 按 VT 的模式编码，和桌面一样（程序据此分出换行和提交）；交给系统的话
            // 经 `insertText` 成了不带修饰键的回车。
            if key.keyCode == .keyboardReturnOrEnter, modifiers == .shift {
                return KeyInput(key: .enter, modifiers: modifiers)
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
