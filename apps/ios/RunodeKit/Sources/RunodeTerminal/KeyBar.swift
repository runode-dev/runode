#if os(iOS)
    import UIKit

    /// 一条快捷键栏的底座：左边一排横着能拖的键（`keys`），右端几个不跟着滚走的键（`pinTrailing`）。
    /// `resting` 为假时跟在软键盘上面，为真时是软键盘收着时页面底部的那条，背景透明。按钮的样子、按键音和
    /// 让开安全区都在这里，各条栏只管摆哪些键。
    open class KeyBar: UIInputView, UIInputViewAudioFeedback {
        /// 横着能拖的那排键。
        public let keys = UIStackView()
        private let scroll = UIScrollView()

        public init(resting: Bool) {
            super.init(
                frame: CGRect(x: 0, y: 0, width: 320, height: 44), inputViewStyle: resting ? .default : .keyboard)
            if resting { backgroundColor = .clear }
            allowsSelfSizing = true
            scroll.showsHorizontalScrollIndicator = false
            scroll.translatesAutoresizingMaskIntoConstraints = false
            addSubview(scroll)
            keys.axis = .horizontal
            keys.spacing = 6
            keys.translatesAutoresizingMaskIntoConstraints = false
            scroll.addSubview(keys)
            // 软键盘不弹出时（接了硬件键盘）这条栏贴在屏幕最底下，按钮要排在安全区里，避开 Home 条和圆角；
            // 栏自己的背景照样铺到底。横屏时左右也让开刘海。
            let safe = safeAreaLayoutGuide
            // 系统有时把栏高钉在 44，底部安全区却还没归零，这时让开安全区的这条让步，免得和高度冲突。
            let aboveHomeIndicator = scroll.bottomAnchor.constraint(equalTo: safe.bottomAnchor)
            aboveHomeIndicator.priority = .required - 1
            NSLayoutConstraint.activate([
                scroll.leadingAnchor.constraint(equalTo: safe.leadingAnchor),
                scroll.topAnchor.constraint(equalTo: topAnchor),
                aboveHomeIndicator,
                scroll.heightAnchor.constraint(equalToConstant: 44),
                keys.leadingAnchor.constraint(equalTo: scroll.contentLayoutGuide.leadingAnchor, constant: 8),
                keys.trailingAnchor.constraint(equalTo: scroll.contentLayoutGuide.trailingAnchor, constant: -8),
                keys.topAnchor.constraint(equalTo: scroll.contentLayoutGuide.topAnchor, constant: 6),
                keys.bottomAnchor.constraint(equalTo: scroll.contentLayoutGuide.bottomAnchor, constant: -6),
                keys.heightAnchor.constraint(equalTo: scroll.frameLayoutGuide.heightAnchor, constant: -12),
            ])
        }

        @available(*, unavailable)
        public required init?(coder: NSCoder) {
            fatalError("init(coder:) is not supported")
        }

        /// 把 `buttons` 从左到右固定在右端，能拖的那排键排到它们左边为止。只调一次。
        public func pinTrailing(_ buttons: [UIButton]) {
            var trailing = safeAreaLayoutGuide.trailingAnchor
            var gap: CGFloat = -8
            for button in buttons.reversed() {
                button.translatesAutoresizingMaskIntoConstraints = false
                addSubview(button)
                NSLayoutConstraint.activate([
                    button.trailingAnchor.constraint(equalTo: trailing, constant: gap),
                    button.centerYAnchor.constraint(equalTo: scroll.centerYAnchor),
                    button.heightAnchor.constraint(equalTo: scroll.heightAnchor, constant: -12),
                ])
                trailing = button.leadingAnchor
                gap = -6
            }
            scroll.trailingAnchor.constraint(equalTo: trailing, constant: -6).isActive = true
        }

        /// 一个键：`title` 是字或者（`symbol` 为真时）SF Symbol 的名字，`label` 报给辅助工具。按下时响一声
        /// 按键音再做 `action`。
        public func key(
            _ title: String, symbol: Bool = false, label: String, action: @escaping @MainActor () -> Void
        ) -> UIButton {
            let button = styled(title, symbol: symbol, label: label)
            button.addAction(
                UIAction { _ in
                    UIDevice.current.playInputClick()
                    action()
                }, for: .primaryActionTriggered)
            return button
        }

        /// 样子和 `key` 一样、不带动作的按钮，给要自己接按下、松开的键用。
        public func styled(_ title: String, symbol: Bool = false, label: String) -> UIButton {
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
            let button = UIButton(configuration: configuration)
            button.accessibilityLabel = label
            return button
        }

        /// 按下去的样子（粘住、按住的修饰键，开着的键盘）：反色，和 app 里的实心按钮一样黑白为主。不用
        /// `tintColor`：跟在软键盘上面时这条栏在键盘的窗口里，接不到 app 设的强调色，会是系统的蓝色。
        public func highlight(_ button: UIButton, _ on: Bool) {
            button.isSelected = on
            button.configuration?.baseBackgroundColor = on ? .label : .secondarySystemFill
            button.configuration?.baseForegroundColor = on ? .systemBackground : .label
        }

        public var enableInputClicksWhenVisible: Bool { true }
    }
#endif
