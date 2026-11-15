#if os(iOS)
    import AVFoundation
    import RunodeProtocol
    import RunodeTerminal
    import SwiftUI
    import UIKit
    import UIKit.UIGestureRecognizerSubclass

    /// 远程桌面页：电脑的整块屏幕。触屏模式点按是左键，长按后原地松手是右键、接着拖是按着左键拖，没放大时单指
    /// 拖也是按着左键拖，放大后单指拖是挪画面；触控板模式单指挪光标，点按是左键，点一下再按住拖是按着左键拖。
    /// 两种模式下双指点按、一指按住另一指点都是右键。双指滑是滚动，捏合放大，放大后双指拖着看别处，按着左键
    /// 拖到或光标挪到页面边上时画面跟着往那边挪。
    /// 整页全屏，不要导航栏和状态栏，返回按钮浮在左上角，电脑有几块屏幕时右上角能切换。底部一条快捷键
    /// （软键盘没有的键，右端开关软键盘），软键盘弹出时这条跟着键盘走（`ScreenKeyBar`）。
    struct RemoteDesktopScreen: View {
        @Bindable var model: ScreenModel
        @State private var keyboardVisible = false
        @Environment(\.dismiss) private var dismiss
        @Environment(\.appTheme) private var theme

        var body: some View {
            VStack(spacing: 0) {
                ZStack {
                    Color.black
                    ScreenSurface(model: model, keyboardVisible: $keyboardVisible)
                        .opacity(model.phase == .live ? 1 : 0)
                    switch model.phase {
                    case .connecting:
                        ProgressView("正在连接远程桌面…").tint(.white).foregroundStyle(.white)
                    case .failed(let reason):
                        // 缺屏幕录制时连不上是意料中的，说清缺什么、去哪里开，不摆电脑那边的原话；只缺辅助功能时
                        // 画面照样看得了，断开另有原因，照说原因。
                        ContentUnavailableView {
                            if failedForPermission {
                                Label("电脑上的 Runode 缺少权限", systemImage: "lock.display")
                            } else {
                                Label("远程桌面断开了", systemImage: "display.trianglebadge.exclamationmark")
                            }
                        } description: {
                            Text(failedForPermission ? permissionMessage ?? reason : reason)
                        } actions: {
                            Button("重新连接") { model.retry() }
                        }
                        .foregroundStyle(.white)
                    case .live:
                        EmptyView()
                    }
                }
                // 画面铺到屏幕的上边和两边；底边留给快捷键栏，软键盘弹出时留给键盘，画面在剩下的地方缩放。
                .ignoresSafeArea(.container, edges: [.top, .horizontal])
                if !keyboardVisible, model.phase == .live {
                    RestingKeyBar(model: model, mods: model.stickyMods) { keyboardVisible = true }
                        .frame(height: 44)
                        .background(Color.black.ignoresSafeArea())
                }
            }
            .background(Color.black.ignoresSafeArea())
            .overlay(alignment: .top) {
                // 中间的失败页说着缺的权限时不再重复；排在浮动按钮下面，不被它们盖住。
                if let permissionMessage, !failedForPermission {
                    Text(permissionMessage)
                        .font(.footnote)
                        .padding(.horizontal, 12)
                        .padding(.vertical, 8)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(.yellow.opacity(0.9), in: .rect(cornerRadius: 12))
                        .foregroundStyle(.black)
                        .padding(.horizontal, FloatingButton.margin)
                        .padding(.top, FloatingButton.size + 2 * FloatingButton.margin)
                }
            }
            .overlay(alignment: .topLeading) {
                floatingButton("返回", systemImage: "chevron.left") { dismiss() }
                    .padding(FloatingButton.margin)
            }
            .overlay(alignment: .topTrailing) {
                HStack(spacing: 12) {
                    if model.locked {
                        Button {
                            model.unlock()
                        } label: {
                            Group {
                                if model.unlocking {
                                    ProgressView().tint(.white)
                                } else {
                                    Label("解锁电脑", systemImage: "lock.open").labelStyle(.iconOnly)
                                }
                            }
                            .floatingStyle()
                        }
                        .disabled(model.unlocking)
                    }
                    Button {
                        model.pointerMode = model.pointerMode == .touch ? .trackpad : .touch
                    } label: {
                        Label(
                            model.pointerMode == .touch ? "切到触控板模式" : "切到触屏模式",
                            systemImage: model.pointerMode == .touch ? "hand.tap" : "cursorarrow.motionlines"
                        )
                        .labelStyle(.iconOnly).floatingStyle()
                    }
                    if model.displays.count > 1 {
                        Menu {
                            ForEach(model.displays) { display in
                                Toggle(
                                    model.name(of: display),
                                    isOn: Binding(
                                        get: { model.display?.id == display.id },
                                        set: { if $0 { model.switchDisplay(to: display) } }))
                            }
                        } label: {
                            Label("切换屏幕", systemImage: "display.2").labelStyle(.iconOnly).floatingStyle()
                        }
                    }
                }
                .padding(FloatingButton.margin)
            }
            .sheet(isPresented: $model.askingPassword) {
                UnlockSheet(model: model)
                    .appTheme(theme)
            }
            .navigationTitle(model.machine.name)
            .toolbar(.hidden, for: .navigationBar)
            .toolbar(.hidden, for: .tabBar)
            .statusBarHidden()
            .persistentSystemOverlays(.hidden)
            // 离开页面时由 `AppModel` 断开；不在 onDisappear 里断，页面一时消失又出现（弹出解锁框这类）时会
            // 反复断开重连。
            .onAppear { model.start() }
        }

        private var failed: Bool {
            if case .failed = model.phase { true } else { false }
        }

        /// 连不上是因为电脑缺屏幕录制的权限，中间的失败页说的就是缺的权限。
        private var failedForPermission: Bool {
            failed && model.lacksScreenCapture
        }

        /// 电脑上缺的权限和去哪里开；不缺时为空。
        private var permissionMessage: String? {
            guard !model.missingPermissions.isEmpty else { return nil }
            return String(
                localized: "电脑上的 Runode 还没有「\(model.missingPermissions.joined(separator: "、"))」的权限，到系统设置的隐私与安全性里打开")
        }

        private func floatingButton(
            _ title: LocalizedStringKey, systemImage: String, action: @escaping () -> Void
        ) -> some View {
            Button(action: action) {
                Label(title, systemImage: systemImage).labelStyle(.iconOnly).floatingStyle()
            }
        }
    }

    /// 浮在画面左上、右上角的圆形按钮：边长和离安全区边缘的距离。
    private enum FloatingButton {
        static let size: CGFloat = 44
        static let margin: CGFloat = 8
    }

    extension View {
        /// 浮在画面上的圆形小按钮的样子。
        fileprivate func floatingStyle() -> some View {
            font(.body.weight(.semibold))
                .foregroundStyle(.white)
                .frame(width: FloatingButton.size, height: FloatingButton.size)
                .background(.ultraThinMaterial, in: Circle())
                .environment(\.colorScheme, .dark)
        }
    }

    /// 电脑锁着屏时输登录密码：发给电脑敲进锁屏的密码框，勾了记住时解开以后存在这台手机上。
    private struct UnlockSheet: View {
        let model: ScreenModel
        @State private var password = ""
        @State private var remember = true
        @FocusState private var focused: Bool

        var body: some View {
            NavigationStack {
                Form {
                    Section {
                        SecureField("登录密码", text: $password)
                            .focused($focused)
                            .textContentType(.password)
                            .submitLabel(.go)
                            .onSubmit(send)
                        Toggle("用 Face ID 记住", isOn: $remember)
                    } footer: {
                        if let error = model.unlockError {
                            Text(error).foregroundStyle(.red)
                        } else {
                            Text("密码发给电脑、敲进锁屏的密码框；记住时只存在这台手机上，每次要过 Face ID 才取得出。")
                        }
                    }
                }
                .navigationTitle("解锁电脑")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("取消") { model.askingPassword = false }
                    }
                    ToolbarItem(placement: .confirmationAction) {
                        Button("解锁", action: send).disabled(password.isEmpty)
                    }
                }
            }
            .presentationDetents([.medium])
            .onAppear { focused = true }
        }

        private func send() {
            guard !password.isEmpty else { return }
            model.unlock(with: password, remember: remember)
        }
    }

    /// 软键盘收着时页面底部的快捷键栏。`mods` 只为让按住的修饰键变了时（比如点按时用掉了）跟着刷新。
    private struct RestingKeyBar: UIViewRepresentable {
        let model: ScreenModel
        let mods: ScreenMods
        let showKeyboard: @MainActor () -> Void

        func makeUIView(context: Context) -> ScreenKeyBar {
            ScreenKeyBar(model: model, resting: true, onKeyboard: showKeyboard)
        }

        func updateUIView(_ bar: ScreenKeyBar, context: Context) {
            bar.refreshMods()
        }
    }

    /// 放画面的滚动视图：捏合放大，放大后双指拖着看别处；没放大时双指滑是电脑上的滚动。
    private struct ScreenSurface: UIViewRepresentable {
        let model: ScreenModel
        @Binding var keyboardVisible: Bool

        func makeUIView(context: Context) -> ScreenSurfaceView {
            let view = ScreenSurfaceView(model: model)
            model.onVideo = { [weak view] frame in view?.video.show(frame) }
            model.onVideoReset = { [weak view] in view?.video.reset() }
            model.onCursor = { [weak view] x, y in view?.placeCursor(x: x, y: y) }
            view.keys.onDismiss = { keyboardVisible = false }
            return view
        }

        func updateUIView(_ view: ScreenSurfaceView, context: Context) {
            view.videoSize = model.videoSize
            view.trackpad = model.pointerMode == .trackpad
            if keyboardVisible, !view.keys.isFirstResponder {
                _ = view.keys.becomeFirstResponder()
            } else if !keyboardVisible, view.keys.isFirstResponder {
                _ = view.keys.resignFirstResponder()
            }
        }
    }

    final class ScreenSurfaceView: UIScrollView, UIScrollViewDelegate {
        let video = ScreenVideoView()
        let keys: ScreenKeyInput
        private let model: ScreenModel
        private let scrollRecognizer = UIPanGestureRecognizer()
        private let press = UILongPressGestureRecognizer()
        /// 单指拖；触屏模式放大着时关掉，单指拖交给滚动视图挪画面。
        private let drag = UIPanGestureRecognizer()
        private let rightTap = SecondFingerTap()
        private let touches = TouchSequence()
        private var lastScroll = CGPoint.zero
        private var lastPoint = CGPoint.zero
        /// 触屏模式这次长按走到哪一步。
        private var pressState = PressState.idle
        /// 长按开始时手指在窗口上的位置，量挪开了多远。
        private var pressStart = CGPoint.zero
        /// 这次长按、单指拖开始时是不是触控板模式：中途切了模式，收尾照开始时的走，左键才不会一直按着。
        private var pressTrackpad = false
        private var dragTrackpad = false
        /// 触控板模式点一下的左键压到双击的时限过了才发：时限里又按下时，这一下是双击或点按拖移的开头。
        private var pendingClick: Task<Void, Never>?
        /// 这次按下时上一下点按的左键还压着没发。
        private var armed = false
        /// 触控板模式点一下再按住拖着，左键按着。
        private var tapDragging = false
        /// 这一轮触摸在触屏模式下已经右键过。
        private var rightClicked = false
        /// 这一轮触摸按下时画面还在惯性滑动：这一下是让画面停下，不是点电脑。
        private var stoppedScroll = false
        /// 触屏模式按着左键拖时手指停在页面边上，每一帧挪画面。
        private var edgeLink: CADisplayLink?
        private weak var edgeRecognizer: UIGestureRecognizer?

        private enum PressState {
            case idle
            /// 按住了还没挪开，带着按下时在画面上的位置。
            case holding(CGPoint)
            /// 原地松手，同一轮事件处理完、另一根手指也没点过时在这里右键。
            case released(CGPoint)
            /// 挪开了，按着左键拖。
            case dragging
        }
        /// 触控板模式的光标。
        let cursor = UIImageView(image: UIImage(systemName: "cursorarrow"))
        /// 换了尺寸时退回不放大：放大着时 `layoutSubviews` 不重排，画面会留在旧比例的框里，点的位置就对不上。
        var videoSize: CGSize? {
            didSet {
                guard videoSize != oldValue else { return }
                setZoomScale(1, animated: false)
                setNeedsLayout()
            }
        }
        /// 在触控板模式：显示光标。切进来时电脑上的光标挪到显示的光标那里，点下去的就是看到的位置。
        var trackpad = false {
            didSet {
                cursor.isHidden = !trackpad
                rightTap.requiresStillHeld = !trackpad
                updatePanning()
                if trackpad, !oldValue { model.moveCursor(dx: 0, dy: 0) }
            }
        }

        init(model: ScreenModel) {
            self.model = model
            keys = ScreenKeyInput(model: model)
            super.init(frame: .zero)
            delegate = self
            minimumZoomScale = 1
            maximumZoomScale = 5
            showsHorizontalScrollIndicator = false
            showsVerticalScrollIndicator = false
            contentInsetAdjustmentBehavior = .never
            panGestureRecognizer.minimumNumberOfTouches = 2
            panGestureRecognizer.isEnabled = false
            addSubview(video)
            addSubview(keys)
            video.onNeedKeyframe = { [weak model] in model?.requestKeyframe() }
            video.isAccessibilityElement = true
            video.accessibilityLabel = String(localized: "电脑屏幕")
            video.accessibilityTraits = .image

            let tap = UITapGestureRecognizer(target: self, action: #selector(tapped))
            press.addTarget(self, action: #selector(pressed))
            press.minimumPressDuration = Self.pressDuration
            rightTap.addTarget(self, action: #selector(rightTapped))
            touches.onBegan = { [weak self] in self?.touchesBegan() }
            touches.onEnded = { [weak self] in
                // 点按、拖动的收尾在同一轮触摸事件里处理完以后再看压着的左键用掉没有。
                Task { @MainActor in self?.flushArmedClick() }
            }
            cursor.tintColor = .white
            cursor.isHidden = true
            cursor.layer.shadowOpacity = 1
            cursor.layer.shadowRadius = 1
            cursor.layer.shadowOffset = .zero
            cursor.isUserInteractionEnabled = false
            video.addSubview(cursor)
            placeCursor(x: model.cursor.x, y: model.cursor.y)
            drag.addTarget(self, action: #selector(dragged))
            drag.maximumNumberOfTouches = 1
            scrollRecognizer.minimumNumberOfTouches = 2
            scrollRecognizer.addTarget(self, action: #selector(scrolled))
            for recognizer in [tap, rightTap, touches, press, drag, scrollRecognizer] {
                video.addGestureRecognizer(recognizer)
            }
            video.isUserInteractionEnabled = true
        }

        required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

        /// UIScrollView 缩放时把 `contentSize` 定为画面缩放后的大小，所以画面的原点须留在内容的 (0, 0)，
        /// 摆在哪由 `placeContent` 用 `contentInset` 定。
        override func layoutSubviews() {
            super.layoutSubviews()
            if zoomScale == 1 {
                let size = restingArea.size
                let fitted =
                    if let videoSize, videoSize.width > 0, videoSize.height > 0 {
                        AVMakeRect(aspectRatio: videoSize, insideRect: CGRect(origin: .zero, size: size)).size
                    } else {
                        size
                    }
                video.frame = CGRect(origin: .zero, size: fitted)
                contentSize = fitted
                placeCursor(x: model.cursor.x, y: model.cursor.y)
            }
            placeContent()
        }

        /// 不放大时画面在其中按比例放到最大、居中的区域。两边的黑边放不下浮动按钮时让到按钮那一条下面，
        /// 电脑屏幕的左上角、右上角才点得到。
        private var restingArea: CGRect {
            let full = CGRect(origin: .zero, size: bounds.size)
            guard let videoSize, videoSize.width > 0, videoSize.height > 0 else { return full }
            let fitted = AVMakeRect(aspectRatio: videoSize, insideRect: full)
            let reach = FloatingButton.margin + FloatingButton.size
            let top = safeAreaInsets.top + reach
            guard fitted.minX < safeAreaInsets.left + reach, fitted.minY < top else { return full }
            return full.inset(by: UIEdgeInsets(top: top, left: 0, bottom: 0, right: 0))
        }

        /// 画面放得下的方向上居中在 `restingArea` 里、拖不动。放大以后画面的四边最远拖到安全区边上，
        /// 上边最远拖到浮动按钮那一条下面，电脑屏幕的边角都露得出来、点得到。
        private func placeContent() {
            let area = restingArea
            let size = contentSize
            let zoomed = zoomScale > 1
            let reach = FloatingButton.margin + FloatingButton.size
            let floor =
                zoomed
                ? UIEdgeInsets(
                    top: safeAreaInsets.top + reach, left: safeAreaInsets.left,
                    bottom: safeAreaInsets.bottom, right: safeAreaInsets.right)
                : .zero
            let left = max(floor.left, area.minX + (area.width - size.width) / 2)
            let top = max(floor.top, area.minY + (area.height - size.height) / 2)
            let inset = UIEdgeInsets(
                top: top, left: left,
                bottom: max(floor.bottom, bounds.height - top - size.height),
                right: max(floor.right, bounds.width - left - size.width))
            if contentInset != inset { contentInset = inset }
            if !zoomed {
                let rest = CGPoint(x: -inset.left, y: -inset.top)
                if contentOffset != rest { contentOffset = rest }
                return
            }
            // 放大着时滚动视图变了尺寸（软键盘弹出收起、转屏），偏移收回能滚到的范围里。
            guard !isTracking, !isDecelerating, !isZooming, !isZoomBouncing else { return }
            let clamped = clampedOffset(contentOffset)
            if contentOffset != clamped { contentOffset = clamped }
        }

        /// `offset` 收进能滚到的范围里。
        private func clampedOffset(_ offset: CGPoint) -> CGPoint {
            let inset = contentInset
            return CGPoint(
                x: min(max(offset.x, -inset.left), contentSize.width + inset.right - bounds.width),
                y: min(max(offset.y, -inset.top), contentSize.height + inset.bottom - bounds.height))
        }

        /// 放大着时把画面挪 `delta`（到能滚到的边为止）；挪动了返回 true。
        @discardableResult
        private func panZoomed(by delta: CGPoint) -> Bool {
            guard zoomScale > 1.01, delta != .zero else { return false }
            let next = clampedOffset(CGPoint(x: contentOffset.x + delta.x, y: contentOffset.y + delta.y))
            guard next != contentOffset else { return false }
            contentOffset = next
            return true
        }

        /// 光标随放大反向缩小、看着大小不变，箭头尖钉在电脑光标的位置上。
        func placeCursor(x: Double, y: Double) {
            cursor.transform = .identity
            cursor.sizeToFit()
            let scale = 1 / zoomScale
            cursor.transform = CGAffineTransform(scaleX: scale, y: scale)
            let size = cursor.bounds.size
            cursor.center = CGPoint(
                x: x * video.bounds.width + size.width * scale / 2,
                y: y * video.bounds.height + size.height * scale / 2)
        }

        func viewForZooming(in scrollView: UIScrollView) -> UIView? { video }

        func scrollViewDidZoom(_ scrollView: UIScrollView) {
            placeCursor(x: model.cursor.x, y: model.cursor.y)
            placeContent()
            updatePanning()
        }

        func scrollViewWillBeginDragging(_ scrollView: UIScrollView) {
            flushArmedClick()
        }

        func scrollViewWillBeginZooming(_ scrollView: UIScrollView, with view: UIView?) {
            flushArmedClick()
        }

        /// 放大着时由滚动视图挪画面：触屏模式单指、双指都挪，触控板模式双指挪（单指要挪光标）。没放大时双指滑是
        /// 电脑上的滚动。
        private func updatePanning() {
            let zoomed = zoomScale > 1.01
            panGestureRecognizer.minimumNumberOfTouches = zoomed && !trackpad ? 1 : 2
            panGestureRecognizer.isEnabled = zoomed
            scrollRecognizer.isEnabled = !zoomed
            drag.isEnabled = trackpad || !zoomed
        }

        /// 手势在画面上的位置换成比例。
        private func fraction(_ recognizer: UIGestureRecognizer) -> (Double, Double) {
            fraction(of: recognizer.location(in: video))
        }

        /// 画面上的点换成比例。
        private func fraction(of point: CGPoint) -> (Double, Double) {
            let size = video.bounds.size
            guard size.width > 0, size.height > 0 else { return (0, 0) }
            return (Double(point.x / size.width), Double(point.y / size.height))
        }

        /// 一轮触摸的第一根手指按下：上一下点按的左键还压着时，触控板模式下这一下接着它，触屏模式下先把它发出去。
        private func touchesBegan() {
            rightClicked = false
            stoppedScroll = isDecelerating
            guard let pendingClick else { return }
            pendingClick.cancel()
            self.pendingClick = nil
            if trackpad { armed = true } else { model.clickAtCursor(.left) }
        }

        /// 压着的左键这一轮没连成双击或点按拖移，照常发出去。
        private func flushArmedClick() {
            guard armed else { return }
            armed = false
            model.clickAtCursor(.left)
        }

        @objc private func tapped(_ recognizer: UITapGestureRecognizer) {
            if stoppedScroll { return }
            guard model.pointerMode == .trackpad else {
                let (x, y) = fraction(recognizer)
                return model.tap(x: x, y: y, view: recognizer.location(in: self))
            }
            if armed {
                armed = false
                model.clickAtCursor(.left)
                model.clickAtCursor(.left)
                return
            }
            pendingClick = Task { @MainActor [weak self] in
                // 睡醒后到排上主线程之间也可能被取消。
                guard (try? await Task.sleep(for: Self.doubleTapWindow)) != nil, !Task.isCancelled, let self else {
                    return
                }
                self.pendingClick = nil
                self.model.clickAtCursor(.left)
            }
        }

        @objc private func rightTapped(_ recognizer: SecondFingerTap) {
            // 按着左键拖的时候不插一下右键。
            if tapDragging { return }
            if case .dragging = pressState { return }
            if model.pointerMode == .trackpad {
                flushArmedClick()
                return model.clickAtCursor(.right)
            }
            pressState = .idle
            rightClick(at: recognizer.heldLocation(in: video))
        }

        /// 触屏模式在画面上的 `point` 右键；长按松手和另一根手指点一下都会走到这里，一轮触摸只右键一次。
        private func rightClick(at point: CGPoint) {
            guard !rightClicked else { return }
            rightClicked = true
            let (x, y) = fraction(of: point)
            model.rightClick(x: x, y: y)
        }

        /// 触屏模式：长按认出来时震一下，原地松手是右键，挪开了是从按下的地方按着左键拖。触控板模式：长按挪的是
        /// 光标，上一下点按压着时是点按拖移。
        @objc private func pressed(_ recognizer: UILongPressGestureRecognizer) {
            if recognizer.state == .began { pressTrackpad = model.pointerMode == .trackpad }
            if pressTrackpad {
                trackpadMoved(recognizer)
                return
            }
            switch recognizer.state {
            case .began:
                flushArmedClick()
                pressState = .holding(recognizer.location(in: video))
                pressStart = recognizer.location(in: nil)
                // 放大着时滚动视图的单指平移也在等这根手指，取消掉，接下来挪的是长按拖动。
                panGestureRecognizer.isEnabled = false
                updatePanning()
                UIImpactFeedbackGenerator(style: .medium).impactOccurred()
            case .changed:
                if case .dragging = pressState {
                    let (x, y) = fraction(recognizer)
                    return model.drag(.moved, x: x, y: y)
                }
                guard case .holding(let start) = pressState else { return }
                let now = recognizer.location(in: nil)
                guard hypot(now.x - pressStart.x, now.y - pressStart.y) > Self.pressSlop else { return }
                pressState = .dragging
                let (x, y) = fraction(of: start)
                model.drag(.began, x: x, y: y)
                let (mx, my) = fraction(recognizer)
                model.drag(.moved, x: mx, y: my)
                startEdgePan(recognizer)
            case .ended:
                switch pressState {
                case .holding(let start):
                    // 同一轮事件里另一根手指的点按可能还没处理，处理完了再看还要不要右键。
                    pressState = .released(start)
                    Task { @MainActor [weak self] in
                        guard let self, case .released(let start) = self.pressState else { return }
                        self.pressState = .idle
                        self.rightClick(at: start)
                    }
                case .dragging: endPressDrag(recognizer)
                default: pressState = .idle
                }
            case .cancelled, .failed:
                if case .dragging = pressState { endPressDrag(recognizer) } else { pressState = .idle }
            default: break
            }
        }

        private func endPressDrag(_ recognizer: UIGestureRecognizer) {
            stopEdgePan()
            pressState = .idle
            let (x, y) = fraction(recognizer)
            model.drag(.ended, x: x, y: y)
        }

        /// 触控板模式单指挪光标；上一下点按的左键压着时，这一下按着左键拖。
        private func trackpadMoved(_ recognizer: UIGestureRecognizer) {
            let point = recognizer.location(in: video)
            switch recognizer.state {
            case .began:
                if armed {
                    armed = false
                    tapDragging = true
                    model.dragAtCursor(down: true)
                }
            case .changed where video.bounds.width > 0 && video.bounds.height > 0:
                model.moveCursor(
                    dx: Double((point.x - lastPoint.x) / video.bounds.width),
                    dy: Double((point.y - lastPoint.y) / video.bounds.height))
                revealCursor()
            case .ended, .cancelled, .failed:
                if tapDragging {
                    tapDragging = false
                    model.dragAtCursor(down: false)
                }
            default: break
            }
            // 画面挪过以后按挪过的位置算下一下的位移，光标才不跟着画面多走。
            lastPoint = recognizer.location(in: video)
        }

        @objc private func dragged(_ recognizer: UIPanGestureRecognizer) {
            if recognizer.state == .began { dragTrackpad = model.pointerMode == .trackpad }
            if dragTrackpad {
                trackpadMoved(recognizer)
                return
            }
            let (x, y) = fraction(recognizer)
            switch recognizer.state {
            case .began:
                flushArmedClick()
                model.drag(.began, x: x, y: y)
                startEdgePan(recognizer)
            case .changed: model.drag(.moved, x: x, y: y)
            case .ended, .cancelled, .failed:
                stopEdgePan()
                model.drag(.ended, x: x, y: y)
            default: break
            }
        }

        /// 光标出了页面边上那一圈时把放大着的画面挪到光标露出来。
        private func revealCursor() {
            let point = video.convert(
                CGPoint(x: model.cursor.x * video.bounds.width, y: model.cursor.y * video.bounds.height), to: self)
            let area = bounds.insetBy(dx: Self.edgeZone, dy: Self.edgeZone)
            let dx = point.x < area.minX ? point.x - area.minX : point.x > area.maxX ? point.x - area.maxX : 0
            let dy = point.y < area.minY ? point.y - area.minY : point.y > area.maxY ? point.y - area.maxY : 0
            panZoomed(by: CGPoint(x: dx, y: dy))
        }

        private func startEdgePan(_ recognizer: UIGestureRecognizer) {
            // CADisplayLink 强引用 self，换下的那个不失效就一直跑。
            stopEdgePan()
            edgeRecognizer = recognizer
            let link = CADisplayLink(target: self, selector: #selector(edgeTick))
            link.add(to: .main, forMode: .common)
            edgeLink = link
        }

        private func stopEdgePan() {
            edgeLink?.invalidate()
            edgeLink = nil
        }

        /// 触屏模式按着左键拖、手指停在页面边上那一圈时，放大着的画面往那边挪，越靠边越快，手指下的位置跟着变。
        @objc private func edgeTick() {
            guard let recognizer = edgeRecognizer, [.began, .changed].contains(recognizer.state) else {
                return stopEdgePan()
            }
            let point = recognizer.location(in: self)
            func axis(_ value: CGFloat, _ low: CGFloat, _ high: CGFloat) -> CGFloat {
                if value < low + Self.edgeZone { return -Self.edgeStep * min(1, (low + Self.edgeZone - value) / Self.edgeZone) }
                if value > high - Self.edgeZone { return Self.edgeStep * min(1, (value - high + Self.edgeZone) / Self.edgeZone) }
                return 0
            }
            let delta = CGPoint(x: axis(point.x, bounds.minX, bounds.maxX), y: axis(point.y, bounds.minY, bounds.maxY))
            guard panZoomed(by: delta) else { return }
            let (x, y) = fraction(recognizer)
            model.drag(.moved, x: x, y: y)
        }

        /// 双击的两下之间最长隔多久。
        private static let doubleTapWindow: Duration = .milliseconds(300)
        /// 按住多久算长按。
        private static let pressDuration: TimeInterval = 0.35
        /// 长按开始以后手指挪开多远就不再是右键、开始按着左键拖。
        private static let pressSlop: CGFloat = 4
        /// 手指或光标离页面边不到这么远时，放大着的画面往那边挪。
        private static let edgeZone: CGFloat = 40
        /// 手指停在页面最边上时画面每一帧挪多远。
        private static let edgeStep: CGFloat = 12

        @objc private func scrolled(_ recognizer: UIPanGestureRecognizer) {
            let translation = recognizer.translation(in: video)
            if recognizer.state == .began {
                lastScroll = .zero
                flushArmedClick()
            }
            model.scroll(
                dx: translation.x - lastScroll.x, dy: translation.y - lastScroll.y, viewWidth: video.bounds.width)
            lastScroll = translation
        }
    }

    /// 第二根手指点一下：两指一起点，或一指按住、另一指点。第二根落下以后两根手指哪根挪开了（那是捏合或滚动）、
    /// 第二根按得太久、落下第三根手指都不算；`requiresStillHeld` 时先按下的那根在第二根落下前挪开了也不算。
    /// 不拦别的手势，也不被别的手势拦。
    final class SecondFingerTap: UIGestureRecognizer {
        var requiresStillHeld = true
        private var held: UITouch?
        private var heldStart = CGPoint.zero
        private var heldLast = CGPoint.zero
        private var second: UITouch?
        private var secondStart = CGPoint.zero
        private var secondAt: TimeInterval = 0
        private static let slop: CGFloat = 10
        private static let maxDuration: TimeInterval = 0.5

        /// 先按下的那根手指（抬起了就是抬起时）在 `view` 上的位置。
        func heldLocation(in view: UIView) -> CGPoint {
            view.convert(heldLast, from: nil)
        }

        override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent) {
            for touch in touches {
                if held == nil {
                    held = touch
                    heldStart = touch.location(in: nil)
                    heldLast = heldStart
                } else if second == nil {
                    second = touch
                    secondStart = touch.location(in: nil)
                    secondAt = touch.timestamp
                    heldStart = heldLast
                } else {
                    state = .failed
                    return
                }
            }
        }

        override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent) {
            for touch in touches {
                let point = touch.location(in: nil)
                if touch === held {
                    heldLast = point
                    if requiresStillHeld || second != nil, Self.distance(point, heldStart) > Self.slop {
                        state = .failed
                    }
                } else if touch === second, Self.distance(point, secondStart) > Self.slop {
                    state = .failed
                }
            }
        }

        override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent) {
            if let held, touches.contains(held) {
                heldLast = held.location(in: nil)
            }
            if let second, touches.contains(second) {
                state = second.timestamp - secondAt <= Self.maxDuration ? .recognized : .failed
            } else if second == nil {
                state = .failed
            }
        }

        override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent) {
            state = .failed
        }

        override func reset() {
            held = nil
            second = nil
        }

        override func canPrevent(_ preventedGestureRecognizer: UIGestureRecognizer) -> Bool { false }
        override func canBePrevented(by preventingGestureRecognizer: UIGestureRecognizer) -> Bool { false }

        private static func distance(_ a: CGPoint, _ b: CGPoint) -> CGFloat {
            hypot(a.x - b.x, a.y - b.y)
        }
    }

    /// 只看一轮触摸的头尾：第一根手指按下时调 `onBegan`，手指都抬起时调 `onEnded`。自己从不认出手势，不拦别的。
    final class TouchSequence: UIGestureRecognizer {
        var onBegan: (() -> Void)?
        var onEnded: (() -> Void)?
        private var down = 0

        override init(target: Any?, action: Selector?) {
            super.init(target: target, action: action)
            cancelsTouchesInView = false
            delaysTouchesEnded = false
        }

        override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent) {
            if down == 0 { onBegan?() }
            down += touches.count
        }

        override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent) {
            lifted(touches.count)
        }

        override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent) {
            lifted(touches.count)
        }

        private func lifted(_ count: Int) {
            down -= count
            guard down <= 0 else { return }
            onEnded?()
            state = .failed
        }

        override func reset() {
            down = 0
        }

        override func canPrevent(_ preventedGestureRecognizer: UIGestureRecognizer) -> Bool { false }
        override func canBePrevented(by preventingGestureRecognizer: UIGestureRecognizer) -> Bool { false }
    }

    /// 显示画面：帧拼成样本交给 `AVSampleBufferDisplayLayer`，由它解码、马上显示。
    final class ScreenVideoView: UIView {
        override class var layerClass: AnyClass { AVSampleBufferDisplayLayer.self }
        private var displayLayer: AVSampleBufferDisplayLayer { layer as! AVSampleBufferDisplayLayer }
        private var builder = VideoSampleBuilder()
        /// 在等关键帧：这期间的别的帧解不出来，丢掉，也不重复要。
        private var awaitingKeyframe = false
        var onNeedKeyframe: (() -> Void)?

        override init(frame: CGRect) {
            super.init(frame: frame)
            displayLayer.videoGravity = .resizeAspect
            backgroundColor = .black
        }

        required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

        /// 采集重新开始了：清掉旧画面和旧格式。新的第一帧就是关键帧，不是的话照常要一个。
        func reset() {
            displayLayer.sampleBufferRenderer.flush(removingDisplayedImage: true, completionHandler: nil)
            builder = VideoSampleBuilder()
            awaitingKeyframe = false
        }

        func show(_ frame: VideoFrame) {
            let renderer = displayLayer.sampleBufferRenderer
            if renderer.status == .failed {
                renderer.flush()
                builder = VideoSampleBuilder()
                needKeyframe()
            }
            if awaitingKeyframe, !frame.keyframe { return }
            guard let sample = builder.sample(for: frame) else { return needKeyframe() }
            awaitingKeyframe = false
            renderer.enqueue(sample)
        }

        private func needKeyframe() {
            guard !awaitingKeyframe else { return }
            awaitingKeyframe = true
            onNeedKeyframe?()
        }
    }

    /// 接软键盘的隐藏输入框：上屏的字（输入法组完的）发给电脑，删除键在框里没字时发退格键，回车发回车。
    final class ScreenKeyInput: UITextField, UITextFieldDelegate {
        private let model: ScreenModel
        var onDismiss: (() -> Void)?

        init(model: ScreenModel) {
            self.model = model
            super.init(frame: CGRect(x: -100, y: -100, width: 1, height: 1))
            delegate = self
            autocorrectionType = .no
            autocapitalizationType = .none
            spellCheckingType = .no
            smartQuotesType = .no
            smartDashesType = .no
            // 画面四周常黑，键盘和跟在上面的快捷键栏不随 app 主题变浅。
            keyboardAppearance = .dark
            alpha = 0.01
            addTarget(self, action: #selector(changed), for: .editingChanged)
            inputAccessoryView = ScreenKeyBar(model: model, resting: false) { [weak self] in
                _ = self?.resignFirstResponder()
            }
        }

        override func becomeFirstResponder() -> Bool {
            // 底部那条上按住的修饰键带过来。
            modsUsed()
            return super.becomeFirstResponder()
        }

        required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

        @objc private func changed() {
            guard markedTextRange == nil, let text, !text.isEmpty else { return }
            model.type(text)
            self.text = ""
            modsUsed()
        }

        override func deleteBackward() {
            if text?.isEmpty ?? true {
                model.press("Backspace")
                modsUsed()
            }
            super.deleteBackward()
        }

        func textFieldShouldReturn(_ textField: UITextField) -> Bool {
            model.press("Enter")
            modsUsed()
            return false
        }

        /// 按住的修饰键随这次输入用掉了，键盘上面那条跟着松开。
        private func modsUsed() {
            (inputAccessoryView as? ScreenKeyBar)?.refreshMods()
        }

        override func resignFirstResponder() -> Bool {
            let resigned = super.resignFirstResponder()
            if resigned { onDismiss?() }
            return resigned
        }
    }

    /// 一条快捷键：Esc、Tab、按住的 ⌘ ⌃ ⌥ ⇧、方向键，右端开关软键盘。`resting` 时是软键盘收着时页面底部
    /// 那条，否则跟在软键盘上面。
    final class ScreenKeyBar: KeyBar {
        private let model: ScreenModel
        private var modButtons: [(UIButton, WritableKeyPath<ScreenMods, Bool>)] = []

        init(model: ScreenModel, resting: Bool, onKeyboard: @escaping @MainActor () -> Void) {
            self.model = model
            super.init(resting: resting)
            // 这一页的背景常黑，按钮的系统色要按深色取，浅色主题下才看得清。
            overrideUserInterfaceStyle = .dark
            let keyboard = key(
                resting ? "keyboard" : "keyboard.chevron.compact.down", symbol: true,
                label: resting ? String(localized: "打开键盘") : String(localized: "收起键盘"), action: onKeyboard)
            pinTrailing([keyboard])
            for (title, key, label) in [
                ("esc", "Escape", String(localized: "Esc 键")), ("tab", "Tab", String(localized: "Tab 键")),
            ] {
                keys.addArrangedSubview(button(title, label: label) { $0.model.press(key) })
            }
            for (title, mod, label) in [
                ("⌘", \ScreenMods.cmd, String(localized: "按住 Command")),
                ("⌃", \ScreenMods.ctrl, String(localized: "按住 Control")),
                ("⌥", \ScreenMods.alt, String(localized: "按住 Option")),
                ("⇧", \ScreenMods.shift, String(localized: "按住 Shift")),
            ] {
                let modButton = button(title, label: label) { bar in
                    bar.model.stickyMods[keyPath: mod].toggle()
                }
                modButtons.append((modButton, mod))
                keys.addArrangedSubview(modButton)
            }
            for (symbol, key, label) in [
                ("arrow.left", "ArrowLeft", String(localized: "左方向键")),
                ("arrow.up", "ArrowUp", String(localized: "上方向键")),
                ("arrow.down", "ArrowDown", String(localized: "下方向键")),
                ("arrow.right", "ArrowRight", String(localized: "右方向键")),
            ] {
                keys.addArrangedSubview(button(symbol, symbol: true, label: label) { $0.model.press(key) })
            }
        }

        /// 一个键，按下后修饰键按下去的样子跟着刷新：用掉了的自己松开。
        private func button(
            _ title: String, symbol: Bool = false, label: String, action: @escaping (ScreenKeyBar) -> Void
        ) -> UIButton {
            let button = key(title, symbol: symbol, label: label) { [weak self] in
                guard let self else { return }
                action(self)
                self.refreshMods()
            }
            button.widthAnchor.constraint(greaterThanOrEqualToConstant: 44).isActive = true
            return button
        }

        /// 按住的修饰键按下去的样子跟着 `stickyMods` 走：用掉以后自己松开。
        func refreshMods() {
            for (button, mod) in modButtons {
                highlight(button, model.stickyMods[keyPath: mod])
            }
        }
    }
#endif
