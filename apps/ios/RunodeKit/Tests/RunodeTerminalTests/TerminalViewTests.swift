#if os(iOS)
    import RunodeProtocol
    import Testing
    import UIKit
    import UniformTypeIdentifiers

    @testable import RunodeTerminal

    /// 软键盘弹出、收起时终端视图向上层报「适配手机」尺寸的次数。
    @MainActor @Suite struct TerminalViewTests {
        @MainActor final class Recorder: TerminalViewDelegate {
            var fitSizes: [GridSize] = []
            var inputs = 0
            var pasted: [TerminalInput] = []
            var images: [Data] = []

            func terminalView(_ view: TerminalView, didInput input: TerminalInput) {
                inputs += 1
                if case .paste = input { pasted.append(input) }
            }
            func terminalView(_ view: TerminalView, fitSizeDidChange size: GridSize) { fitSizes.append(size) }
            func terminalView(_ view: TerminalView, didScrollBack scrolledBack: Bool) {}
            func terminalView(_ view: TerminalView, keyboardVisible visible: Bool) {}
            func terminalView(_ view: TerminalView, dictation press: DictationPress) {}
            func terminalViewRequestsMedia(_ view: TerminalView) {}
            func terminalView(_ view: TerminalView, didPasteImage data: Data) { images.append(data) }
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

        /// 输入法设的选区是照更长的组字文字算的：按现在的长度夹住，退格只删得到还在的字，不越界。
        @Test func aStaleSelectionIsClampedToTheMarkedText() {
            let view = TerminalView(frame: CGRect(x: 0, y: 0, width: 402, height: 680))
            view.setMarkedText("ab", selectedRange: NSRange(location: 2, length: 0))
            view.selectedTextRange = TerminalTextRange(1, 5)
            #expect(view.markedSelection == NSRange(location: 1, length: 1))
            view.deleteBackward()
            #expect(view.markedText == "a")
            #expect(view.markedSelection == NSRange(location: 1, length: 0))
        }

        /// 组字到一半换了会话：组字中的文字丢掉，之后输入法再交出来也不发给新的会话。
        @Test func switchingSessionsDiscardsTheComposition() {
            let view = TerminalView(frame: CGRect(x: 0, y: 0, width: 402, height: 680))
            let recorder = Recorder()
            view.delegate = recorder
            view.setMarkedText("ni", selectedRange: NSRange(location: 2, length: 0))
            view.terminalDidReset(nil, settings: .default)
            #expect(view.markedText.isEmpty)
            #expect(view.markedTextRange == nil)
            view.unmarkText()
            #expect(recorder.inputs == 0)
        }

        // MARK: 选区

        /// 字号定死的视图挂着一份写了 `text` 的真 VT。
        func selectionView(_ text: String) throws -> (TerminalView, VTerminal, Recorder) {
            let view = TerminalView(frame: CGRect(x: 0, y: 0, width: 402, height: 680))
            view.fontSizeOverride = 13
            let recorder = Recorder()
            view.delegate = recorder
            let terminal = try VTerminal(
                size: GridSize(cols: 30, rows: 8, cellWidthPx: 16, cellHeightPx: 32), settings: .default)
            terminal.feed(Array(text.utf8))
            view.terminalDidReset(terminal, settings: .default)
            view.layoutIfNeeded()
            return (view, terminal, recorder)
        }

        /// 第 `row` 行第 `column` 列格子里 x 方向 `fraction` 处的点（网格坐标）。
        func point(_ column: Int, _ row: Int, fraction: CGFloat = 0.5) -> CGPoint {
            let font = TerminalFont(size: 13)
            return CGPoint(x: (CGFloat(column) + fraction) * font.cellWidth, y: (CGFloat(row) + 0.5) * font.cellHeight)
        }

        /// 长按选中一个词，打一个字就退出选中、清掉 VT 里的选区。
        @Test func typingEndsTheSelection() throws {
            let (view, terminal, recorder) = try selectionView("hello world")
            #expect(view.beginSelection(at: point(7, 0)))
            #expect(view.selecting)
            #expect(terminal.selectionText() == "world")
            view.emit(.text("x"))
            #expect(!view.selecting)
            #expect(!terminal.hasSelection)
            #expect(recorder.inputs == 1)
        }

        /// 长按在空白上不进入选中。
        @Test func pressingOnBlanksSelectsNothing() throws {
            let (view, terminal, _) = try selectionView("hello     world")
            #expect(!view.beginSelection(at: point(7, 0)))
            #expect(!view.beginSelection(at: point(3, 5)))
            #expect(!view.selecting)
            #expect(!terminal.hasSelection)
        }

        /// 选中时再长按空白格或网格外：退出选中、清掉原来的选区，不在旧选区上接着选。
        @Test func pressingOnBlanksWhileSelectingEndsTheSelection() throws {
            let (view, terminal, _) = try selectionView("hello world")
            #expect(view.beginSelection(at: point(1, 0)))
            #expect(!view.beginSelection(at: point(20, 5)))
            #expect(!view.selecting)
            #expect(!terminal.hasSelection)
            #expect(view.beginSelection(at: point(1, 0)))
            #expect(!view.beginSelection(at: CGPoint(x: -5, y: -5)))
            #expect(!view.selecting)
            #expect(!terminal.hasSelection)
        }

        /// 拖终点柄把选区拉长，拷贝写进剪贴板后退出选中。
        @Test func copyWritesTheDraggedSelectionAndEnds() throws {
            let (view, terminal, _) = try selectionView("hello world")
            let pasteboard = try #require(UIPasteboard(name: UIPasteboard.Name("runode-test-\(UUID())"), create: true))
            defer { UIPasteboard.remove(withName: pasteboard.name) }
            view.pasteboard = pasteboard
            #expect(view.beginSelection(at: point(1, 0)))
            #expect(terminal.selectionText() == "hello")
            view.dragSelection(to: point(10, 0, fraction: 0.9))
            #expect(terminal.selectionText() == "hello world")
            view.copy(nil)
            #expect(pasteboard.string == "hello world")
            #expect(!view.selecting)
            #expect(!terminal.hasSelection)
        }

        /// 剪贴板上只有图片时粘贴交出图片的原始数据，有文字时照旧粘贴文字。
        @Test func pastingAnImageHandsItToTheDelegate() throws {
            let (view, _, recorder) = try selectionView("hello")
            let pasteboard = try #require(UIPasteboard(name: UIPasteboard.Name("runode-test-\(UUID())"), create: true))
            defer { UIPasteboard.remove(withName: pasteboard.name) }
            view.pasteboard = pasteboard
            let png = try #require(
                UIGraphicsImageRenderer(size: CGSize(width: 4, height: 4)).image { context in
                    UIColor.red.setFill()
                    context.fill(CGRect(x: 0, y: 0, width: 4, height: 4))
                }.pngData())
            pasteboard.setData(png, forPasteboardType: UTType.png.identifier)
            #expect(view.canPerformAction(#selector(UIResponderStandardEditActions.paste(_:)), withSender: nil))
            view.paste(nil)
            #expect(recorder.images == [png])
            #expect(recorder.inputs == 0)

            pasteboard.string = "ls"
            view.paste(nil)
            #expect(recorder.images.count == 1)
            #expect(recorder.pasted.count == 1)
        }

        /// 选中时轻点选区外只退出选中：程序开着鼠标上报也不发点击，不弹键盘；点在选区里还在选中。
        @Test func tappingOutsideTheSelectionOnlyEndsIt() throws {
            let (view, terminal, recorder) = try selectionView("\u{1b}[?1000hhello world")
            #expect(terminal.mouseTracking)
            #expect(view.beginSelection(at: point(1, 0)))
            // 选区落进视图的帧要一次刷新；`scrollToBottom` 当场刷新。
            view.scrollToBottom()
            view.handleTap(at: point(2, 0))
            #expect(view.selecting)
            view.handleTap(at: point(4, 5))
            #expect(!view.selecting)
            #expect(!terminal.hasSelection)
            #expect(recorder.inputs == 0)
            #expect(!view.isFirstResponder)
        }

        /// 换一份 VT 就退出选中，原来那份的选区也清掉。
        @Test func resettingTheTerminalEndsTheSelection() throws {
            let (view, terminal, _) = try selectionView("hello world")
            #expect(view.beginSelection(at: point(1, 0)))
            view.terminalDidReset(nil, settings: .default)
            #expect(!view.selecting)
            #expect(!terminal.hasSelection)
        }
    }
#endif
