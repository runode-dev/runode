#if os(iOS)
    import AVFoundation
    import RunodeProtocol
    import SwiftUI
    import UIKit

    /// 远程桌面页：电脑的整块屏幕，点按是左键，长按是右键，单指拖是按着左键拖，双指滑是滚动，捏合放大，
    /// 放大后双指拖着看别处。整页全屏，不要导航栏和状态栏，返回按钮浮在左上角，电脑有几块屏幕时右上角能切换。底部一条快捷键（软键盘没有的
    /// 键，右端开关软键盘），软键盘弹出时这条跟着键盘走（`ScreenKeyBar`）。
    struct RemoteDesktopScreen: View {
        @Bindable var model: ScreenModel
        @State private var keyboardVisible = false
        @Environment(\.dismiss) private var dismiss

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
                        ContentUnavailableView {
                            Label("远程桌面断开了", systemImage: "display.trianglebadge.exclamationmark")
                        } description: {
                            Text(reason)
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
                if !model.missingPermissions.isEmpty {
                    Text("电脑上的 Runode 还没有「\(model.missingPermissions.joined(separator: "、"))」的权限，到系统设置的隐私与安全性里打开")
                        .font(.footnote)
                        .padding(8)
                        .frame(maxWidth: .infinity)
                        .background(.yellow.opacity(0.9))
                        .foregroundStyle(.black)
                }
            }
            .overlay(alignment: .topLeading) {
                floatingButton("返回", systemImage: "chevron.left") { dismiss() }
                    .padding(8)
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
                .padding(8)
            }
            .sheet(isPresented: $model.askingPassword) {
                UnlockSheet(model: model)
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

        private func floatingButton(
            _ title: LocalizedStringKey, systemImage: String, action: @escaping () -> Void
        ) -> some View {
            Button(action: action) {
                Label(title, systemImage: systemImage).labelStyle(.iconOnly).floatingStyle()
            }
        }
    }

    extension View {
        /// 浮在画面上的圆形小按钮的样子。
        fileprivate func floatingStyle() -> some View {
            font(.body.weight(.semibold))
                .foregroundStyle(.white)
                .frame(width: 44, height: 44)
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
        let showKeyboard: () -> Void

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
            view.keys.onDismiss = { keyboardVisible = false }
            return view
        }

        func updateUIView(_ view: ScreenSurfaceView, context: Context) {
            view.videoSize = model.videoSize
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
        private var lastScroll = CGPoint.zero
        var videoSize: CGSize? {
            didSet { if videoSize != oldValue { setNeedsLayout() } }
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
            let press = UILongPressGestureRecognizer(target: self, action: #selector(pressed))
            let drag = UIPanGestureRecognizer(target: self, action: #selector(dragged))
            drag.maximumNumberOfTouches = 1
            scrollRecognizer.minimumNumberOfTouches = 2
            scrollRecognizer.addTarget(self, action: #selector(scrolled))
            for recognizer in [tap, press, drag, scrollRecognizer] {
                video.addGestureRecognizer(recognizer)
            }
            video.isUserInteractionEnabled = true
        }

        required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

        override func layoutSubviews() {
            super.layoutSubviews()
            guard zoomScale == 1 else { return }
            contentSize = bounds.size
            guard let videoSize, videoSize.width > 0, videoSize.height > 0 else {
                video.frame = bounds
                return
            }
            video.frame = AVMakeRect(aspectRatio: videoSize, insideRect: bounds)
        }

        func viewForZooming(in scrollView: UIScrollView) -> UIView? { video }

        func scrollViewDidZoom(_ scrollView: UIScrollView) {
            let zoomed = zoomScale > 1.01
            panGestureRecognizer.isEnabled = zoomed
            scrollRecognizer.isEnabled = !zoomed
        }

        /// 手势在画面上的位置换成比例。
        private func fraction(_ recognizer: UIGestureRecognizer) -> (Double, Double) {
            let point = recognizer.location(in: video)
            let size = video.bounds.size
            guard size.width > 0, size.height > 0 else { return (0, 0) }
            return (Double(point.x / size.width), Double(point.y / size.height))
        }

        @objc private func tapped(_ recognizer: UITapGestureRecognizer) {
            let (x, y) = fraction(recognizer)
            model.tap(x: x, y: y, view: recognizer.location(in: self))
        }

        @objc private func pressed(_ recognizer: UILongPressGestureRecognizer) {
            guard recognizer.state == .began else { return }
            let (x, y) = fraction(recognizer)
            model.rightClick(x: x, y: y)
        }

        @objc private func dragged(_ recognizer: UIPanGestureRecognizer) {
            let (x, y) = fraction(recognizer)
            switch recognizer.state {
            case .began: model.drag(.began, x: x, y: y)
            case .changed: model.drag(.moved, x: x, y: y)
            case .ended, .cancelled, .failed: model.drag(.ended, x: x, y: y)
            default: break
            }
        }

        @objc private func scrolled(_ recognizer: UIPanGestureRecognizer) {
            let translation = recognizer.translation(in: video)
            if recognizer.state == .began { lastScroll = .zero }
            model.scroll(
                dx: translation.x - lastScroll.x, dy: translation.y - lastScroll.y, viewWidth: video.bounds.width)
            lastScroll = translation
        }
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
    final class ScreenKeyBar: UIInputView {
        private let model: ScreenModel
        private var modButtons: [(UIButton, WritableKeyPath<ScreenMods, Bool>)] = []

        init(model: ScreenModel, resting: Bool, onKeyboard: @escaping () -> Void) {
            self.model = model
            super.init(
                frame: CGRect(x: 0, y: 0, width: 320, height: 44), inputViewStyle: resting ? .default : .keyboard)
            allowsSelfSizing = true
            if resting { backgroundColor = .clear }
            let scroll = UIScrollView()
            scroll.showsHorizontalScrollIndicator = false
            scroll.translatesAutoresizingMaskIntoConstraints = false
            addSubview(scroll)
            let stack = UIStackView()
            stack.axis = .horizontal
            stack.spacing = 6
            stack.translatesAutoresizingMaskIntoConstraints = false
            scroll.addSubview(stack)
            let keyboard = button(
                resting ? "keyboard" : "keyboard.chevron.compact.down", symbol: true,
                label: resting ? String(localized: "打开键盘") : String(localized: "收起键盘")
            ) { _ in onKeyboard() }
            keyboard.translatesAutoresizingMaskIntoConstraints = false
            addSubview(keyboard)
            NSLayoutConstraint.activate([
                scroll.leadingAnchor.constraint(equalTo: safeAreaLayoutGuide.leadingAnchor),
                scroll.trailingAnchor.constraint(equalTo: keyboard.leadingAnchor, constant: -4),
                scroll.topAnchor.constraint(equalTo: topAnchor),
                scroll.bottomAnchor.constraint(equalTo: bottomAnchor),
                keyboard.trailingAnchor.constraint(equalTo: safeAreaLayoutGuide.trailingAnchor, constant: -8),
                keyboard.centerYAnchor.constraint(equalTo: centerYAnchor),
                keyboard.heightAnchor.constraint(equalToConstant: 34),
                stack.leadingAnchor.constraint(equalTo: scroll.contentLayoutGuide.leadingAnchor, constant: 8),
                stack.trailingAnchor.constraint(equalTo: scroll.contentLayoutGuide.trailingAnchor, constant: -8),
                stack.centerYAnchor.constraint(equalTo: scroll.centerYAnchor),
                stack.heightAnchor.constraint(equalToConstant: 34),
            ])
            for (title, key, label) in [
                ("esc", "Escape", String(localized: "Esc 键")), ("tab", "Tab", String(localized: "Tab 键")),
            ] {
                stack.addArrangedSubview(button(title, label: label) { $0.model.press(key) })
            }
            for (title, mod, label) in [
                ("⌘", \ScreenMods.cmd, String(localized: "按住 Command")),
                ("⌃", \ScreenMods.ctrl, String(localized: "按住 Control")),
                ("⌥", \ScreenMods.alt, String(localized: "按住 Option")),
                ("⇧", \ScreenMods.shift, String(localized: "按住 Shift")),
            ] {
                let modButton = button(title, label: label) { bar in
                    bar.model.stickyMods[keyPath: mod].toggle()
                    bar.refreshMods()
                }
                modButtons.append((modButton, mod))
                stack.addArrangedSubview(modButton)
            }
            for (symbol, key, label) in [
                ("arrow.left", "ArrowLeft", String(localized: "左方向键")),
                ("arrow.up", "ArrowUp", String(localized: "上方向键")),
                ("arrow.down", "ArrowDown", String(localized: "下方向键")),
                ("arrow.right", "ArrowRight", String(localized: "右方向键")),
            ] {
                stack.addArrangedSubview(button(symbol, symbol: true, label: label) { $0.model.press(key) })
            }
        }

        required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

        private func button(
            _ title: String, symbol: Bool = false, label: String, action: @escaping (ScreenKeyBar) -> Void
        ) -> UIButton {
            var config = UIButton.Configuration.gray()
            if symbol { config.image = UIImage(systemName: title) } else { config.title = title }
            let button = UIButton(
                configuration: config,
                primaryAction: UIAction { [weak self] _ in
                    guard let self else { return }
                    action(self)
                    self.refreshMods()
                })
            button.accessibilityLabel = label
            button.widthAnchor.constraint(greaterThanOrEqualToConstant: 44).isActive = true
            return button
        }

        /// 按住的修饰键按下去的样子跟着 `stickyMods` 走：用掉以后自己松开。
        func refreshMods() {
            for (button, mod) in modButtons {
                let on = model.stickyMods[keyPath: mod]
                var config = button.configuration
                config?.baseBackgroundColor = on ? .tintColor : nil
                config?.baseForegroundColor = on ? .white : nil
                button.configuration = config
                button.accessibilityTraits = on ? [.button, .selected] : .button
            }
        }
    }
#endif
