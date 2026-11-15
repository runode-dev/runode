#if os(iOS)
    import SwiftUI

    /// 提问的选项怎么排。
    enum PromptOptionLayout {
        /// 一项一行，编号块加原文：首页等你回答的卡片。
        case rows
        /// `AgentPrompt.compactOptions` 并排成键，还有别的选项时全部收进「⋯」菜单：会话列表的卡片。
        case compact
    }

    /// 认出来的提问（`AgentPrompt`）的选项，按下去交给 `press` 发编号键。默认项（`AgentPrompt.isDefault`）
    /// 是琥珀色，其余中性；有复选框的项在编号后画出勾没勾，原文去掉复选框。原文在 `rows` 里至多折成两行、
    /// 在 `compact` 里只占一行，再长就截断。
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
                    let isDefault = prompt.isDefault(option)
                    Button {
                        press(option.key)
                    } label: {
                        HStack(spacing: 12) {
                            Text(verbatim: String(option.number))
                                .font(.footnote.monospaced())
                                .foregroundStyle(isDefault ? .primary : .secondary)
                                .frame(width: numberSize, height: numberSize)
                                .background(
                                    isDefault ? colors.onWaitingFill.opacity(0.18) : colors.well,
                                    in: RoundedRectangle(cornerRadius: 7, style: .continuous))
                            if let checkbox = option.checkbox {
                                OptionCheckbox(checked: checkbox.checked)
                                    .font(.subheadline)
                            }
                            Text(verbatim: option.displayText)
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
                    .buttonStyle(SurfaceButtonStyle(isDefault ? .waiting : .raised, height: 46))
                    .optionAccessibility(option, in: prompt)
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
                            if let checkbox = option.checkbox {
                                OptionCheckbox(checked: checkbox.checked)
                            }
                            Text(verbatim: option.displayText)
                                .lineLimit(1)
                                .truncationMode(.tail)
                        }
                        .padding(.horizontal, 12)
                        .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(SurfaceButtonStyle(prompt.isDefault(option) ? .waiting : .raised))
                    .optionAccessibility(option, in: prompt)
                }
                if prompt.options.count > shown.count {
                    Menu {
                        ForEach(prompt.options) { option in
                            let title = Text(verbatim: "\(option.number). \(option.displayText)")
                            // 菜单里的内容会转成系统菜单项，自定义的图标和读屏值带不过去；复选框用 `Toggle`，
                            // 系统画勾、读屏报勾没勾。勾没勾以屏幕为准，按下只发编号键。
                            if let checkbox = option.checkbox {
                                Toggle(isOn: Binding(get: { checkbox.checked }, set: { _ in press(option.key) })) {
                                    title
                                }
                                .accessibilityLabel("发送 \(option.key.accessibilityLabel)")
                            } else {
                                Button {
                                    press(option.key)
                                } label: {
                                    title
                                }
                                .accessibilityLabel("发送 \(option.key.accessibilityLabel)")
                            }
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

    /// 多选题选项的复选框，颜色跟着旁边的字：勾上的用前景色，没勾的淡一级。读屏不念它。
    struct OptionCheckbox: View {
        let checked: Bool

        var body: some View {
            Image(systemName: checked ? "checkmark.square.fill" : "square")
                .foregroundStyle(checked ? .primary : .secondary)
                .accessibilityHidden(true)
        }
    }

    extension View {
        /// 选项键的读屏：「发送 编号：原文」，有复选框的念勾没勾，默认项带上选中。
        func optionAccessibility(_ option: AgentPrompt.Option, in prompt: AgentPrompt) -> some View {
            accessibilityLabel("发送 \(option.key.accessibilityLabel)")
                .checkboxAccessibility(option)
                .accessibilityAddTraits(prompt.isDefault(option) ? .isSelected : [])
        }

        /// 有复选框的选项念勾没勾。
        func checkboxAccessibility(_ option: AgentPrompt.Option) -> some View {
            accessibilityValue(
                option.checkbox.map { $0.checked ? String(localized: "已勾选") : String(localized: "未勾选") } ?? "")
        }
    }
#endif
