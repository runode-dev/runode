import CoreGraphics
import RunodeProtocol

#if os(iOS)
    import UIKit
#endif

/// 选区拖柄和自动滚动的几何、选区的配色：不碰界面的纯计算，坐标都在网格自己的坐标系里（点）。
enum SelectionGeometry {
    /// 拖柄的竖条：`x` 是它贴着的格子边，`top`、`bottom` 是那一行的上下沿。
    struct HandleAnchor: Equatable {
        var x: CGFloat
        var top: CGFloat
        var bottom: CGFloat
    }

    /// 手指越出可见区域时自动滚动、平移的速度（格每秒）：刚越出去时的速度、每多越出一格加的速度和上限。
    static let edgeRateBase: Double = 6
    static let edgeRatePerCell: Double = 8
    static let edgeRateLimit: Double = 40
    /// 可见区域贴着视图边时，往里留出的这么宽（点）的一条也算越出：手指出不了屏幕和旁边的按键栏。
    static let edgeBand: CGFloat = 24

    /// 没配选区底色时的统一底色，按终端背景深浅取。
    static let selectionOnDark = Rgb(0x26, 0x4F, 0x78)
    static let selectionOnLight = Rgb(0xAD, 0xD6, 0xFF)

    /// 拖柄拖到 (`x`, `y`) 时该落在哪一格：`x` 四舍五入到最近的格子边界，起点取边界右边那格、终点取
    /// 左边那格；行取手指所在的那行。行列都夹在网格里。
    static func handleCell(
        _ end: SelectionEnd, x: CGFloat, y: CGFloat, cellWidth: CGFloat, cellHeight: CGFloat, columns: Int,
        rows: Int
    ) -> GridCell {
        let boundary = cellWidth > 0 ? Int((x / cellWidth).rounded()) : 0
        let column = end == .start ? boundary : boundary - 1
        let row = cellHeight > 0 ? Int((y / cellHeight).rounded(.down)) : 0
        return GridCell(
            column: min(max(column, 0), max(columns - 1, 0)), row: min(max(row, 0), max(rows - 1, 0)))
    }

    /// 拖柄画在哪：起点柄贴着起点格的左边（落在宽字符后一格时退到前一格），终点柄贴着终点格的右边
    /// （宽字符算两格）。
    static func handleAnchor(
        _ end: SelectionEnd, at cell: GridCell, in cells: [[ScreenCell]], cellWidth: CGFloat, cellHeight: CGFloat
    ) -> HandleAnchor {
        let width = cellAt(cells, cell)?.width
        let edge: Int =
            switch (end, width) {
            case (.start, .spacerTail): max(cell.column - 1, 0)
            case (.start, _): cell.column
            case (.end, .wide): cell.column + 2
            case (.end, _): cell.column + 1
            }
        return HandleAnchor(
            x: CGFloat(edge) * cellWidth, top: CGFloat(cell.row) * cellHeight,
            bottom: CGFloat(cell.row + 1) * cellHeight)
    }

    /// 按在 `point` 上抓的是哪个拖柄：`radius` 以内离得最近的那个，一样近时取终点柄。不显示的拖柄传空。
    static func hitHandle(_ point: CGPoint, start: CGPoint?, end: CGPoint?, radius: CGFloat) -> SelectionEnd? {
        func distance(_ center: CGPoint?) -> CGFloat? {
            guard let center else { return nil }
            let d = hypot(point.x - center.x, point.y - center.y)
            return d <= radius ? d : nil
        }
        switch (distance(start), distance(end)) {
        case (let s?, let e?): return s < e ? .start : .end
        case (_?, nil): return .start
        case (nil, _?): return .end
        case (nil, nil): return nil
        }
    }

    /// `value` 越出 [`low`, `high`] 时该往外挪的速度（格每秒，`cell` 是一格的长度）：越出去越快，有上限；
    /// 低于 `low` 为负，高于 `high` 为正，区间里为 0。
    static func edgeRate(_ value: CGFloat, low: CGFloat, high: CGFloat, cell: CGFloat) -> Double {
        let beyond: CGFloat
        if value < low {
            beyond = value - low
        } else if value > high {
            beyond = value - high
        } else {
            return 0
        }
        let cells = Double(abs(beyond) / max(cell, 1))
        let rate = min(edgeRateBase + cells * edgeRatePerCell, edgeRateLimit)
        return beyond < 0 ? -rate : rate
    }

    /// 拖柄拖到可见区域上下边以外时滚回滚历史的速度（行每秒）：往上越出为正（看更早的内容），往下为负。
    static func autoscrollRate(y: CGFloat, top: CGFloat, bottom: CGFloat, cellHeight: CGFloat) -> Double {
        -edgeRate(y, low: top, high: bottom, cell: cellHeight)
    }

    /// 选中格子的前景、背景色。`foreground`、`background` 是格子原来实际的颜色（已按反显对调）：没配
    /// 选区颜色时底色取统一的 `selectionOnDark` 或 `selectionOnLight`、字色不变；只配了底色时字色取格子
    /// 原来的背景色。
    static func selectionColors(foreground: Rgb, background: Rgb, settings: TermSettings, screenBackground: Rgb)
        -> (foreground: Rgb, background: Rgb)
    {
        func resolve(_ color: TerminalColor) -> Rgb {
            switch color {
            case .rgb(let rgb): rgb
            case .cellForeground: foreground
            case .cellBackground: background
            }
        }
        let selectedBackground = settings.selectionBackground.map(resolve) ?? defaultBackground(screenBackground)
        let selectedForeground: Rgb =
            switch (settings.selectionForeground, settings.selectionBackground) {
            case (let color?, _): resolve(color)
            case (nil, _?): background
            case (nil, nil): foreground
            }
        return (selectedForeground, selectedBackground)
    }

    private static func defaultBackground(_ screenBackground: Rgb) -> Rgb {
        let luma =
            0.299 * Double(screenBackground.r) + 0.587 * Double(screenBackground.g) + 0.114 * Double(screenBackground.b)
        return luma < 128 ? selectionOnDark : selectionOnLight
    }

    private static func cellAt(_ cells: [[ScreenCell]], _ cell: GridCell) -> ScreenCell? {
        guard cells.indices.contains(cell.row), cells[cell.row].indices.contains(cell.column) else { return nil }
        return cells[cell.row][cell.column]
    }
}

#if os(iOS)
    /// 选区一端的拖柄：一根和行一样高的竖条，终点柄的圆点在下，颜色取 `tintColor`。
    /// 拖动交给 `onPan`；能按住的范围放大到 `minimumTouchSize` 见方。
    final class SelectionHandleView: UIView {
        let end: SelectionEnd
        var onPan: ((SelectionHandleView, UIPanGestureRecognizer) -> Void)?

        static let barWidth: CGFloat = 2
        static let knobDiameter: CGFloat = 10
        static let minimumTouchSize: CGFloat = 44

        init(end: SelectionEnd) {
            self.end = end
            super.init(frame: .zero)
            isOpaque = false
            backgroundColor = .clear
            isHidden = true
            isAccessibilityElement = false
            addGestureRecognizer(UIPanGestureRecognizer(target: self, action: #selector(panned(_:))))
        }

        @available(*, unavailable)
        required init?(coder: NSCoder) {
            fatalError("init(coder:) is not supported")
        }

        /// 圆点在竖条上头：起点柄平常在上头，上面没地方时（选区在视图第一行）挪到下头，免得伸出视图被盖住。
        private var knobAbove = false

        /// 摆到竖条贴着 `x`、从 `top` 到 `bottom` 的位置（父视图坐标），圆点伸到行外。
        func place(x: CGFloat, top: CGFloat, bottom: CGFloat) {
            let knob = Self.knobDiameter
            let above = end == .start && top - knob >= 0
            let frame = CGRect(
                x: x - knob / 2, y: above ? top - knob : top, width: knob,
                height: max(bottom - top, 0) + knob)
            if self.frame != frame || knobAbove != above {
                self.frame = frame
                knobAbove = above
                setNeedsDisplay()
            }
        }

        override func draw(_ rect: CGRect) {
            guard let context = UIGraphicsGetCurrentContext() else { return }
            let knob = Self.knobDiameter
            context.setFillColor(tintColor.cgColor)
            let barTop: CGFloat = knobAbove ? knob / 2 : 0
            let barBottom = knobAbove ? bounds.height : bounds.height - knob / 2
            context.fill(
                CGRect(x: (bounds.width - Self.barWidth) / 2, y: barTop, width: Self.barWidth, height: barBottom - barTop))
            let knobY = knobAbove ? 0 : bounds.height - knob
            context.fillEllipse(in: CGRect(x: 0, y: knobY, width: knob, height: knob))
        }

        override func tintColorDidChange() {
            super.tintColorDidChange()
            setNeedsDisplay()
        }

        override func point(inside point: CGPoint, with event: UIEvent?) -> Bool {
            let size = Self.minimumTouchSize
            let dx = min(0, (bounds.width - size) / 2)
            let dy = min(0, (bounds.height - size) / 2)
            return bounds.insetBy(dx: dx, dy: dy).contains(point)
        }

        @objc private func panned(_ pan: UIPanGestureRecognizer) {
            onPan?(self, pan)
        }
    }
#endif
