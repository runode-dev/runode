#if os(iOS)
    import RunodeConnection
    import RunodeProtocol
    import RunodeTerminal
    import SwiftUI
    import UIKit

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

    /// 屏幕最后几行的预览：等宽小字，浅底圆角，每行单独截断（长行折下去会把别的行挤掉）。
    struct ScreenPreview: View {
        let lines: [String]
        @Environment(\.dynamicTypeSize) private var dynamicTypeSize

        var body: some View {
            VStack(alignment: .leading, spacing: 2) {
                ForEach(Array(lines.enumerated()), id: \.offset) { _, line in
                    text(line)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
            }
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(8)
            .background(Color(.secondarySystemFill), in: RoundedRectangle(cornerRadius: 8))
            .accessibilityLabel("屏幕预览：\(lines.joined(separator: "，"))")
        }

        /// 预览的字号：等宽小字，跟着动态字体走。
        private var size: CGFloat {
            let traits = UITraitCollection(preferredContentSizeCategory: UIContentSizeCategory(dynamicTypeSize))
            return UIFont.preferredFont(forTextStyle: .caption1, compatibleWith: traits).pointSize
        }

        /// 一行预览：私用区的字（提示符里的 Powerline、Nerd Font 图标）那几段用随包的符号字体，其余用
        /// 等宽系统字体。SwiftUI 的 `Font` 不带 Core Text 的后备列表，只能这样分段指定。
        private func text(_ line: String) -> Text {
            let size = size
            var result = AttributedString()
            var run = ""
            var runIsSymbol = false
            func flush() {
                guard !run.isEmpty else { return }
                var piece = AttributedString(run)
                if runIsSymbol, let name = SymbolFont.postScriptName {
                    piece.font = .custom(name, fixedSize: size)
                } else {
                    piece.font = .system(size: size, design: .monospaced)
                }
                result += piece
                run = ""
            }
            for character in line {
                let symbol = character.unicodeScalars.first.map(SymbolFont.isPrivateUse) ?? false
                if symbol != runIsSymbol {
                    flush()
                    runIsSymbol = symbol
                }
                run.append(character)
            }
            flush()
            return Text(result)
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
