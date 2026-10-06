#if os(iOS)
    import UIKit

    /// 键盘上方的一条辅助栏：软键盘上没有的 Esc、粘住的 Ctrl、Tab、方向键和几个常用符号，横着能拖。
    final class KeyboardAccessoryBar: UIInputView {
        private weak var owner: TerminalView?
        private var controlButton: UIButton?

        init(owner: TerminalView) {
            self.owner = owner
            super.init(frame: CGRect(x: 0, y: 0, width: 320, height: 44), inputViewStyle: .keyboard)
            allowsSelfSizing = true
            let scroll = UIScrollView()
            scroll.showsHorizontalScrollIndicator = false
            scroll.translatesAutoresizingMaskIntoConstraints = false
            addSubview(scroll)
            let stack = UIStackView()
            stack.axis = .horizontal
            stack.spacing = 6
            stack.translatesAutoresizingMaskIntoConstraints = false
            scroll.addSubview(stack)
            // 软键盘不弹出时（接了硬件键盘）这条栏贴在屏幕最底下，按钮要排在安全区里，避开 Home 条和圆角；
            // 栏自己的背景照样铺到底。横屏时左右也让开刘海。
            let safe = safeAreaLayoutGuide
            NSLayoutConstraint.activate([
                scroll.leadingAnchor.constraint(equalTo: safe.leadingAnchor),
                scroll.trailingAnchor.constraint(equalTo: safe.trailingAnchor),
                scroll.topAnchor.constraint(equalTo: topAnchor),
                scroll.bottomAnchor.constraint(equalTo: safe.bottomAnchor),
                scroll.heightAnchor.constraint(equalToConstant: 44),
                stack.leadingAnchor.constraint(equalTo: scroll.contentLayoutGuide.leadingAnchor, constant: 8),
                stack.trailingAnchor.constraint(equalTo: scroll.contentLayoutGuide.trailingAnchor, constant: -8),
                stack.topAnchor.constraint(equalTo: scroll.contentLayoutGuide.topAnchor, constant: 6),
                stack.bottomAnchor.constraint(equalTo: scroll.contentLayoutGuide.bottomAnchor, constant: -6),
                stack.heightAnchor.constraint(equalTo: scroll.frameLayoutGuide.heightAnchor, constant: -12),
            ])

            stack.addArrangedSubview(button("esc", label: "Esc") { $0.press(.escape) })
            let control = button("ctrl", label: "Ctrl（粘住）") { $0.controlLatched.toggle() }
            controlButton = control
            stack.addArrangedSubview(control)
            stack.addArrangedSubview(button("tab", label: "Tab") { $0.press(.tab) })
            stack.addArrangedSubview(button("arrow.left", symbol: true, label: "左") { $0.press(.left) })
            stack.addArrangedSubview(button("arrow.down", symbol: true, label: "下") { $0.press(.down) })
            stack.addArrangedSubview(button("arrow.up", symbol: true, label: "上") { $0.press(.up) })
            stack.addArrangedSubview(button("arrow.right", symbol: true, label: "右") { $0.press(.right) })
            for symbol in ["|", "~", "/", "-", "_", "`"] {
                stack.addArrangedSubview(button(symbol, label: symbol) { $0.typeText(symbol) })
            }
            stack.addArrangedSubview(button("doc.on.clipboard", symbol: true, label: "粘贴") { $0.paste(nil) })
            stack.addArrangedSubview(
                button("keyboard.chevron.compact.down", symbol: true, label: "收起键盘") { $0.resignFirstResponder() })
        }

        @available(*, unavailable)
        required init?(coder: NSCoder) {
            fatalError("init(coder:) is not supported")
        }

        func setControlLatched(_ latched: Bool) {
            controlButton?.isSelected = latched
            controlButton?.configuration?.baseBackgroundColor = latched ? .tintColor : .secondarySystemFill
            controlButton?.configuration?.baseForegroundColor = latched ? .white : .label
        }

        private func button(
            _ title: String, symbol: Bool = false, label: String, action: @escaping @MainActor (TerminalView) -> Void
        ) -> UIButton {
            var configuration = UIButton.Configuration.filled()
            configuration.baseBackgroundColor = .secondarySystemFill
            configuration.baseForegroundColor = .label
            configuration.cornerStyle = .medium
            configuration.contentInsets = NSDirectionalEdgeInsets(top: 4, leading: 10, bottom: 4, trailing: 10)
            if symbol {
                configuration.image = UIImage(systemName: title)
            } else {
                var attributed = AttributedString(title)
                attributed.font = .monospacedSystemFont(ofSize: 15, weight: .medium)
                configuration.attributedTitle = attributed
            }
            let button = UIButton(
                configuration: configuration,
                primaryAction: UIAction { [weak self] _ in
                    guard let owner = self?.owner else { return }
                    UIDevice.current.playInputClick()
                    action(owner)
                })
            button.accessibilityLabel = label
            return button
        }
    }

    extension KeyboardAccessoryBar: UIInputViewAudioFeedback {
        var enableInputClicksWhenVisible: Bool { true }
    }
#endif
