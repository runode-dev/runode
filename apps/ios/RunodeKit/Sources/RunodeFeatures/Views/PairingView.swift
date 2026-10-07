#if os(iOS)
    import RunodeConnection
    import SwiftUI
    import UIKit

    /// 配对页：相机铺满整屏，顶上写电脑上要做的几步，中间是取景框；扫不了码（模拟器、没给权限）时
    /// 从底下的按钮拉出面板粘贴链接。配好后底下显示结果，点「完成」进到这台电脑。
    struct PairingView: View {
        @Bindable var model: PairingModel
        /// 关掉配对页；配对成功时带着那台电脑。
        let onFinish: (MachineRecord?) -> Void
        @State private var showingPaste = false

        var body: some View {
            VStack(alignment: .leading, spacing: 0) {
                header
                Spacer()
                reticle
                    .frame(maxWidth: .infinity)
                Spacer()
                if model.phase != .idle {
                    status
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(16)
                        .background(.regularMaterial, in: .card)
                        .padding(.horizontal, 16)
                        .transition(.move(edge: .bottom).combined(with: .opacity))
                }
                Button {
                    showingPaste = true
                } label: {
                    Label("或者粘贴配对链接", systemImage: "clipboard")
                        .frame(maxWidth: .infinity, minHeight: 44)
                }
                .buttonStyle(.borderless)
                .foregroundStyle(.white)
                .padding(.vertical, 8)
            }
            .background {
                ZStack {
                    Color.black
                    if model.scannerUnavailable == nil {
                        QRScannerView(
                            onCode: { code in Task { await model.scanned(code) } },
                            onUnavailable: { message in model.scannerUnavailable = message }
                        )
                        .accessibilityLabel("二维码取景框")
                    }
                    // 压暗上下两头，相机画面再亮，说明文字和按钮也看得清。
                    LinearGradient(
                        stops: [
                            .init(color: .black.opacity(0.7), location: 0), .init(color: .clear, location: 0.3),
                            .init(color: .clear, location: 0.8), .init(color: .black.opacity(0.7), location: 1),
                        ], startPoint: .top, endPoint: .bottom)
                }
                .ignoresSafeArea()
            }
            // 叠在相机画面上，不管 app 是深是浅都按深色画；弹出的面板各自再套一次。
            .preferredColorScheme(.dark)
            .animation(.default, value: model.phase)
            .sheet(isPresented: $showingPaste) {
                PasteLinkSheet(model: model)
            }
        }

        private var header: some View {
            VStack(alignment: .leading, spacing: 14) {
                Button {
                    onFinish(nil)
                } label: {
                    Image(systemName: "chevron.left")
                        .font(.title3.weight(.medium))
                        .frame(width: 44, height: 44)
                }
                .foregroundStyle(.white)
                .accessibilityLabel("关闭")
                .padding(.leading, 4)
                VStack(alignment: .leading, spacing: 12) {
                    StepRow(step: 1, text: "在配置文件里加上 `remote-access = true`", command: "remote-access = true")
                    StepRow(step: 2, text: "在电脑的终端里运行 `runode remote pair`", command: "runode remote pair")
                    StepRow(step: 3, text: "扫终端里的二维码（5 分钟内有效）")
                }
                .padding(.horizontal, 16)
            }
        }

        /// 四个角标出的取景框；扫不了码时框里写原因。
        private var reticle: some View {
            ZStack {
                ReticleCorners()
                    .stroke(.white.opacity(0.85), style: StrokeStyle(lineWidth: 3, lineCap: .round))
                if let message = model.scannerUnavailable {
                    VStack(spacing: 10) {
                        Image(systemName: "camera.badge.ellipsis")
                            .font(.title)
                        Text(message)
                            .font(.footnote)
                            .multilineTextAlignment(.center)
                    }
                    .foregroundStyle(.secondary)
                    .padding(24)
                }
            }
            .frame(width: 260, height: 260)
        }

        @ViewBuilder
        private var status: some View {
            switch model.phase {
            case .idle:
                EmptyView()
            case .pairing(let hostName):
                HStack(spacing: 10) {
                    ProgressView()
                    Text("正在和 \(hostName) 配对…")
                }
            case .paired(let machine):
                VStack(alignment: .leading, spacing: 12) {
                    Label("已和 \(machine.name) 配对", systemImage: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                        .font(.headline)
                    Button {
                        onFinish(machine)
                    } label: {
                        Text("完成").frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.large)
                }
            case .failed(let message):
                VStack(alignment: .leading, spacing: 12) {
                    Label(message, systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.red)
                    Button("重新扫码") { model.reset() }
                        .buttonStyle(.bordered)
                }
            }
        }
    }

    /// 一步操作：圆底的序号加一句说明，说明里的命令按等宽显示；有命令的长按能拷贝，经通用剪贴板粘到电脑上。
    private struct StepRow: View {
        let step: Int
        let text: LocalizedStringKey
        var command: String?

        var body: some View {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Text("\(step)")
                    .font(.caption.weight(.semibold).monospacedDigit())
                    .frame(width: 22, height: 22)
                    .background(.white.opacity(0.15), in: .circle)
                    .alignmentGuide(.firstTextBaseline) { $0[.bottom] - 6 }
                Text(text)
                    .font(.subheadline)
                    .foregroundStyle(.white.opacity(0.9))
            }
            .contextMenu {
                if let command {
                    Button("拷贝 \(command)", systemImage: "doc.on.doc") {
                        UIPasteboard.general.string = command
                    }
                }
            }
        }
    }

    /// 取景框的四个角。
    private struct ReticleCorners: Shape {
        var length: CGFloat = 32

        func path(in rect: CGRect) -> Path {
            var path = Path()
            for (corner, dx, dy) in [
                (CGPoint(x: rect.minX, y: rect.minY), 1.0, 1.0), (CGPoint(x: rect.maxX, y: rect.minY), -1.0, 1.0),
                (CGPoint(x: rect.minX, y: rect.maxY), 1.0, -1.0), (CGPoint(x: rect.maxX, y: rect.maxY), -1.0, -1.0),
            ] {
                path.move(to: CGPoint(x: corner.x, y: corner.y + dy * length))
                path.addLine(to: corner)
                path.addLine(to: CGPoint(x: corner.x + dx * length, y: corner.y))
            }
            return path
        }
    }

    /// 从底下拉出的面板：粘贴电脑终端里二维码下面那行链接。
    private struct PasteLinkSheet: View {
        @Bindable var model: PairingModel
        @Environment(\.dismiss) private var dismiss
        @FocusState private var focused: Bool
        /// 面板按内容的高度停，字号调大了也不裁掉按钮。
        @State private var height: CGFloat = 200

        var body: some View {
            VStack(alignment: .leading, spacing: 12) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("粘贴配对链接").font(.headline)
                    Text("拷贝电脑终端里二维码下面那行链接。")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
                TextField("runode://pair?…", text: $model.linkText, axis: .vertical)
                    .font(.footnote.monospaced())
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .lineLimit(1...3)
                    .focused($focused)
                    .padding(12)
                    .background(Color(.tertiarySystemFill), in: .inner)
                HStack(spacing: 12) {
                    PasteButton(payloadType: String.self) { strings in
                        guard let link = strings.first else { return }
                        Task { @MainActor in
                            model.linkText = link
                            submit()
                        }
                    }
                    .labelStyle(.iconOnly)
                    .buttonBorderShape(.capsule)
                    Spacer()
                    Button("取消") { dismiss() }
                    Button("配对") { submit() }
                        .buttonStyle(.borderedProminent)
                        .disabled(model.linkText.isEmpty || model.isBusy)
                }
                .frame(minHeight: 44)
            }
            .padding(16)
            .fixedSize(horizontal: false, vertical: true)
            .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { height = $0 }
            .presentationDetents([.height(height)])
            .presentationDragIndicator(.visible)
            .preferredColorScheme(.dark)
            .onAppear { focused = true }
        }

        private func submit() {
            dismiss()
            Task { await model.submitLink() }
        }
    }
#endif
