#if os(iOS)
    import RunodeConnection
    import SwiftUI
    import UIKit

    /// 配对页：先说清电脑上要做的两步（打开远程访问、运行 `runode remote pair`），再扫码；没有摄像头时
    /// 粘贴链接。配好后显示结果，点「完成」进到这台电脑。
    struct PairingView: View {
        @Bindable var model: PairingModel
        /// 关掉配对页；配对成功时带着那台电脑。
        let onFinish: (MachineRecord?) -> Void

        var body: some View {
            NavigationStack {
                List {
                    Section {
                        ListSectionHeader(title: "在电脑上")
                        VStack(alignment: .leading, spacing: 12) {
                            CommandRow(
                                step: 1, text: "在 Runode 的配置文件里加上这一行，打开远程访问：",
                                command: "remote-access = true")
                            Divider()
                            CommandRow(step: 2, text: "在电脑的终端里运行：", command: "runode remote pair")
                        }
                        .cardBackground()
                        .plainListRow()
                        ListSectionFooter(text: "终端里会出现一个二维码，5 分钟内有效。")
                    }
                    Section {
                        ListSectionHeader(title: "扫二维码")
                        scanner
                    }
                    Section {
                        ListSectionHeader(title: "或者粘贴配对链接")
                        VStack(alignment: .leading, spacing: 12) {
                            TextField("runode://pair?…", text: $model.linkText, axis: .vertical)
                                .font(.footnote.monospaced())
                                .textInputAutocapitalization(.never)
                                .autocorrectionDisabled()
                                .lineLimit(1...4)
                                .padding(10)
                                .background(Color(.tertiarySystemFill), in: .inner)
                            HStack {
                                PasteButton(payloadType: String.self) { strings in
                                    guard let link = strings.first else { return }
                                    Task { @MainActor in
                                        model.linkText = link
                                        await model.submitLink()
                                    }
                                }
                                .labelStyle(.titleAndIcon)
                                .buttonBorderShape(.capsule)
                                Spacer()
                                Button("配对") {
                                    Task { await model.submitLink() }
                                }
                                .buttonStyle(.borderedProminent)
                                .disabled(model.linkText.isEmpty || model.isBusy)
                            }
                            .frame(minHeight: 44)
                        }
                        .cardBackground()
                        .plainListRow()
                    }
                    if model.phase != .idle {
                        Section {
                            status
                                .cardBackground()
                                .plainListRow()
                        }
                    }
                }
                .cardList()
                .navigationTitle("配对电脑")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("关闭") { onFinish(nil) }
                    }
                }
                .animation(.default, value: model.phase)
            }
        }

        /// 取景框；扫不了码（没有摄像头、没给权限）时缩成一行说明，把地方让给粘贴链接。
        @ViewBuilder
        private var scanner: some View {
            if let message = model.scannerUnavailable {
                Label(message, systemImage: "camera.badge.ellipsis")
                    .foregroundStyle(.secondary)
                    .cardBackground()
                    .plainListRow()
            } else {
                QRScannerView(
                    onCode: { code in Task { await model.scanned(code) } },
                    onUnavailable: { message in model.scannerUnavailable = message }
                )
                .frame(height: 240)
                .clipShape(.card)
                .plainListRow()
                .accessibilityLabel("二维码取景框")
            }
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
                VStack(alignment: .leading, spacing: 10) {
                    Label("已和 \(machine.name) 配对", systemImage: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                        .font(.headline)
                    Button("完成") { onFinish(machine) }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.large)
                }
            case .failed(let message):
                VStack(alignment: .leading, spacing: 10) {
                    Label(message, systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.red)
                    Button("重新扫码") { model.reset() }
                        .buttonStyle(.bordered)
                }
            }
        }
    }

    /// 一步操作：说明加一条等宽显示、能一键复制的命令。
    private struct CommandRow: View {
        let step: Int
        let text: String
        let command: String
        @State private var copied = false

        var body: some View {
            VStack(alignment: .leading, spacing: 8) {
                Label {
                    Text(text)
                } icon: {
                    Image(systemName: "\(step).circle.fill")
                        .foregroundStyle(.tint)
                }
                HStack(spacing: 8) {
                    Text(command)
                        .font(.callout.monospaced())
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 8)
                        .background(Color(.tertiarySystemFill), in: .inner)
                    Button {
                        UIPasteboard.general.string = command
                        copied = true
                        Task {
                            try? await Task.sleep(for: .seconds(1.5))
                            copied = false
                        }
                    } label: {
                        Image(systemName: copied ? "checkmark" : "doc.on.doc")
                            .frame(minWidth: 44, minHeight: 44)
                            .contentTransition(.symbolEffect(.replace))
                    }
                    .buttonStyle(.borderless)
                    .accessibilityLabel(copied ? "已复制" : "复制 \(command)")
                }
            }
            .sensoryFeedback(.success, trigger: copied) { _, new in new }
            .padding(.vertical, 2)
        }
    }
#endif
