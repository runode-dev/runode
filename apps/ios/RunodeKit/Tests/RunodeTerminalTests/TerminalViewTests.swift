#if os(iOS)
    import RunodeProtocol
    import Testing
    import UIKit

    @testable import RunodeTerminal

    /// 软键盘弹出、收起时终端视图向上层报「适配手机」尺寸的次数。
    @MainActor @Suite struct TerminalViewTests {
        @MainActor final class Recorder: TerminalViewDelegate {
            var fitSizes: [GridSize] = []

            func terminalView(_ view: TerminalView, didInput input: TerminalInput) {}
            func terminalView(_ view: TerminalView, fitSizeDidChange size: GridSize) { fitSizes.append(size) }
            func terminalView(_ view: TerminalView, didScrollBack scrolledBack: Bool) {}
            func terminalView(_ view: TerminalView, keyboardVisible visible: Bool) {}
        }

        /// 键盘动的时候视图的高度一下变好几次，只在停稳以后按最后的大小报一次。
        @Test func keyboardMotionReportsTheFitSizeOnceItSettles() {
            let view = TerminalView(frame: CGRect(x: 0, y: 0, width: 402, height: 680))
            let recorder = Recorder()
            view.delegate = recorder
            view.layoutIfNeeded()
            #expect(recorder.fitSizes.count == 1)

            let center = NotificationCenter.default
            center.post(name: UIResponder.keyboardWillShowNotification, object: nil)
            for height: CGFloat in [724, 457, 413] {
                view.frame.size.height = height
                view.layoutIfNeeded()
            }
            #expect(recorder.fitSizes.count == 1)
            center.post(name: UIResponder.keyboardDidShowNotification, object: nil)
            view.layoutIfNeeded()
            #expect(recorder.fitSizes.count == 2)
            #expect(recorder.fitSizes.last == view.fitSize)
        }
    }
#endif
