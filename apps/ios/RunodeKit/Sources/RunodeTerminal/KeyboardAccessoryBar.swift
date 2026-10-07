#if os(iOS)
    import UIKit

    /// 键盘上方的一条辅助栏：软键盘上没有的 Esc、粘住的 Ctrl、Tab，回车（软键盘收着时也按得到）、方向键
    /// 和几个常用符号，横着能拖。软键盘收着时界面底部放一条一样的（`resting` 为真，见
    /// `TerminalView.makeRestingKeyBar`）。开关软键盘的键固定在右端，不跟着别的键滚走，两条都用键盘图标：
    /// 底部那条是「打开键盘」，跟着软键盘的那条是「收起键盘」，按下去的样子表示键盘开着。
    final class KeyboardAccessoryBar: UIInputView {
        private weak var owner: TerminalView?
        private var controlButton: UIButton?

        init(owner: TerminalView, resting: Bool = false) {
            self.owner = owner
            super.init(
                frame: CGRect(x: 0, y: 0, width: 320, height: 44), inputViewStyle: resting ? .default : .keyboard)
            if resting { backgroundColor = .clear }
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
            let keyboard =
                resting
                ? button("keyboard", symbol: true, label: "打开键盘") { $0.showKeyboard() }
                : button("keyboard", symbol: true, label: "收起键盘") { $0.resignFirstResponder() }
            if !resting { highlight(keyboard, true) }
            keyboard.translatesAutoresizingMaskIntoConstraints = false
            addSubview(keyboard)
            // 软键盘不弹出时（接了硬件键盘）这条栏贴在屏幕最底下，按钮要排在安全区里，避开 Home 条和圆角；
            // 栏自己的背景照样铺到底。横屏时左右也让开刘海。
            let safe = safeAreaLayoutGuide
            NSLayoutConstraint.activate([
                scroll.leadingAnchor.constraint(equalTo: safe.leadingAnchor),
                scroll.trailingAnchor.constraint(equalTo: keyboard.leadingAnchor, constant: -2),
                keyboard.trailingAnchor.constraint(equalTo: safe.trailingAnchor, constant: -8),
                keyboard.centerYAnchor.constraint(equalTo: scroll.centerYAnchor),
                keyboard.heightAnchor.constraint(equalTo: scroll.heightAnchor, constant: -12),
                scroll.topAnchor.constraint(equalTo: topAnchor),
                scroll.bottomAnchor.constraint(equalTo: safe.bottomAnchor),
                scroll.heightAnchor.constraint(equalToConstant: 44),
                stack.leadingAnchor.constraint(equalTo: scroll.contentLayoutGuide.leadingAnchor, constant: 8),
                stack.trailingAnchor.constraint(equalTo: scroll.contentLayoutGuide.trailingAnchor, constant: -8),
                stack.topAnchor.constraint(equalTo: scroll.contentLayoutGuide.topAnchor, constant: 6),
                stack.bottomAnchor.constraint(equalTo: scroll.contentLayoutGuide.bottomAnchor, constant: -6),
                stack.heightAnchor.constraint(equalTo: scroll.frameLayoutGuide.heightAnchor, constant: -12),
            ])

            stack.addArrangedSubview(button("escape", symbol: true, label: "Esc") { $0.press(.escape) })
            // 不用 `control` 符号：它照 ⌃ 的样子画在字框的上半截，放在按钮里偏上；`chevron.up` 形状一样、上下居中。
            let control = button("chevron.up", symbol: true, label: "Ctrl（粘住）") { $0.controlLatched.toggle() }
            controlButton = control
            stack.addArrangedSubview(control)
            stack.addArrangedSubview(button("arrow.right.to.line", symbol: true, label: "Tab") { $0.press(.tab) })
            stack.addArrangedSubview(button("return", symbol: true, label: "回车") { $0.press(.enter) })
            stack.addArrangedSubview(button("arrowtriangle.left.fill", symbol: true, label: "左") { $0.press(.left) })
            stack.addArrangedSubview(button("arrowtriangle.down.fill", symbol: true, label: "下") { $0.press(.down) })
            stack.addArrangedSubview(button("arrowtriangle.up.fill", symbol: true, label: "上") { $0.press(.up) })
            stack.addArrangedSubview(button("arrowtriangle.right.fill", symbol: true, label: "右") { $0.press(.right) })
            for symbol in ["|", "~", "/", "-", "_", "`"] {
                stack.addArrangedSubview(button(symbol, label: symbol) { $0.typeText(symbol) })
            }
            stack.addArrangedSubview(button("doc.on.clipboard", symbol: true, label: "粘贴") { $0.paste(nil) })
        }

        @available(*, unavailable)
        required init?(coder: NSCoder) {
            fatalError("init(coder:) is not supported")
        }

        func setControlLatched(_ latched: Bool) {
            guard let controlButton else { return }
            highlight(controlButton, latched)
        }

        /// 按下去的样子（粘住的 Ctrl、开着的键盘）：反色，和 app 里的实心按钮一样黑白为主。不用 `tintColor`：
        /// 这条栏在键盘的窗口里，接不到 app 设的强调色，会是系统的蓝色。
        private func highlight(_ button: UIButton, _ on: Bool) {
            button.isSelected = on
            button.configuration?.baseBackgroundColor = on ? .label : .secondarySystemFill
            button.configuration?.baseForegroundColor = on ? .systemBackground : .label
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
                // 比旁边 15 点的字小两号：图标比字显得满，小一号时方向键的实心三角还是太重。
                configuration.preferredSymbolConfigurationForImage = UIImage.SymbolConfiguration(
                    pointSize: 11, weight: .medium)
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
