#if os(iOS)
    import RunodeConnection
    import RunodeProtocol
    import SwiftUI

    extension Color {
        init(_ rgb: Rgb) {
            self.init(.sRGB, red: Double(rgb.r) / 255, green: Double(rgb.g) / 255, blue: Double(rgb.b) / 255)
        }
    }

    extension Rgb {
        /// 按相对亮度看是深色还是浅色背景，决定上面的导航栏、底栏用深色还是浅色模式。
        var isDark: Bool {
            let luminance = 0.2126 * Double(r) + 0.7152 * Double(g) + 0.0722 * Double(b)
            return luminance < 128
        }
    }

    /// agent 的状态：图标加文字，不只靠颜色区分。
    struct AgentBadge: View {
        let agent: Agent?

        var body: some View {
            if let status = Presentation.agentStatus(agent) {
                let group: SessionGroup =
                    switch status.state {
                    case .blocked: .waiting
                    case .working: .working
                    default: .other
                    }
                Label(status.text, systemImage: group == .other ? "moon.zzz.fill" : Presentation.symbol(for: group))
                    .font(.caption.weight(.semibold))
                    .labelStyle(.titleAndIcon)
                    .foregroundStyle(Self.tint(for: group))
                    .padding(.horizontal, 8)
                    .padding(.vertical, 3)
                    .background(Self.tint(for: group).opacity(0.14), in: Capsule())
                    .accessibilityElement(children: .combine)
            }
        }

        static func tint(for group: SessionGroup) -> Color {
            switch group {
            case .waiting: .orange
            case .working: .blue
            case .other: .secondary
            }
        }
    }

    /// 快速回复栏：一排常用按键，加一个单行文本框（发送时粘贴进去再按回车）。会话列表上等回答的
    /// 会话、终端页底部都用它。
    struct QuickReplyBar: View {
        @Bindable var model: QuickReplyModel
        /// 栏的标题，比如「Claude Code 在等你回答」；为空时不显示。
        var prompt: String?

        var body: some View {
            VStack(alignment: .leading, spacing: 8) {
                if let prompt {
                    Label(prompt, systemImage: "exclamationmark.bubble.fill")
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(.orange)
                }
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        ForEach(QuickKey.standard) { key in
                            Button {
                                Task { await model.press(key) }
                            } label: {
                                Text(key.label)
                                    .font(.body.monospaced().weight(.semibold))
                                    .frame(minWidth: 30, minHeight: 30)
                            }
                            .buttonStyle(.bordered)
                            .accessibilityLabel("发送 \(key.accessibilityLabel)")
                        }
                    }
                }
                .scrollClipDisabled()
                HStack(spacing: 8) {
                    TextField("输入回复，发送后按回车", text: $model.draft)
                        .textFieldStyle(.roundedBorder)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .submitLabel(.send)
                        .onSubmit { Task { await model.sendDraft() } }
                    Button {
                        Task { await model.sendDraft() }
                    } label: {
                        Image(systemName: "arrow.up.circle.fill")
                            .font(.title2)
                            .frame(minWidth: 44, minHeight: 44)
                    }
                    .buttonStyle(.borderless)
                    .disabled(!model.canSendDraft)
                    .accessibilityLabel("发送回复")
                }
                if let error = model.errorMessage {
                    Label(error, systemImage: "exclamationmark.triangle.fill")
                        .font(.footnote)
                        .foregroundStyle(.red)
                }
            }
            .sensoryFeedback(.success, trigger: model.deliveredCount)
            .sensoryFeedback(.error, trigger: model.failedCount)
        }
    }

    /// 连接状态：图标、文字（重连时带倒计时），断开时有「重试」。
    struct ConnectionStatusRow: View {
        let state: LinkState
        let onRetry: () -> Void

        var body: some View {
            HStack(spacing: 10) {
                switch state {
                case .connecting, .idle:
                    ProgressView()
                case .waiting:
                    Image(systemName: "wifi.exclamationmark").foregroundStyle(.orange)
                case .failed:
                    Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.red)
                case .connected:
                    Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
                }
                TimelineView(.periodic(from: .now, by: 1)) { context in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(Presentation.linkStatus(state, now: context.date))
                            .font(.subheadline.weight(.semibold))
                        if case .failed = state {
                            Text(Presentation.linkState(state))
                                .font(.footnote)
                                .foregroundStyle(.secondary)
                        } else if case .waiting(let reason, _) = state {
                            Text(reason)
                                .font(.footnote)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                Spacer(minLength: 8)
                switch state {
                case .waiting, .failed:
                    Button("重试", action: onRetry)
                        .buttonStyle(.bordered)
                        .frame(minHeight: 44)
                default:
                    EmptyView()
                }
            }
            .accessibilityElement(children: .contain)
        }
    }
#endif
