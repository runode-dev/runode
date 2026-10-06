#if os(iOS)
    import RunodeProtocol
    import RunodeTerminal
    import SwiftUI

    /// 终端页：终端视图铺满，上面一条状态（连接中、断开、已退出），右上角菜单里切尺寸、结束会话。
    struct TerminalScreen: View {
        @Bindable var model: TerminalModel

        var body: some View {
            TerminalViewRepresentable(model: model)
                .ignoresSafeArea(.container, edges: [.bottom, .horizontal])
                .overlay(alignment: .top) {
                    if let banner {
                        Text(banner)
                            .font(.footnote)
                            .padding(.horizontal, 12)
                            .padding(.vertical, 6)
                            .background(.regularMaterial, in: Capsule())
                            .padding(.top, 8)
                    }
                }
                .overlay(alignment: .bottomTrailing) {
                    if model.scrolledBack {
                        Button("回到最新", systemImage: "arrow.down.to.line") {
                            model.scrollToBottom()
                        }
                        .buttonStyle(.borderedProminent)
                        .padding()
                    }
                }
                .navigationTitle(model.title)
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .primaryAction) {
                        Menu {
                            Section(Presentation.sizeOwnership(model.sizeOwnership)) {
                                Button("适配本机屏幕", systemImage: "iphone") { model.fitToScreen() }
                                    .disabled(model.fitsScreen && model.sizeOwnership == .mine)
                                Button("跟随 Mac 的尺寸", systemImage: "laptopcomputer") { model.followHostSize() }
                                    .disabled(!model.fitsScreen)
                            }
                            if let size = model.gridSize {
                                Text("网格 \(Presentation.gridSize(size))")
                            }
                            Button("结束会话", systemImage: "xmark.circle", role: .destructive) {
                                model.isConfirmingKill = true
                            }
                        } label: {
                            Image(systemName: "ellipsis.circle")
                        }
                        .accessibilityLabel("终端选项")
                    }
                }
                .confirmationDialog("结束这个终端？", isPresented: $model.isConfirmingKill, titleVisibility: .visible) {
                    Button("结束", role: .destructive) { model.kill() }
                } message: {
                    Text("里面正在跑的程序会收到 SIGHUP 并退出。")
                }
        }

        private var banner: String? {
            switch model.phase {
            case .connecting: "正在连接…"
            case .replaying, .live: model.errorMessage
            case .exited(let status?): "shell 已退出（退出码 \(status)）"
            case .exited(nil): "shell 已退出"
            case .disconnected(let reason): reason
            case .gone(let message): "这个终端已经不在了：\(message)"
            }
        }
    }

    /// 把 UIKit 的 `TerminalView` 嵌进 SwiftUI，用户的输入转给视图模型。
    struct TerminalViewRepresentable: UIViewRepresentable {
        let model: TerminalModel

        func makeCoordinator() -> Coordinator {
            Coordinator(model: model)
        }

        func makeUIView(context: Context) -> TerminalView {
            let view = TerminalView(frame: .zero)
            view.delegate = context.coordinator
            model.attachDisplay(view)
            return view
        }

        func updateUIView(_ view: TerminalView, context: Context) {}

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
        }
    }
#endif
