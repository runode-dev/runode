#if os(iOS)
    import UIKit

    /// 按键栏上按钮的配色。
    public struct KeyBarPalette: Equatable {
        public var key: UIColor
        public var border: UIColor
        public var label: UIColor
        /// 按下去的样子（粘住的修饰键、开着的键盘、正在录音）的底和字。
        public var selectedKey: UIColor
        public var selectedLabel: UIColor

        public init(key: UIColor, border: UIColor, label: UIColor, selectedKey: UIColor, selectedLabel: UIColor) {
            self.key = key
            self.border = border
            self.label = label
            self.selectedKey = selectedKey
            self.selectedLabel = selectedLabel
        }
    }

    /// 一条快捷键栏的底座：左边一排横着能拖的键（`keys`），右端几个不跟着滚走的键（`pinTrailing`）。
    /// `resting` 为假时跟在软键盘上面，为真时是软键盘收着时页面底部的那条，背景透明。按钮的样子、按键音和
    /// 让开安全区都在这里，各条栏只管摆哪些键。栏高 `height`，按钮上下各让 `keyInset`。
    open class KeyBar: UIInputView, UIInputViewAudioFeedback, UIScrollViewDelegate {
        /// 横着能拖的那排键。
        public let keys = UIStackView()
        /// 按钮的配色；为空时用系统色。跟在软键盘上的栏在键盘的窗口里，留空才和键盘协调。
        public var palette: KeyBarPalette? {
            didSet {
                guard palette != oldValue else { return }
                for button in allButtons { recolor(button) }
            }
        }
        private let scroll = LayoutReportingScrollView()
        /// 那排键还能往哪边拖，哪边就渐隐下去，看得出后面还有键；放得下时不遮。
        private let fade = CAGradientLayer()
        private static let fadeWidth: CGFloat = 28
        /// `styled` 做过的按钮，换配色时逐个重新上色。
        private var allButtons: [UIButton] = []
        private let keyInset: CGFloat

        public init(resting: Bool, height: CGFloat = 44, keyInset: CGFloat = 6) {
            self.keyInset = keyInset
            super.init(
                frame: CGRect(x: 0, y: 0, width: 320, height: height), inputViewStyle: resting ? .default : .keyboard)
            if resting { backgroundColor = .clear }
            allowsSelfSizing = true
            scroll.showsHorizontalScrollIndicator = false
            scroll.delegate = self
            scroll.didLayout = { [weak self] in self?.updateFade() }
            fade.startPoint = CGPoint(x: 0, y: 0.5)
            fade.endPoint = CGPoint(x: 1, y: 0.5)
            scroll.translatesAutoresizingMaskIntoConstraints = false
            addSubview(scroll)
            keys.axis = .horizontal
            keys.spacing = 4
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
                scroll.heightAnchor.constraint(equalToConstant: height),
                keys.leadingAnchor.constraint(equalTo: scroll.contentLayoutGuide.leadingAnchor, constant: 8),
                keys.trailingAnchor.constraint(equalTo: scroll.contentLayoutGuide.trailingAnchor, constant: -8),
                keys.topAnchor.constraint(equalTo: scroll.contentLayoutGuide.topAnchor, constant: keyInset),
                keys.bottomAnchor.constraint(equalTo: scroll.contentLayoutGuide.bottomAnchor, constant: -keyInset),
                keys.heightAnchor.constraint(equalTo: scroll.frameLayoutGuide.heightAnchor, constant: -2 * keyInset),
            ])
        }

        @available(*, unavailable)
        public required init?(coder: NSCoder) {
            fatalError("init(coder:) is not supported")
        }

        public func scrollViewDidScroll(_ scrollView: UIScrollView) {
            updateFade()
        }

        private func updateFade() {
            let width = scroll.bounds.width
            let overflow = scroll.contentSize.width - width
            guard width > 0, overflow > 1 else {
                scroll.layer.mask = nil
                return
            }
            let offset = scroll.contentOffset.x
            let shown = UIColor.black.cgColor
            let hidden = UIColor.clear.cgColor
            let edge = min(Self.fadeWidth / width, 0.5)
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            // 遮罩在滚动视图的图层坐标里，要跟着拖动的偏移走。
            fade.frame = scroll.bounds
            fade.colors = [offset > 1 ? hidden : shown, shown, shown, offset < overflow - 1 ? hidden : shown]
            fade.locations = [0, NSNumber(value: edge), NSNumber(value: 1 - edge), 1]
            CATransaction.commit()
            scroll.layer.mask = fade
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
                    button.heightAnchor.constraint(equalTo: scroll.heightAnchor, constant: -2 * keyInset),
                ])
                trailing = button.leadingAnchor
                gap = -keys.spacing
            }
            scroll.trailingAnchor.constraint(equalTo: trailing, constant: -keys.spacing).isActive = true
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
            configuration.cornerStyle = .fixed
            configuration.background.cornerRadius = 10
            configuration.contentInsets = NSDirectionalEdgeInsets(top: 4, leading: 10, bottom: 4, trailing: 10)
            if symbol {
                configuration.image = UIImage(systemName: title)
                configuration.preferredSymbolConfigurationForImage = UIImage.SymbolConfiguration(
                    pointSize: 13, weight: .semibold)
            } else {
                var attributed = AttributedString(title)
                attributed.font = .monospacedSystemFont(ofSize: 13, weight: .regular)
                configuration.attributedTitle = attributed
            }
            let button = UIButton(configuration: configuration)
            button.accessibilityLabel = label
            allButtons.append(button)
            recolor(button)
            return button
        }

        /// 按下去的样子（粘住、按住的修饰键，开着的键盘）：反色，和 app 里的实心按钮一样黑白为主。不用
        /// `tintColor`：跟在软键盘上面时这条栏在键盘的窗口里，接不到 app 设的强调色，会是系统的蓝色。
        public func highlight(_ button: UIButton, _ on: Bool) {
            button.isSelected = on
            recolor(button)
        }

        /// 按 `palette` 和按钮按没按下去上色；没有配色时用系统色、不描边。
        private func recolor(_ button: UIButton) {
            guard var configuration = button.configuration else { return }
            let on = button.isSelected
            if let palette {
                configuration.baseBackgroundColor = on ? palette.selectedKey : palette.key
                configuration.baseForegroundColor = on ? palette.selectedLabel : palette.label
                configuration.background.strokeColor = on ? palette.selectedKey : palette.border
                configuration.background.strokeWidth = 1
            } else {
                configuration.baseBackgroundColor = on ? .label : .secondarySystemFill
                configuration.baseForegroundColor = on ? .systemBackground : .label
                configuration.background.strokeColor = nil
                configuration.background.strokeWidth = 0
            }
            button.configuration = configuration
        }

        public var enableInputClicksWhenVisible: Bool { true }
    }

    /// 排完版告诉外面一声：内容宽度要等它自己排版时才按约束算出来。
    private final class LayoutReportingScrollView: UIScrollView {
        var didLayout: (() -> Void)?

        override func layoutSubviews() {
            super.layoutSubviews()
            didLayout?()
        }
    }
#endif
