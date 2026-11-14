#if os(iOS)
    import UIKit

    /// 键盘上方的一条辅助栏：软键盘上没有的 Esc、粘住的 Ctrl、Tab、方向键，回车（软键盘收着时也按得到）
    /// 和几个常用符号，横着能拖。软键盘收着时界面底部放一条一样的键、更高一点的栏（`resting` 为真，由
    /// `TerminalView.makeRestingKeyBar` 建）。语音输入的麦克风键和开关软键盘的键固定在右端，不跟着别的键
    /// 滚走；麦克风键按下、松开都报给代理，录音时显示成按下去的样子。开关软键盘的键两条都用键盘图标：
    /// 底部那条是「打开键盘」，跟着软键盘的那条是「收起键盘」，按下去的样子表示键盘开着。
    final class KeyboardAccessoryBar: KeyBar {
        private weak var owner: TerminalView?
        private var controlButton: UIButton?
        private var micButton: UIButton?

        init(owner: TerminalView, resting: Bool = false) {
            self.owner = owner
            if resting {
                super.init(resting: true, height: 52, keyInset: 4)
            } else {
                super.init(resting: false)
            }
            let keyboard =
                resting
                ? button("keyboard", symbol: true, label: String(localized: "打开键盘")) { $0.showKeyboard() }
                : button("keyboard", symbol: true, label: String(localized: "收起键盘")) { $0.resignFirstResponder() }
            if !resting { highlight(keyboard, true) }
            let mic = microphone()
            micButton = mic
            pinTrailing([mic, keyboard])

            // 修饰键和 Tab 写终端里的原文，不用 SF Symbol：`escape` 画成带箭头的圆圈，看着像撤销。
            keys.addArrangedSubview(button("esc", label: "Esc") { $0.press(.escape) })
            let control = button("ctrl", label: String(localized: "Ctrl（粘住）")) { $0.controlLatched.toggle() }
            controlButton = control
            keys.addArrangedSubview(control)
            keys.addArrangedSubview(button("tab", label: "Tab") { $0.press(.tab) })
            keys.addArrangedSubview(button("chevron.left", symbol: true, label: String(localized: "左")) { $0.press(.left) })
            keys.addArrangedSubview(button("chevron.down", symbol: true, label: String(localized: "下")) { $0.press(.down) })
            keys.addArrangedSubview(button("chevron.up", symbol: true, label: String(localized: "上")) { $0.press(.up) })
            keys.addArrangedSubview(button("chevron.right", symbol: true, label: String(localized: "右")) { $0.press(.right) })
            keys.addArrangedSubview(button("return", symbol: true, label: String(localized: "回车")) { $0.press(.enter) })
            for symbol in ["|", "~", "/", "-", "_", "`"] {
                keys.addArrangedSubview(button(symbol, label: symbol) { $0.typeText(symbol) })
            }
            keys.addArrangedSubview(button("doc.on.clipboard", symbol: true, label: String(localized: "粘贴")) { $0.paste(nil) })
        }

        func setControlLatched(_ latched: Bool) {
            guard let controlButton else { return }
            highlight(controlButton, latched)
        }

        func setDictating(_ dictating: Bool) {
            guard let micButton else { return }
            highlight(micButton, dictating)
            micButton.accessibilityValue = dictating ? String(localized: "正在录音") : nil
        }

        /// 麦克风键：按下、松开要分开报，按住说话和点一下开始都由视图模型判断，所以不用 `primaryAction`。
        private func microphone() -> UIButton {
            let mic = styled("mic", symbol: true, label: String(localized: "语音输入"))
            mic.accessibilityHint = String(localized: "按住说话，松开后把文字打进终端；也可以点一下开始、再点一下结束")
            let report: (DictationPress) -> UIAction = { [weak self] press in
                UIAction { _ in
                    guard let owner = self?.owner else { return }
                    owner.delegate?.terminalView(owner, dictation: press)
                }
            }
            mic.addAction(report(.down), for: .touchDown)
            mic.addAction(report(.up), for: .touchUpInside)
            mic.addAction(report(.cancel), for: [.touchUpOutside, .touchCancel])
            return mic
        }

        private func button(
            _ title: String, symbol: Bool = false, label: String, action: @escaping @MainActor (TerminalView) -> Void
        ) -> UIButton {
            key(title, symbol: symbol, label: label) { [weak self] in
                guard let owner = self?.owner else { return }
                action(owner)
            }
        }
    }
#endif
