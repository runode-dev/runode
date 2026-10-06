#if os(iOS)
    import RunodeConnection
    import SwiftUI

    /// 配对页：上面扫码，下面可以粘贴链接。配好后显示结果，点「完成」进到这台 Mac。
    struct PairingView: View {
        @Bindable var model: PairingModel
        /// 关掉配对页；配对成功时带着那台 Mac。
        let onFinish: (MachineRecord?) -> Void

        var body: some View {
            NavigationStack {
                Form {
                    Section {
                        scanner
                            .frame(height: 260)
                            .listRowInsets(EdgeInsets())
                    } footer: {
                        Text("在 Mac 上的 runode 里打开远程访问，点「配对新设备」，扫出现的二维码。")
                    }
                    Section("或者粘贴配对链接") {
                        TextField("runode://pair?…", text: $model.linkText, axis: .vertical)
                            .font(.footnote.monospaced())
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .lineLimit(1...4)
                        Button("配对") {
                            Task { await model.submitLink() }
                        }
                        .disabled(model.linkText.isEmpty || model.isBusy)
                    }
                    Section {
                        status
                    }
                }
                .navigationTitle("配对 Mac")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("关闭") { onFinish(nil) }
                    }
                }
            }
        }

        @ViewBuilder
        private var scanner: some View {
            if let scannerMessage = model.scannerUnavailable {
                ContentUnavailableView("没法扫码", systemImage: "camera", description: Text(scannerMessage))
            } else {
                QRScannerView(
                    onCode: { code in Task { await model.scanned(code) } },
                    onUnavailable: { message in model.scannerUnavailable = message })
            }
        }

        @ViewBuilder
        private var status: some View {
            switch model.phase {
            case .idle:
                Text("等待扫码或粘贴链接")
                    .foregroundStyle(.secondary)
            case .pairing(let hostName):
                HStack {
                    ProgressView()
                    Text("正在和 \(hostName) 配对…")
                }
            case .paired(let machine):
                VStack(alignment: .leading, spacing: 8) {
                    Label("已和 \(machine.name) 配对", systemImage: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                    Button("完成") { onFinish(machine) }
                        .buttonStyle(.borderedProminent)
                }
            case .failed(let message):
                VStack(alignment: .leading, spacing: 8) {
                    Label(message, systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.red)
                    Button("重试") { model.reset() }
                }
            }
        }
    }
#endif
