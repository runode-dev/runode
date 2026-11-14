#if os(iOS)
    import SwiftUI

    /// 提问的选项怎么排。
    enum PromptOptionLayout {
        /// 一项一行，编号块加原文：首页等你回答的卡片。
        case rows
        /// `AgentPrompt.compactOptions` 并排成键，还有别的选项时全部收进「⋯」菜单：会话列表的卡片。
        case compact
    }

    /// 认出来的提问（`AgentPrompt`）的选项，按下去交给 `press` 发编号键。默认项是琥珀色，其余中性；
    /// 原文在 `rows` 里至多折成两行、在 `compact` 里只占一行，再长就截断。
    struct PromptOptions: View {
        let prompt: AgentPrompt
        let layout: PromptOptionLayout
        let press: (QuickKey) -> Void
        @Environment(\.themeColors) private var colors
        @ScaledMetric(relativeTo: .footnote) private var numberSize: CGFloat = 24

        var body: some View {
            switch layout {
            case .rows: rows
            case .compact: compact
            }
        }

        private var rows: some View {
            VStack(spacing: 6) {
                ForEach(prompt.options) { option in
                    Button {
                        press(option.key)
                    } label: {
                        HStack(spacing: 12) {
                            Text(verbatim: String(option.number))
                                .font(.footnote.monospaced())
                                .foregroundStyle(option.selected ? .primary : .secondary)
                                .frame(width: numberSize, height: numberSize)
                                .background(
                                    option.selected ? colors.onWaitingFill.opacity(0.18) : colors.well,
                                    in: RoundedRectangle(cornerRadius: 7, style: .continuous))
                            Text(verbatim: option.text)
                                .font(.subheadline)
                                .lineLimit(2)
                                .multilineTextAlignment(.leading)
                                .truncationMode(.tail)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .padding(.leading, 10)
                        .padding(.trailing, 14)
                        .padding(.vertical, 6)
                    }
                    .buttonStyle(SurfaceButtonStyle(option.selected ? .waiting : .raised, height: 46))
                    .optionAccessibility(option)
                }
            }
        }

        private var compact: some View {
            let shown = prompt.compactOptions
            return HStack(spacing: 6) {
                ForEach(shown) { option in
                    Button {
                        press(option.key)
                    } label: {
                        HStack(spacing: 6) {
                            Text(verbatim: String(option.number))
                                .font(.callout.monospaced())
                                .foregroundStyle(.secondary)
                            Text(verbatim: option.text)
                                .lineLimit(1)
                                .truncationMode(.tail)
                        }
                        .padding(.horizontal, 12)
                        .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(SurfaceButtonStyle(option.selected ? .waiting : .raised))
                    .optionAccessibility(option)
                }
                if prompt.options.count > shown.count {
                    Menu {
                        ForEach(prompt.options) { option in
                            Button {
                                press(option.key)
                            } label: {
                                Text(verbatim: "\(option.number). \(option.text)")
                            }
                            .accessibilityLabel("发送 \(option.key.accessibilityLabel)")
                        }
                    } label: {
                        Image(systemName: "ellipsis")
                            .frame(width: 48)
                            .actionSurface(.raised)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("看全部 \(prompt.options.count) 个选项")
                }
            }
        }
    }

    extension View {
        /// 选项键的读屏：「发送 编号：原文」，默认项带上选中。
        func optionAccessibility(_ option: AgentPrompt.Option) -> some View {
            accessibilityLabel("发送 \(option.key.accessibilityLabel)")
                .accessibilityAddTraits(option.selected ? .isSelected : [])
        }
    }
#endif
