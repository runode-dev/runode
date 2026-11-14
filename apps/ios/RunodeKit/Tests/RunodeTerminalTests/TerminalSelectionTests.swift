import CoreGraphics
import RunodeProtocol
import Testing

@testable import RunodeTerminal

/// 选区拖柄落在哪格、画在哪、按住的是哪个，自动滚动的速度，以及选区的配色。
@Suite struct TerminalSelectionTests {
    typealias Geometry = SelectionGeometry

    func cell(_ end: SelectionEnd, x: CGFloat, y: CGFloat = 5) -> GridCell {
        Geometry.handleCell(end, x: x, y: y, cellWidth: 10, cellHeight: 20, columns: 5, rows: 3)
    }

    /// 手指 x 取整到最近的格子边界：起点取边界右边那格，终点取左边那格。
    @Test func handlesRoundToTheNearestCellBoundary() {
        #expect(cell(.start, x: 14) == GridCell(column: 1, row: 0))
        #expect(cell(.end, x: 14) == GridCell(column: 0, row: 0))
        #expect(cell(.start, x: 16) == GridCell(column: 2, row: 0))
        #expect(cell(.end, x: 16) == GridCell(column: 1, row: 0))
        #expect(cell(.end, x: 30, y: 45) == GridCell(column: 2, row: 2))
    }

    @Test func handleCellsAreClampedToTheGrid() {
        #expect(cell(.start, x: -50, y: -5) == GridCell(column: 0, row: 0))
        #expect(cell(.end, x: -50) == GridCell(column: 0, row: 0))
        #expect(cell(.start, x: 1000, y: 1000) == GridCell(column: 4, row: 2))
        #expect(cell(.end, x: 1000) == GridCell(column: 4, row: 0))
    }

    /// 宽字符：起点柄落在后一格时退到前一格的左边，终点柄在宽字符整个两格的右边。
    @Test func handleAnchorsCoverWideCharacters() {
        var wide = ScreenCell.blank
        wide.text = "中"
        wide.width = .wide
        var tail = ScreenCell.blank
        tail.width = .spacerTail
        let cells = [[ScreenCell.blank], [wide, tail, ScreenCell.blank]]
        func anchor(_ end: SelectionEnd, _ column: Int) -> Geometry.HandleAnchor {
            Geometry.handleAnchor(
                end, at: GridCell(column: column, row: 1), in: cells, cellWidth: 10, cellHeight: 20)
        }
        #expect(anchor(.start, 1) == Geometry.HandleAnchor(x: 0, top: 20, bottom: 40))
        #expect(anchor(.start, 0).x == 0)
        #expect(anchor(.end, 0).x == 20)
        #expect(anchor(.end, 1).x == 20)
        #expect(anchor(.start, 2).x == 20)
        #expect(anchor(.end, 2).x == 30)
    }

    /// 两个拖柄按住的范围叠在一起时取近的那个，一样近取终点柄；都不够近时谁也不是。
    @Test func overlappingHandlesPickTheNearestOne() {
        let start = CGPoint(x: 0, y: 0)
        let end = CGPoint(x: 10, y: 0)
        #expect(Geometry.hitHandle(CGPoint(x: 2, y: 0), start: start, end: end, radius: 22) == .start)
        #expect(Geometry.hitHandle(CGPoint(x: 8, y: 0), start: start, end: end, radius: 22) == .end)
        #expect(Geometry.hitHandle(CGPoint(x: 5, y: 0), start: start, end: end, radius: 22) == .end)
        #expect(Geometry.hitHandle(CGPoint(x: 100, y: 0), start: start, end: end, radius: 22) == nil)
        #expect(Geometry.hitHandle(CGPoint(x: 2, y: 0), start: nil, end: end, radius: 22) == .end)
    }

    /// 可见区域里不滚；往上越出看更早的内容（正），往下越出为负；越远越快，有上限。
    @Test func autoscrollSpeedsUpBeyondTheEdgeUpToALimit() {
        func rate(_ y: CGFloat) -> Double {
            Geometry.autoscrollRate(y: y, top: 100, bottom: 300, cellHeight: 20)
        }
        #expect(rate(200) == 0)
        #expect(rate(100) == 0)
        #expect(rate(90) > 0)
        #expect(rate(50) > rate(90))
        #expect(rate(310) < 0)
        #expect(rate(-10_000) == Geometry.edgeRateLimit)
        #expect(rate(10_000) == -Geometry.edgeRateLimit)
        #expect(Geometry.edgeRate(-5, low: 0, high: 50, cell: 10) < 0)
        #expect(Geometry.edgeRate(60, low: 0, high: 50, cell: 10) > 0)
    }

    @Test func selectionColorsFollowTheSettings() {
        let foreground = Rgb(200, 200, 200)
        let background = Rgb(10, 20, 30)
        var settings = TermSettings.default
        settings.selectionBackground = nil
        settings.selectionForeground = nil
        func colors(screen: Rgb = Rgb(0, 0, 0)) -> (foreground: Rgb, background: Rgb) {
            Geometry.selectionColors(
                foreground: foreground, background: background, settings: settings, screenBackground: screen)
        }
        // 都没配：底色按终端背景深浅取，字色不变。
        #expect(colors() == (foreground, Geometry.selectionOnDark))
        #expect(colors(screen: Rgb(250, 250, 250)) == (foreground, Geometry.selectionOnLight))
        // 只配了底色：字色取格子原来的背景色。
        settings.selectionBackground = .rgb(Rgb(1, 2, 3))
        #expect(colors() == (background, Rgb(1, 2, 3)))
        // 都配了：照配的解析，跟随格子颜色的取格子原来的颜色。
        settings.selectionForeground = .cellBackground
        settings.selectionBackground = .cellForeground
        #expect(colors() == (background, foreground))
        // 只配了字色：底色仍取统一的。
        settings.selectionBackground = nil
        settings.selectionForeground = .rgb(Rgb(9, 9, 9))
        #expect(colors() == (Rgb(9, 9, 9), Geometry.selectionOnDark))
    }
}
