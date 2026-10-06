#if os(iOS)
    import CoreText
    import RunodeProtocol
    import UIKit

    /// 终端用的等宽字体和由它算出的单元格尺寸。
    struct TerminalFont {
        let size: CGFloat
        let regular: CTFont
        let bold: CTFont
        let italic: CTFont
        let boldItalic: CTFont
        let cellWidth: CGFloat
        let cellHeight: CGFloat
        let ascent: CGFloat
        /// 主字体里没有的私用区图标（Powerline、Nerd Font）退到的符号字体。
        let symbols: CTFont?

        init(size: CGFloat) {
            self.size = size
            symbols = SymbolFont.font(size: size)
            let regularFont = UIFont.monospacedSystemFont(ofSize: size, weight: .regular)
            let boldFont = UIFont.monospacedSystemFont(ofSize: size, weight: .bold)
            regular = regularFont as CTFont
            bold = boldFont as CTFont
            func italicized(_ font: UIFont) -> CTFont {
                guard let descriptor = font.fontDescriptor.withSymbolicTraits(font.fontDescriptor.symbolicTraits.union(.traitItalic))
                else { return font as CTFont }
                return UIFont(descriptor: descriptor, size: size) as CTFont
            }
            italic = italicized(regularFont)
            boldItalic = italicized(boldFont)
            var character: UniChar = 0x4D  // "M"
            var glyph = CGGlyph()
            CTFontGetGlyphsForCharacters(regular, &character, &glyph, 1)
            var advance = CGSize.zero
            CTFontGetAdvancesForGlyphs(regular, .horizontal, &glyph, &advance, 1)
            cellWidth = advance.width
            ascent = CTFontGetAscent(regular)
            cellHeight = ceil(ascent + CTFontGetDescent(regular) + CTFontGetLeading(regular))
        }

        /// 细线（下划线、删除线、Powerline 的细折线）的粗细。
        var thickness: CGFloat { max(1, size / 14) }

        func font(bold isBold: Bool, italic isItalic: Bool) -> CTFont {
            switch (isBold, isItalic) {
            case (true, true): boldItalic
            case (true, false): bold
            case (false, true): italic
            case (false, false): regular
            }
        }
    }

    /// 用 CoreText 画一屏 `ScreenFrame`：背景、文字、下划线和删除线、光标。只重画脏了的行（调用方按行
    /// `setNeedsDisplay(in:)`），整屏的位图由 UIKit 留着。
    final class TerminalGridView: UIView {
        var font = TerminalFont(size: 13)
        var screen = ScreenFrame()
        var settings = TermSettings.default
        /// 视图没有键盘焦点时光标画成空心的。
        var hasKeyboardFocus = false {
            didSet { if hasKeyboardFocus != oldValue { invalidateCursor() } }
        }
        private var colorCache: [Rgb: CGColor] = [:]

        override init(frame: CGRect) {
            super.init(frame: frame)
            isOpaque = true
            contentMode = .topLeft
            layer.drawsAsynchronously = false
        }

        @available(*, unavailable)
        required init?(coder: NSCoder) {
            fatalError("init(coder:) is not supported")
        }

        /// 网格在字体尺寸下占的大小（点）。
        var gridSize: CGSize {
            CGSize(width: CGFloat(screen.columns) * font.cellWidth, height: CGFloat(screen.rows) * font.cellHeight)
        }

        func rect(forRow row: Int) -> CGRect {
            CGRect(x: 0, y: CGFloat(row) * font.cellHeight, width: bounds.width, height: font.cellHeight)
        }

        /// 光标所在格子（宽字符占两格）的矩形；没有光标时为空。
        var cursorRect: CGRect? {
            guard let cursor = screen.cursor else { return nil }
            let column = cursor.onWideTail ? max(cursor.column - 1, 0) : cursor.column
            let wide = cellAt(row: cursor.row, column: column)?.width == .wide
            return CGRect(
                x: CGFloat(column) * font.cellWidth, y: CGFloat(cursor.row) * font.cellHeight,
                width: font.cellWidth * (wide ? 2 : 1), height: font.cellHeight)
        }

        func invalidateCursor() {
            if let cursor = screen.cursor {
                setNeedsDisplay(rect(forRow: cursor.row))
            }
        }

        private func cellAt(row: Int, column: Int) -> ScreenCell? {
            guard row >= 0, row < screen.cells.count, column >= 0, column < screen.cells[row].count else { return nil }
            return screen.cells[row][column]
        }

        private func color(_ rgb: Rgb, alpha: CGFloat = 1) -> CGColor {
            if alpha == 1, let cached = colorCache[rgb] { return cached }
            let color = CGColor(
                srgbRed: CGFloat(rgb.r) / 255, green: CGFloat(rgb.g) / 255, blue: CGFloat(rgb.b) / 255, alpha: alpha)
            if alpha == 1 { colorCache[rgb] = color }
            return color
        }

        /// 单元格实际的前景、背景色：没设的用默认色，反显时对调。
        private func colors(of cell: ScreenCell) -> (foreground: Rgb, background: Rgb) {
            let foreground = cell.foreground ?? screen.foreground
            let background = cell.background ?? screen.background
            return cell.attributes.contains(.inverse) ? (background, foreground) : (foreground, background)
        }

        override func draw(_ rect: CGRect) {
            guard let context = UIGraphicsGetCurrentContext() else { return }
            context.setFillColor(color(screen.background))
            context.fill(rect)
            guard screen.rows > 0, font.cellHeight > 0 else { return }
            let first = max(0, Int(rect.minY / font.cellHeight))
            let last = min(screen.rows - 1, Int((rect.maxY - 0.01) / font.cellHeight))
            guard first <= last else { return }
            for row in first...last {
                drawBackgrounds(row: row, in: context)
            }
            for row in first...last {
                drawText(row: row, in: context)
            }
            if let cursor = screen.cursor, (first...last).contains(cursor.row) {
                drawCursor(cursor, in: context)
            }
        }

        /// 背景按同色的一段一起填：格子宽度不是整像素，一格一格填时同色的相邻格子之间会露出细缝。
        private func drawBackgrounds(row: Int, in context: CGContext) {
            let top = CGFloat(row) * font.cellHeight
            var run: (start: Int, end: Int, color: Rgb)?
            func flush() {
                guard let current = run else { return }
                context.setFillColor(color(current.color))
                context.fill(
                    CGRect(
                        x: CGFloat(current.start) * font.cellWidth, y: top,
                        width: CGFloat(current.end - current.start) * font.cellWidth, height: font.cellHeight))
                run = nil
            }
            for (column, cell) in screen.cells[row].enumerated() {
                // 宽字符的后一格跟着前一格的背景走。
                let background =
                    cell.width == .spacerTail && column > 0
                    ? colors(of: screen.cells[row][column - 1]).background : colors(of: cell).background
                if let current = run, current.color == background, current.end == column {
                    run?.end = column + 1
                    continue
                }
                flush()
                if background != screen.background {
                    run = (column, column + 1, background)
                }
            }
            flush()
        }

        /// 这一格后面紧跟着一个空格子：图标可以占两格宽，和 Ghostty 处理 Nerd Font 图标的做法一样。
        private func followedByBlank(row: Int, column: Int) -> Bool {
            guard let next = cellAt(row: row, column: column + 1) else { return false }
            return next.width == .narrow && (next.text.isEmpty || next.text == " ")
        }

        private func drawText(row: Int, in context: CGContext) {
            let top = CGFloat(row) * font.cellHeight
            let baseline = top + font.ascent
            for (column, cell) in screen.cells[row].enumerated() {
                guard cell.width != .spacerTail, cell.width != .spacerHead else { continue }
                let x = CGFloat(column) * font.cellWidth
                let width = font.cellWidth * (cell.width == .wide ? 2 : 1)
                let foreground = colors(of: cell).foreground
                let faint = cell.attributes.contains(.faint)
                let ink = color(foreground, alpha: faint ? 0.6 : 1)
                if !cell.text.isEmpty, cell.text != " ", !cell.attributes.contains(.invisible) {
                    let cellRect = CGRect(x: x, y: top, width: width, height: font.cellHeight)
                    if !drawPowerline(cell.text, in: cellRect, color: ink, context: context) {
                        let ctFont = font.font(
                            bold: cell.attributes.contains(.bold), italic: cell.attributes.contains(.italic))
                        let iconWidth =
                            cell.width == .narrow && followedByBlank(row: row, column: column) ? width * 2 : width
                        drawGlyphs(
                            cell.text, font: ctFont, color: ink, at: CGPoint(x: x, y: baseline), width: width,
                            iconWidth: iconWidth, in: context)
                    }
                }
                if cell.underline != .none {
                    context.setFillColor(ink)
                    let y = baseline + max(1, font.size * 0.12)
                    context.fill(CGRect(x: x, y: y, width: width, height: max(1, font.size / 14)))
                    if cell.underline == .double {
                        context.fill(CGRect(x: x, y: y + 2, width: width, height: max(1, font.size / 14)))
                    }
                }
                if cell.attributes.contains(.strikethrough) {
                    context.setFillColor(ink)
                    context.fill(
                        CGRect(x: x, y: top + font.cellHeight / 2, width: width, height: max(1, font.size / 14)))
                }
            }
        }

        /// 制表符和 Powerline 的几何分隔符按格子自己画（见 `BoxDrawing`、`PowerlineGlyph`）；不是这类字符时
        /// 返回 false。形状左右各
        /// 多画半个像素，盖住和相邻格子背景之间因为格子宽度不是整像素而露出的缝。
        private func drawPowerline(_ text: String, in rect: CGRect, color: CGColor, context: CGContext) -> Bool {
            guard let scalar = text.unicodeScalars.first, text.unicodeScalars.count == 1 else { return false }
            if BoxDrawing.handles(scalar) {
                let scale = max(contentScaleFactor, 1)
                guard let path = BoxDrawing.path(for: scalar, in: rect, thickness: font.thickness, scale: scale)
                else { return false }
                context.saveGState()
                defer { context.restoreGState() }
                context.addPath(path)
                context.setFillColor(color)
                context.fillPath()
                return true
            }
            guard PowerlineGlyph.handles(scalar) else { return false }
            let hair = 0.5 / max(contentScaleFactor, 1)
            let padded = rect.insetBy(dx: -hair, dy: 0)
            guard let (path, paint) = PowerlineGlyph.path(for: scalar, in: padded, thickness: font.thickness) else {
                return false
            }
            context.saveGState()
            defer { context.restoreGState() }
            context.clip(to: padded)
            context.addPath(path)
            switch paint {
            case .fill:
                context.setFillColor(color)
                context.fillPath()
            case .stroke(let width):
                context.setStrokeColor(color)
                context.setLineWidth(width)
                context.setLineCap(.butt)
                context.strokePath()
            }
            return true
        }

        /// 画一个字素簇：等宽字体里有的字直接画字形；私用区的图标主字体缺字时退到打包的符号字体，缩放到
        /// `iconWidth` 宽、在格子里居中；其余（中文、emoji、组合字符）交给 CTLine 找系统的后备字体，比格子
        /// 窄时在格子里居中（宽字符占的两格比中文字形宽一点）。
        private func drawGlyphs(
            _ text: String, font ctFont: CTFont, color: CGColor, at origin: CGPoint, width: CGFloat,
            iconWidth: CGFloat, in context: CGContext
        ) {
            context.saveGState()
            defer { context.restoreGState() }
            context.textMatrix = .identity
            context.translateBy(x: origin.x, y: origin.y)
            context.scaleBy(x: 1, y: -1)
            if let scalar = text.unicodeScalars.first, text.unicodeScalars.count == 1 {
                var units = Array(text.utf16)
                var glyphs = [CGGlyph](repeating: 0, count: units.count)
                if CTFontGetGlyphsForCharacters(ctFont, &units, &glyphs, units.count), glyphs[0] != 0 {
                    context.setFillColor(color)
                    var position = CGPoint.zero
                    CTFontDrawGlyphs(ctFont, &glyphs, &position, 1, context)
                    return
                }
                if SymbolFont.isPrivateUse(scalar), let symbols = font.symbols,
                    CTFontGetGlyphsForCharacters(symbols, &units, &glyphs, units.count), glyphs[0] != 0
                {
                    drawSymbol(glyphs[0], from: symbols, color: color, width: iconWidth, in: context)
                    return
                }
            }
            let attributed = NSAttributedString(
                string: text,
                attributes: [
                    NSAttributedString.Key(kCTFontAttributeName as String): ctFont,
                    NSAttributedString.Key(kCTForegroundColorAttributeName as String): color,
                ])
            let line = CTLineCreateWithAttributedString(attributed)
            let advance = CGFloat(CTLineGetTypographicBounds(line, nil, nil, nil))
            context.textPosition = CGPoint(x: max(0, (width - advance) / 2), y: 0)
            CTLineDraw(line, context)
        }

        /// 在已经平移到基线、翻转成 y 向上的坐标系里画一个符号字体的字形：按字形的外框缩到 `width` 宽、
        /// 不超过格子高，水平居中，竖直方向以格子中线居中。
        private func drawSymbol(_ glyph: CGGlyph, from symbols: CTFont, color: CGColor, width: CGFloat, in context: CGContext) {
            var glyph = glyph
            let natural = CTFontGetBoundingRectsForGlyphs(symbols, .horizontal, &glyph, nil, 1)
            guard natural.width > 0, natural.height > 0 else { return }
            let scale = min(1, width * 0.96 / natural.width, font.cellHeight * 0.96 / natural.height)
            let scaled = CTFontCreateCopyWithAttributes(symbols, CTFontGetSize(symbols) * scale, nil, nil)
            let bounds = CTFontGetBoundingRectsForGlyphs(scaled, .horizontal, &glyph, nil, 1)
            let center = font.ascent - font.cellHeight / 2
            var position = CGPoint(x: (width - bounds.width) / 2 - bounds.minX, y: center - bounds.midY)
            context.setFillColor(color)
            CTFontDrawGlyphs(scaled, &glyph, &position, 1, context)
        }

        private func drawCursor(_ cursor: ScreenCursor, in context: CGContext) {
            guard let rect = cursorRect else { return }
            let column = cursor.onWideTail ? max(cursor.column - 1, 0) : cursor.column
            let cell = cellAt(row: cursor.row, column: column) ?? .blank
            let (cellForeground, cellBackground) = colors(of: cell)
            let cursorColor: Rgb =
                switch settings.cursorColor {
                case .rgb(let rgb): rgb
                case .cellForeground: cellForeground
                case .cellBackground: cellBackground
                case nil: screen.cursorColor ?? screen.foreground
                }
            let shape: CursorShape = hasKeyboardFocus ? cursor.shape : .blockHollow
            context.setFillColor(color(cursorColor))
            context.setStrokeColor(color(cursorColor))
            switch shape {
            case .block:
                context.fill(rect)
                guard !cell.text.isEmpty, cell.text != " " else { return }
                let textColor: Rgb =
                    switch settings.cursorText {
                    case .rgb(let rgb): rgb
                    case .cellForeground: cellForeground
                    case .cellBackground, nil: cellBackground
                    }
                let textInk = color(textColor)
                if drawPowerline(cell.text, in: rect, color: textInk, context: context) { return }
                let ctFont = font.font(bold: cell.attributes.contains(.bold), italic: cell.attributes.contains(.italic))
                drawGlyphs(
                    cell.text, font: ctFont, color: textInk, at: CGPoint(x: rect.minX, y: rect.minY + font.ascent),
                    width: rect.width, iconWidth: rect.width, in: context)
            case .blockHollow:
                context.setLineWidth(1)
                context.stroke(rect.insetBy(dx: 0.5, dy: 0.5))
            case .bar:
                context.fill(CGRect(x: rect.minX, y: rect.minY, width: max(1.5, font.size / 8), height: rect.height))
            case .underline:
                let height = max(1.5, font.size / 8)
                context.fill(CGRect(x: rect.minX, y: rect.maxY - height, width: rect.width, height: height))
            }
        }
    }
#endif
