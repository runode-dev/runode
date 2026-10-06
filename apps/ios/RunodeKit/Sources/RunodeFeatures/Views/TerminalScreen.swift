#if os(iOS)
    import RunodeConnection
    import RunodeProtocol
    import RunodeTerminal
    import SwiftUI

    /// 终端页：整页铺终端的背景色，导航栏是和终端协调的半透明深色（浅色主题时是浅色），标题下面一行
    /// agent 和连接的状态。断线时终端上方叠一条带「重试」的横幅；agent 等回答时底部出现快速回复栏；
    /// 软键盘没弹出时底部常驻一条按键栏（Esc、Ctrl、方向键……最后一个键打开软键盘），弹出后由键盘上方的
    /// 辅助栏接手。
    struct TerminalScreen: View {
        @Bindable var model: TerminalModel
        /// 设置里的字号和响铃震动。
        let preferences: AppPreferences
        /// 打开这个会话所在仓库的 Git 页。
        var onOpenGit: () -> Void = {}
        /// 断线横幅占的高度，终端视图据此在顶上让出地方，横幅不挡内容。
        @State private var bannerHeight: CGFloat = 0
        /// 嵌着的终端视图，底部的按键栏按了发给它。
        @State private var terminalView: TerminalView?

        private var background: Color { Color(model.background) }
        private var scheme: ColorScheme { model.background.isDark ? .dark : .light }

        var body: some View {
            TerminalViewRepresentable(
                model: model, preferences: preferences, topObstruction: bannerMessage == nil ? 0 : bannerHeight,
                onMake: { view in terminalView = view })
                .ignoresSafeArea(.container, edges: .horizontal)
                .overlay(alignment: .top) { banner }
                .overlay(alignment: .bottomTrailing) {
                    if model.scrolledBack {
                        Button("回到最新", systemImage: "arrow.down.to.line") { model.scrollToBottom() }
                            .buttonStyle(.borderedProminent)
                            .controlSize(.large)
                            .padding()
                            .transition(.opacity)
                    }
                }
                .safeAreaInset(edge: .bottom, spacing: 0) { bottomBars }
                .background(background.ignoresSafeArea())
                .animation(.easeOut(duration: 0.2), value: model.isAwaitingAnswer)
                .animation(.easeOut(duration: 0.2), value: model.keyboardVisible)
                .navigationTitle(model.title)
                .navigationBarTitleDisplayMode(.inline)
                .toolbarBackground(background.opacity(0.92), for: .navigationBar)
                .toolbarBackground(.visible, for: .navigationBar)
                .toolbarColorScheme(scheme, for: .navigationBar)
                .toolbar {
                    ToolbarItem(placement: .principal) { titleView }
                    ToolbarItem(placement: .primaryAction) { sizeMenu }
                    ToolbarItem(placement: .primaryAction) { moreMenu }
                }
        }

        private var titleView: some View {
            TimelineView(.periodic(from: .now, by: 1)) { context in
                VStack(spacing: 0) {
                    Text(model.title)
                        .font(.headline)
                        .lineLimit(1)
                    Text(
                        Presentation.terminalSubtitle(
                            agent: model.agent, link: model.linkState, phase: model.phase, now: context.date)
                    )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                }
                .dynamicTypeSize(...DynamicTypeSize.xxLarge)
                .environment(\.colorScheme, scheme)
                .accessibilityElement(children: .combine)
                .accessibilityAddTraits(.isHeader)
            }
        }

        private var sizeMenu: some View {
            Menu {
                Picker(
                    "终端尺寸",
                    selection: Binding(get: { model.sizePreference }, set: { model.setSizePreference($0) })
                ) {
                    Label("自动", systemImage: "wand.and.stars").tag(SizePreference.automatic)
                    Label("适配手机", systemImage: "iphone").tag(SizePreference.fitPhone)
                    Label("跟随电脑", systemImage: "laptopcomputer").tag(SizePreference.followMachine)
                }
                .pickerStyle(.inline)
                Section {
                    Text(Presentation.sizeOwnership(model.sizeOwnership))
                    if let size = model.gridSize {
                        Text("网格 \(Presentation.gridSize(size))")
                    }
                }
            } label: {
                Image(systemName: model.fitsPhone ? "iphone" : "laptopcomputer")
            }
            .accessibilityLabel("终端尺寸：\(model.fitsPhone ? "适配手机" : "跟随电脑")")
            .accessibilityHint("在适配手机和跟随电脑之间切换")
        }

        private var moreMenu: some View {
            Menu {
                Button("键盘", systemImage: "keyboard") { model.showKeyboard() }
                Button("回到最新", systemImage: "arrow.down.to.line") { model.scrollToBottom() }
                Button("Git", systemImage: "arrow.triangle.branch") { onOpenGit() }
                Button("结束会话", systemImage: "xmark.circle", role: .destructive) {
                    model.isConfirmingKill = true
                }
            } label: {
                Image(systemName: "ellipsis")
            }
            .accessibilityLabel("终端选项")
            // 挂在菜单按钮上，确认框的气泡才指着它。
            .confirmationDialog("结束这个终端？", isPresented: $model.isConfirmingKill, titleVisibility: .visible) {
                Button("结束会话", role: .destructive) { model.kill() }
            } message: {
                Text("里面正在跑的程序会收到 SIGHUP 并退出。")
            }
        }

        @ViewBuilder
        private var banner: some View {
            if let message = bannerMessage {
                TimelineView(.periodic(from: .now, by: 1)) { context in
                    HStack(spacing: 10) {
                        Image(systemName: bannerSymbol)
                            .foregroundStyle(bannerTint)
                        Text(message(context.date))
                            .font(.subheadline)
                            .fixedSize(horizontal: false, vertical: true)
                        Spacer(minLength: 8)
                        if canRetry {
                            Button("重试") { model.reconnect() }
                                .buttonStyle(.bordered)
                                .frame(minHeight: 44)
                        }
                    }
                    .padding(.horizontal, 14)
                    .padding(.vertical, 6)
                    .frame(minHeight: 44)
                    .background(.regularMaterial, in: .card)
                    .environment(\.colorScheme, scheme)
                    .padding(.horizontal, 12)
                    .padding(.top, 8)
                    .accessibilityElement(children: .contain)
                }
                .onGeometryChange(for: CGFloat.self) { $0.size.height + 8 } action: { bannerHeight = $0 }
                .transition(.opacity)
            }
        }

        /// 横幅上的文字；不用横幅时为空。
        private var bannerMessage: ((Date) -> String)? {
            switch model.phase {
            case .disconnected:
                let state = model.linkState
                return { now in
                    if case .failed(let failure) = state { return failure.errorDescription ?? "连接失败" }
                    return Presentation.linkStatus(state, now: now)
                }
            case .exited(let status?):
                return { _ in "shell 已退出（退出码 \(status)）" }
            case .exited(nil):
                return { _ in "shell 已退出" }
            case .gone(let message):
                return { _ in "这个终端已经不在了：\(message)" }
            case .connecting:
                return { _ in "正在连接…" }
            default:
                return nil
            }
        }

        private var canRetry: Bool {
            if case .disconnected = model.phase { return true }
            return false
        }

        private var bannerSymbol: String {
            switch model.phase {
            case .disconnected: "wifi.exclamationmark"
            case .connecting: "arrow.triangle.2.circlepath"
            default: "info.circle"
            }
        }

        private var bannerTint: Color {
            if case .disconnected = model.phase { return .orange }
            return .secondary
        }

        private var bottomBars: some View {
            VStack(spacing: 0) {
                if model.isAwaitingAnswer {
                    QuickReplyBar(model: model.quickReply, prompt: awaitingPrompt)
                        .padding(.horizontal)
                        .padding(.vertical, 10)
                        .transition(.move(edge: .bottom).combined(with: .opacity))
                }
                // 软键盘弹出时键盘上方有一样的辅助栏，这条就收起来。尺寸跟随谁看导航栏右边的图标。
                if !model.keyboardVisible, let terminalView {
                    RestingKeyBar(terminalView: terminalView)
                        .frame(height: 44)
                        .transition(.opacity)
                }
            }
            .background(.bar)
            .environment(\.colorScheme, scheme)
        }

        private var awaitingPrompt: String {
            let name = model.agent?.kind.displayName ?? "Agent"
            return "\(name) 在等你回答"
        }
    }

    /// 软键盘收着时底部的按键栏（`TerminalView.makeRestingKeyBar`）。
    private struct RestingKeyBar: UIViewRepresentable {
        let terminalView: TerminalView

        func makeUIView(context: Context) -> UIView {
            terminalView.makeRestingKeyBar()
        }

        func updateUIView(_ view: UIView, context: Context) {}
    }

    /// 把 UIKit 的 `TerminalView` 嵌进 SwiftUI，用户的输入转给视图模型。
    struct TerminalViewRepresentable: UIViewRepresentable {
        let model: TerminalModel
        let preferences: AppPreferences
        /// 叠在终端顶上的横幅的高度。
        var topObstruction: CGFloat = 0
        /// 建好了终端视图，页面的按键栏要用它。
        var onMake: (TerminalView) -> Void = { _ in }

        func makeCoordinator() -> Coordinator {
            Coordinator(model: model)
        }

        func makeUIView(context: Context) -> TerminalView {
            let view = TerminalView(frame: .zero)
            view.delegate = context.coordinator
            model.attachDisplay(view)
            // 正在更新视图时不能改页面的状态，下一轮再交出去。
            Task { @MainActor in onMake(view) }
            return view
        }

        func updateUIView(_ view: TerminalView, context: Context) {
            view.topObstruction = topObstruction
            view.fontSizeOverride = preferences.fontSize.map { CGFloat($0) }
            view.bellHaptics = preferences.bellHaptics
        }

        @MainActor
        final class Coordinator: TerminalViewDelegate {
            let model: TerminalModel

            init(model: TerminalModel) {
                self.model = model
            }

            func terminalView(_ view: TerminalView, didInput input: TerminalInput) {
                model.send(input)
            }

            func terminalView(_ view: TerminalView, fitSizeDidChange size: GridSize) {
                model.updateFitSize(size)
            }

            func terminalView(_ view: TerminalView, didScrollBack scrolledBack: Bool) {
                model.setScrolledBack(scrolledBack)
            }

            func terminalView(_ view: TerminalView, keyboardVisible visible: Bool) {
                model.setKeyboardVisible(visible)
            }
        }
    }
#endif
