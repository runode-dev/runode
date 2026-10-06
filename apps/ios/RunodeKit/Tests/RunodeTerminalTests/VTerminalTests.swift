import RunodeProtocol
import Testing

@testable import RunodeTerminal

/// 喂字节后读出的屏幕、改尺寸、换主题和输入编码，都经真的 libghostty-vt。
@Suite struct VTerminalTests {
    let size = GridSize(cols: 20, rows: 5, cellWidthPx: 8, cellHeightPx: 16)

    func terminal() throws -> VTerminal {
        try VTerminal(size: size, settings: .default)
    }

    @Test func feedingBytesShowsText() throws {
        let vt = try terminal()
        vt.feed(Array("hello\r\n中文 ok".utf8))
        let lines = vt.screenLines()
        #expect(lines.count == 5)
        #expect(lines[0] == "hello")
        #expect(lines[1] == "中文 ok")
    }

    @Test func wideCharactersTakeTwoCells() throws {
        let vt = try terminal()
        vt.feed(Array("中a".utf8))
        var frame = ScreenFrame()
        _ = vt.refresh(&frame)
        #expect(frame.cells[0][0].text == "中")
        #expect(frame.cells[0][0].width == .wide)
        #expect(frame.cells[0][1].width == .spacerTail)
        #expect(frame.cells[0][2].text == "a")
    }

    @Test func stylesAndColorsAreRead() throws {
        let vt = try terminal()
        vt.feed(Array("\u{1b}[1;3;4;31;44mX\u{1b}[0m\u{1b}[7mY".utf8))
        var frame = ScreenFrame()
        _ = vt.refresh(&frame)
        let x = frame.cells[0][0]
        #expect(x.attributes.contains(.bold))
        #expect(x.attributes.contains(.italic))
        #expect(x.underline == .single)
        // 调色板 1 和 4 是默认主题里的红、蓝。
        #expect(x.foreground == Rgb(hex: 0xFF1261))
        #expect(x.background == Rgb(hex: 0x2D9AFF))
        #expect(frame.cells[0][1].attributes.contains(.inverse))
        #expect(frame.background == TermSettings.default.background)
    }

    @Test func onlyChangedRowsAreReported() throws {
        let vt = try terminal()
        var frame = ScreenFrame()
        #expect(vt.refresh(&frame).full)
        vt.feed(Array("\u{1b}[3;1Hthird".utf8))
        let change = vt.refresh(&frame)
        #expect(!change.full)
        #expect(change.rows.contains(2))
        // 光标原来所在的第一行也会标脏，最后一行没动过。
        #expect(!change.rows.contains(4))
        #expect(frame.lines[2] == "third")
    }

    @Test func cursorFollowsOutput() throws {
        let vt = try terminal()
        vt.feed(Array("ab".utf8))
        var frame = ScreenFrame()
        _ = vt.refresh(&frame)
        #expect(frame.cursor?.column == 2)
        #expect(frame.cursor?.row == 0)
        vt.feed(Array("\u{1b}[?25l".utf8))
        _ = vt.refresh(&frame)
        #expect(frame.cursor == nil)
    }

    @Test func resizeChangesTheGrid() throws {
        let vt = try terminal()
        vt.resize(GridSize(cols: 30, rows: 8, cellWidthPx: 8, cellHeightPx: 16))
        var frame = ScreenFrame()
        _ = vt.refresh(&frame)
        #expect(frame.columns == 30)
        #expect(frame.rows == 8)
    }

    @Test func themeChangesDefaultColors() throws {
        let vt = try terminal()
        var settings = TermSettings.default
        settings.background = Rgb(1, 2, 3)
        settings.palette = [.init(index: 1, color: Rgb(9, 9, 9))]
        vt.applyTheme(settings)
        vt.feed(Array("\u{1b}[31mR\u{1b}[32mG".utf8))
        var frame = ScreenFrame()
        _ = vt.refresh(&frame)
        #expect(frame.background == Rgb(1, 2, 3))
        #expect(frame.cells[0][0].foreground == Rgb(9, 9, 9))
        // 换主题前先把调色板重置回内置的：没在新主题里的 2 号色不是旧主题的绿。
        #expect(frame.cells[0][1].foreground != Rgb(hex: 0x2AD947))
    }

    @Test func scrollbackKeepsHistory() throws {
        let vt = try terminal()
        for line in 1...20 {
            vt.feed(Array("line \(line)\r\n".utf8))
        }
        #expect(vt.scrollbar.total > 5)
        #expect(vt.scrollbar.atBottom)
        vt.scroll(by: -3)
        #expect(!vt.scrollbar.atBottom)
        // 屏幕最后是 17 到 20 行和光标所在的空行，往上滚三行后顶上是第 14 行。
        #expect(vt.screenLines().first == "line 14")
        vt.scrollToBottom()
        #expect(vt.scrollbar.atBottom)
    }

    @Test func keysFollowTerminalModes() throws {
        let vt = try terminal()
        #expect(vt.encode(KeyInput(key: .up)) == Array("\u{1b}[A".utf8))
        // 应用光标键模式（DECCKM）下方向键换成 SS3。
        vt.feed(Array("\u{1b}[?1h".utf8))
        #expect(vt.encode(KeyInput(key: .up)) == Array("\u{1b}OA".utf8))
        #expect(vt.encode(KeyInput(key: .escape)) == [0x1B])
        #expect(vt.encode(KeyInput(key: .enter)) == [0x0D])
        #expect(vt.encode(KeyInput(key: .tab)) == [0x09])
        #expect(vt.encode(KeyInput(key: .backspace)) == [0x7F])
    }

    @Test func controlCharacters() throws {
        let vt = try terminal()
        let ctrlC = try #require(KeyInput.typing("c", modifiers: .control))
        #expect(vt.encode(ctrlC) == [0x03])
        let plain = try #require(KeyInput.typing("|"))
        #expect(vt.encode(plain) == Array("|".utf8))
    }

    @Test func pasteIsBracketedWhenAsked() throws {
        let vt = try terminal()
        #expect(vt.encodePaste("a\nb") == Array("a\rb".utf8))
        vt.feed(Array("\u{1b}[?2004h".utf8))
        #expect(vt.bracketedPaste)
        #expect(vt.encodePaste("a\nb") == Array("\u{1b}[200~a\nb\u{1b}[201~".utf8))
    }

    @Test func titleComesFromOsc() throws {
        let vt = try terminal()
        vt.feed(Array("\u{1b}]2;构建\u{07}".utf8))
        #expect(vt.title == "构建")
    }
}
