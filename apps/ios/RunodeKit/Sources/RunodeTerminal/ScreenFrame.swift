import RunodeProtocol

// 画一屏要的全部数据，从 libghostty 的 render state 抄出来的纯 Swift 值。画的一方（`TerminalGridView`）
// 只读这些，不碰 libghostty。

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

public enum CursorShape: Sendable, Hashable {
    case block, blockHollow, bar, underline
}

public struct ScreenCursor: Sendable, Hashable {
    public var column: Int
    public var row: Int
    public var shape: CursorShape
    public var blinking: Bool
    /// 光标在宽字符的后一格上。
    public var onWideTail: Bool
}

/// 图片画在哪一层，按画的先后排。
public enum ImageLayer: Int, Sendable, Hashable, Comparable {
    case belowBackground, belowText, aboveText

    public static func < (a: Self, b: Self) -> Bool { a.rawValue < b.rawValue }
}

/// 程序用 Kitty 图形协议放在屏幕上的一块图片。位置和大小以单元格计，可以有小数；左上角相对视口，
/// 图片上半截滚出了视口时为负。像素不在这里，画的一方按 `id` 和 `generation` 向 `VTerminal.image`
/// 要，同一个 `generation` 的像素不会变，可以直接拿来当缓存键。
public struct ScreenImage: Sendable, Hashable {
    public var id: UInt32
    public var generation: UInt64
    public var x: Double
    public var y: Double
    public var width: Double
    public var height: Double
    /// 只画图片的这一块（以图片像素计），缩放到上面的大小。
    public var sourceX: UInt32
    public var sourceY: UInt32
    public var sourceWidth: UInt32
    public var sourceHeight: UInt32
    public var layer: ImageLayer
}

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
    /// 屏幕上的图片，按画的先后排好（先按层，同层按 z 值）。
    public var images: [ScreenImage]
    /// 图片动画下一帧离现在多久该画；没有在播的动画时为空。
    public var nextImageFrame: Duration?

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
        images = []
        nextImageFrame = nil
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
    /// 图片（`ScreenFrame.images`）变了：放了、删了、挪了，或者动画换了一帧。
    public var imagesChanged: Bool

    public var isEmpty: Bool { !full && rows.isEmpty && !cursorChanged && !aboveChanged && !imagesChanged }

    public static let none = FrameChange(
        full: false, rows: [], cursorChanged: false, aboveChanged: false, imagesChanged: false)
}
