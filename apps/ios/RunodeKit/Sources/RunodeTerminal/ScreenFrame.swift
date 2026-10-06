import RunodeProtocol

// 画一屏要的全部数据，从 libghostty 的 render state 抄出来的纯 Swift 值。画的一方（现在是
// CoreText 的 `TerminalGridView`，以后可以换成 Metal）只读这些，不碰 libghostty。

/// 单元格占几列。
public enum CellWidth: Sendable, Hashable {
    case narrow
    /// 宽字符的头一格，画的时候占两列。
    case wide
    /// 宽字符的后一格，不画。
    case spacerTail
    /// 行末放不下宽字符时的占位，不画。
    case spacerHead
}

/// 下划线的样式，对应 SGR 4:x。
public enum UnderlineStyle: UInt8, Sendable, Hashable {
    case none = 0, single, double, curly, dotted, dashed
}

/// 单元格的文字属性。
public struct CellAttributes: OptionSet, Sendable, Hashable {
    public let rawValue: UInt8
    public init(rawValue: UInt8) { self.rawValue = rawValue }

    public static let bold = CellAttributes(rawValue: 1 << 0)
    public static let italic = CellAttributes(rawValue: 1 << 1)
    public static let faint = CellAttributes(rawValue: 1 << 2)
    public static let inverse = CellAttributes(rawValue: 1 << 3)
    public static let invisible = CellAttributes(rawValue: 1 << 4)
    public static let strikethrough = CellAttributes(rawValue: 1 << 5)
    public static let blink = CellAttributes(rawValue: 1 << 6)
}

/// 一个单元格。
public struct ScreenCell: Sendable, Hashable {
    /// 整个字素簇；空格子为空串。
    public var text: String
    /// 解析过调色板的前景色；`nil` 用默认前景。
    public var foreground: Rgb?
    /// 解析过调色板的背景色；`nil` 用默认背景。
    public var background: Rgb?
    public var attributes: CellAttributes
    public var underline: UnderlineStyle
    public var width: CellWidth

    public static let blank = ScreenCell(
        text: "", foreground: nil, background: nil, attributes: [], underline: .none, width: .narrow)

    public init(
        text: String, foreground: Rgb?, background: Rgb?, attributes: CellAttributes, underline: UnderlineStyle,
        width: CellWidth
    ) {
        self.text = text
        self.foreground = foreground
        self.background = background
        self.attributes = attributes
        self.underline = underline
        self.width = width
    }
}

/// 光标的形状。
public enum CursorShape: Sendable, Hashable {
    case block, blockHollow, bar, underline
}

/// 视口里的光标。
public struct ScreenCursor: Sendable, Hashable {
    public var column: Int
    public var row: Int
    public var shape: CursorShape
    public var blinking: Bool
    /// 光标在宽字符的后一格上。
    public var onWideTail: Bool
}

/// 一屏的内容。
public struct ScreenFrame: Sendable {
    public var columns: Int
    public var rows: Int
    /// 一行一个数组，每个数组 `columns` 个格子。
    public var cells: [[ScreenCell]]
    /// 视口上面那一行，平滑滚动错开时从顶上露出一部分；视口在历史最顶上时为空。
    public var above: [ScreenCell]
    /// 平滑滚动时整屏往下错开几分之一行，在 [0, 1) 里；`above` 为空时总是 0。
    public var scrollOffset: Double
    public var background: Rgb
    public var foreground: Rgb
    /// 程序或主题明确设了的光标色。
    public var cursorColor: Rgb?
    /// 光标可见且在视口里时才有。
    public var cursor: ScreenCursor?

    public init() {
        columns = 0
        rows = 0
        cells = []
        above = []
        scrollOffset = 0
        background = TermSettings.default.background
        foreground = TermSettings.default.foreground
        cursorColor = nil
        cursor = nil
    }

    /// 屏幕上的文字，一行一个，行尾空白去掉；宽字符的占位格不算。
    public var lines: [String] {
        cells.map(Self.text(of:))
    }

    /// 一行的文字，行尾空白去掉；宽字符的占位格不算。
    static func text(of row: [ScreenCell]) -> String {
        var line = ""
        for cell in row where cell.width != .spacerTail && cell.width != .spacerHead {
            line += cell.text.isEmpty ? " " : cell.text
        }
        while line.last == " " { line.removeLast() }
        return line
    }
}

/// 一次刷新改了哪些：整屏，还是某几行。
public struct FrameChange: Sendable, Hashable {
    public var full: Bool
    public var rows: [Int]
    /// 光标变了（位置、形状、可见）。
    public var cursorChanged: Bool
    /// 视口上面那一行（`ScreenFrame.above`）变了。
    public var aboveChanged: Bool

    public var isEmpty: Bool { !full && rows.isEmpty && !cursorChanged && !aboveChanged }

    public static let none = FrameChange(full: false, rows: [], cursorChanged: false, aboveChanged: false)
}
