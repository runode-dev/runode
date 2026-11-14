#if os(iOS)
    import RunodeConnection
    import RunodeProtocol
    import SwiftUI

    /// 配对页：取景卡里扫码；扫不了码（模拟器、没给权限）时卡里写原因，从底下拉出面板粘贴链接。从别处打开
    /// 的配对链接已经填在输入框里，一进来就拉出面板，用户看过要连的电脑再点「配对」。配好了换成
    /// `PairedPage`，由用户决定去看会话还是再配一台。
    struct PairingView: View {
        @Bindable var model: PairingModel
        let settings: SettingsModel
        /// 弹出的面板不跟着页面的深浅模式，要自己再套一次。
        let theme: AppTheme
        /// 关掉配对页；从配好了的那页去看会话时带着那台电脑。
        let onFinish: (MachineRecord?) -> Void
        @State private var showingPaste: Bool
        @Environment(\.themeColors) private var colors
        @ScaledMetric(relativeTo: .subheadline) private var pasteIconSize: CGFloat = 17

        init(
            model: PairingModel, settings: SettingsModel, theme: AppTheme,
            onFinish: @escaping (MachineRecord?) -> Void
        ) {
            self.model = model
            self.settings = settings
            self.theme = theme
            self.onFinish = onFinish
            _showingPaste = State(initialValue: !model.linkText.isEmpty)
        }

        var body: some View {
            Group {
                if case .paired(let machine) = model.phase {
                    PairedPage(
                        machine: machine, settings: settings, onViewSessions: { onFinish(machine) },
                        onPairAnother: { model.reset() })
                } else {
                    scanPage
                }
            }
            .background(colors.page)
            .animation(.default, value: model.phase)
            .sensoryFeedback(.success, trigger: model.phase) { _, phase in
                if case .paired = phase { true } else { false }
            }
            .sheet(isPresented: $showingPaste) {
                PasteLinkSheet(model: model)
                    .appTheme(theme)
            }
        }

        private var scanPage: some View {
            FillingScrollView {
                VStack(spacing: 14) {
                    header
                    Viewfinder(
                        unavailable: model.scannerUnavailable,
                        onCode: { code in
                            // 粘贴面板开着时用户在看面板里那台电脑，后面的相机扫到别的码不能越过确认去配。
                            guard !showingPaste else { return }
                            Task { await model.scanned(code) }
                        },
                        onUnavailable: { message in model.scannerUnavailable = message })
                    status
                        .transition(.opacity)
                    Spacer(minLength: 0)
                    Button {
                        showingPaste = true
                    } label: {
                        HStack(spacing: 8) {
                            Image(systemName: "keyboard")
                                .font(.system(size: pasteIconSize))
                                .accessibilityHidden(true)
                            Text("粘贴配对链接")
                        }
                        .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(SurfaceButtonStyle(.secondary, height: 50, cornerRadius: CornerRadius.action))
                    .font(.subheadline)
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 8)
            }
        }

        /// 左边关掉配对页的圆键，标题居中；右边留出和圆键一样宽的空，标题才在正中。
        private var header: some View {
            HStack(spacing: 10) {
                Button {
                    onFinish(nil)
                } label: {
                    Image(systemName: "chevron.left")
                        .font(.body.weight(.semibold))
                        .frame(width: 44, height: 44)
                        .background(colors.card, in: .circle)
                        .overlay(Circle().strokeBorder(colors.hairline, lineWidth: 1).allowsHitTesting(false))
                        .contentShape(.circle)
                }
                .buttonStyle(.plain)
                .foregroundStyle(.primary)
                .accessibilityLabel("关闭")
                Text("配对电脑")
                    .font(.headline)
                    .lineLimit(1)
                    .frame(maxWidth: .infinity)
                    .accessibilityAddTraits(.isHeader)
                Color.clear
                    .frame(width: 44, height: 44)
                    .accessibilityHidden(true)
            }
        }

        @ViewBuilder
        private var status: some View {
            switch model.phase {
            case .idle, .paired:
                EmptyView()
            case .pairing(let hostName):
                HStack(spacing: 10) {
                    ProgressView()
                    Text("正在和 \(hostName) 配对…")
                        .font(.subheadline)
                }
                .cardBackground(padding: 14)
                .accessibilityElement(children: .combine)
            case .failed(let message):
                VStack(alignment: .leading, spacing: 12) {
                    HStack(alignment: .firstTextBaseline, spacing: 10) {
                        Image(systemName: "xmark")
                            .font(.subheadline.weight(.bold))
                            .foregroundStyle(colors.failure)
                            .accessibilityHidden(true)
                        Text(message)
                            .font(.subheadline)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    Button {
                        model.reset()
                    } label: {
                        Text("重新扫码")
                            .padding(.horizontal, 16)
                    }
                    .buttonStyle(SurfaceButtonStyle(.raised))
                    .font(.subheadline)
                }
                .cardBackground(padding: 14)
            }
        }
    }

    /// 取景卡：相机画面裁进圆角卡片，四个角标出取景的范围，正中写怎么扫。扫不了码（`unavailable` 是原因）
    /// 时不放相机，正中改写原因。相机开着时说明叠在画面上，按深色画、底下压暗，画面再亮也看得清。
    private struct Viewfinder: View {
        let unavailable: String?
        let onCode: @MainActor (String) -> Void
        let onUnavailable: @MainActor (String) -> Void
        @Environment(\.themeColors) private var colors
        @Environment(\.colorScheme) private var colorScheme
        @ScaledMetric(relativeTo: .subheadline) private var iconSize: CGFloat = 30

        private static let height: CGFloat = 252
        private static let radius: CGFloat = 28
        /// 说明文字离卡边的空，竖着要让开四个角标。
        private static let textInsets = EdgeInsets(top: 56, leading: 24, bottom: 56, trailing: 24)
        private static let scrimOpacity: Double = 0.5

        private var live: Bool { unavailable == nil }

        var body: some View {
            let shape = RoundedRectangle(cornerRadius: Self.radius, style: .continuous)
            content
                .padding(Self.textInsets)
                .frame(maxWidth: .infinity, minHeight: Self.height)
                .background {
                    ZStack {
                        colors.well
                        if live {
                            QRScannerView(onCode: onCode, onUnavailable: onUnavailable)
                                .accessibilityHidden(true)
                            Color.black.opacity(Self.scrimOpacity)
                        }
                    }
                }
                .overlay {
                    ReticleCorners()
                        .stroke(.primary, style: StrokeStyle(lineWidth: 3, lineCap: .round, lineJoin: .round))
                        .accessibilityHidden(true)
                }
                .environment(\.colorScheme, live ? .dark : colorScheme)
                .clipShape(shape)
                .overlay(shape.strokeBorder(colors.hairline, lineWidth: 1).allowsHitTesting(false))
                .accessibilityElement(children: .combine)
        }

        private var content: some View {
            VStack(spacing: 10) {
                Image(systemName: live ? "qrcode" : "camera.badge.ellipsis")
                    .font(.system(size: iconSize))
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
                if let unavailable {
                    Text(unavailable)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                } else {
                    Text("把电脑上的二维码放进框里")
                        .font(.subheadline.weight(.semibold))
                    Text("在电脑的 Runode 里点侧栏的「手机」就能看到")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            }
            .multilineTextAlignment(.center)
            .fixedSize(horizontal: false, vertical: true)
        }
    }

    /// 取景卡四个角上的 L 形角标，拐角是圆的。
    private struct ReticleCorners: Shape {
        /// 角标离卡边的距离。
        var inset: CGFloat = 22
        /// 每条边的长度。
        var length: CGFloat = 34
        var radius: CGFloat = 12

        func path(in rect: CGRect) -> Path {
            let frame = rect.insetBy(dx: inset, dy: inset)
            var path = Path()
            for (corner, dx, dy) in [
                (CGPoint(x: frame.minX, y: frame.minY), 1.0, 1.0), (CGPoint(x: frame.maxX, y: frame.minY), -1.0, 1.0),
                (CGPoint(x: frame.minX, y: frame.maxY), 1.0, -1.0), (CGPoint(x: frame.maxX, y: frame.maxY), -1.0, -1.0),
            ] {
                let end = CGPoint(x: corner.x + dx * length, y: corner.y)
                path.move(to: CGPoint(x: corner.x, y: corner.y + dy * length))
                path.addArc(tangent1End: corner, tangent2End: end, radius: radius)
                path.addLine(to: end)
            }
            return path
        }
    }

    /// 配好了的那页：打勾，问要不要在 agent 等回答时收通知，再去看会话或者再配一台。电脑上的会话这时还没
    /// 读进来，不写有几个。
    private struct PairedPage: View {
        let machine: MachineRecord
        @Bindable var settings: SettingsModel
        let onViewSessions: () -> Void
        let onPairAnother: () -> Void
        @Environment(\.themeColors) private var colors
        @ScaledMetric(relativeTo: .title) private var badgeSize: CGFloat = 72
        @ScaledMetric(relativeTo: .title) private var glyphSize: CGFloat = 34
        @ScaledMetric(relativeTo: .body) private var bellSize: CGFloat = 20

        private static let badgeOpacity: Double = 0.14

        var body: some View {
            FillingScrollView {
                VStack(spacing: 14) {
                    Spacer(minLength: 0)
                    VStack(spacing: 14) {
                        StatusGlyph(.done, size: glyphSize)
                            .frame(width: badgeSize, height: badgeSize)
                            .background(colors.success.opacity(Self.badgeOpacity), in: .circle)
                        Text("配好了")
                            .font(.title.weight(.semibold))
                            .accessibilityAddTraits(.isHeader)
                        Text("已和 \(machine.name) 配对")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                    }
                    .multilineTextAlignment(.center)
                    .padding(.vertical, 24)
                    Spacer(minLength: 0)
                    HStack(spacing: 12) {
                        Image(systemName: "bell")
                            .font(.system(size: bellSize))
                            .foregroundStyle(.secondary)
                            .accessibilityHidden(true)
                        Toggle(isOn: $settings.preferences.alertsBlockedAgents) {
                            VStack(alignment: .leading, spacing: 1) {
                                Text("agent 问你问题时通知我")
                                    .font(.subheadline.weight(.semibold))
                                Text("不开 Runode 也能收到")
                                    .font(.footnote)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        // 主题的强调色是前景色，开关打开时底和圆钮一样白，看不出开着。
                        .tint(.green)
                    }
                    .cardBackground(padding: 14)
                    VStack(spacing: 10) {
                        Button(action: onViewSessions) {
                            Text("去看会话")
                                .frame(maxWidth: .infinity)
                        }
                        .buttonStyle(SurfaceButtonStyle(.primary, height: 50, cornerRadius: CornerRadius.action))
                        Button(action: onPairAnother) {
                            Text("再配一台")
                                .frame(maxWidth: .infinity, minHeight: 44)
                                .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .foregroundStyle(.secondary)
                    }
                    .font(.subheadline)
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 8)
            }
        }
    }

    /// 内容不满一屏时撑满一屏，里面的 `Spacer` 才把底下的按钮推到底；字号调大放不下时能滚。
    private struct FillingScrollView<Content: View>: View {
        @ViewBuilder let content: Content
        @State private var height: CGFloat = 0

        var body: some View {
            ScrollView {
                content
                    .frame(maxWidth: .infinity, minHeight: height, alignment: .top)
            }
            .scrollBounceBehavior(.basedOnSize)
            .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { height = $0 }
        }
    }

    /// 从底下拉出的面板：粘贴电脑上 Runode 的「手机」页里二维码下面的配对链接。链接认得出时列出要连的电脑名和地址，用户确认
    /// 是自己的电脑后点「配对」；粘贴进来也不直接配。
    private struct PasteLinkSheet: View {
        @Bindable var model: PairingModel
        @Environment(\.dismiss) private var dismiss
        @Environment(\.themeColors) private var colors
        @FocusState private var focused: Bool
        /// 面板按内容的高度停，字号调大了也不裁掉按钮。
        @State private var height: CGFloat = 200
        /// 链接是从别处打开时带进来的：面板只是让用户确认，不用再粘贴，也不弹键盘。
        @State private var opened: Bool

        init(model: PairingModel) {
            self.model = model
            _opened = State(initialValue: !model.linkText.isEmpty)
        }

        /// 带进来的链接认得出时不再显示输入框；认不出时照常显示，用户看得到它错在哪、能改。
        private var confirmsOnly: Bool { opened && model.invitation != nil }

        var body: some View {
            VStack(alignment: .leading, spacing: 12) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(confirmsOnly ? "和这台电脑配对？" : "粘贴配对链接").font(.headline)
                    Text(confirmsOnly ? "打开的配对链接要连这台电脑。" : "在电脑的 Runode 里点侧栏的「手机」，复制二维码下面的配对链接。")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
                if !confirmsOnly {
                    TextField("runode://pair?…", text: $model.linkText, axis: .vertical)
                        .font(.footnote.monospaced())
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .lineLimit(1...3)
                        .focused($focused)
                        .padding(12)
                        .background(colors.fill, in: .inner)
                }
                if let invitation = model.invitation {
                    VStack(alignment: .leading, spacing: 4) {
                        Label(invitation.hostName, systemImage: "desktopcomputer")
                            .font(.subheadline.weight(.semibold))
                        Text(verbatim: Presentation.pairingAddresses(invitation.addresses, port: invitation.port))
                            .font(.footnote.monospaced())
                            .foregroundStyle(.secondary)
                        Text("确认这是你自己的电脑再配对。")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                    .accessibilityElement(children: .combine)
                }
                HStack(spacing: 12) {
                    if !confirmsOnly {
                        PasteButton(payloadType: String.self) { strings in
                            guard let link = strings.first else { return }
                            Task { @MainActor in model.linkText = link }
                        }
                        .labelStyle(.iconOnly)
                        .buttonBorderShape(.capsule)
                    }
                    Spacer()
                    Button("取消") { dismiss() }
                    Button("配对") { submit() }
                        .prominentButtonStyle()
                        .disabled(model.linkText.isEmpty || model.isBusy)
                }
                .frame(minHeight: 44)
            }
            .padding(16)
            .fixedSize(horizontal: false, vertical: true)
            .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { height = $0 }
            .presentationDetents([.height(height)])
            .presentationDragIndicator(.visible)
            .onAppear { focused = !opened }
        }

        private func submit() {
            dismiss()
            Task { await model.submitLink() }
        }
    }

    #Preview {
        PairedPage(
            machine: MachineRecord(
                name: "MacBook Pro", hostName: "MacBook Pro",
                fingerprint: CertificateFingerprint(bytes: Data(repeating: 7, count: 32))!, port: 7866,
                addresses: ["127.0.0.1"], lastAddress: nil, deviceId: "00000000000000000000000000000000"),
            settings: SettingsModel(
                store: DefaultsStore("preferences", defaults: .temporary()), systemDeviceName: "iPhone"),
            onViewSessions: {}, onPairAnother: {})
    }
#endif
