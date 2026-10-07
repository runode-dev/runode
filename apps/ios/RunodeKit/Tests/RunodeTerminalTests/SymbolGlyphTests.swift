import CoreGraphics
import CoreText
import Testing

@testable import RunodeTerminal

@Suite struct SymbolGlyphTests {
    /// 打包的 Symbols Nerd Font Mono 能加载，提示符里常见的私用区图标都有字形：Powerline 的分支符号、
    /// Font Awesome 的 Apple 标志、辅助平面上的 Material Design 图标。
    @Test func bundledSymbolFontHasPromptIcons() throws {
        let font = try #require(SymbolFont.font(size: 13))
        for scalar: UInt32 in [0xE0A0, 0xE0B0, 0xF113, 0xF0001] {
            var units = Array(String(Unicode.Scalar(scalar)!).utf16)
            var glyphs = [CGGlyph](repeating: 0, count: units.count)
            #expect(CTFontGetGlyphsForCharacters(font, &units, &glyphs, units.count), "U+\(String(scalar, radix: 16))")
            #expect(glyphs[0] != 0)
        }
    }

    /// 实心的分隔符正好填满格子：外框就是格子的矩形，相邻格子之间没有空隙。
    @Test(arguments: [0xE0B0, 0xE0B2, 0xE0B4, 0xE0B6, 0xE0B8, 0xE0BA, 0xE0BC, 0xE0BE] as [UInt32])
    func solidSeparatorsFillTheCell(_ value: UInt32) throws {
        let rect = CGRect(x: 15.5, y: 32, width: 7.8, height: 16)
        let (path, paint) = try #require(PowerlineGlyph.path(for: Unicode.Scalar(value)!, in: rect, thickness: 1))
        guard case .fill = paint else {
            Issue.record("U+\(String(value, radix: 16)) should be filled")
            return
        }
        let box = path.boundingBoxOfPath
        // 半圆的半径取宽度和半高里小的那个，宽度用满。
        #expect(abs(box.minX - rect.minX) < 0.001 && abs(box.maxX - rect.maxX) < 0.001)
        #expect(abs(box.minY - rect.minY) < 0.001 && abs(box.maxY - rect.maxY) < 0.001)
    }

    @Test func rightTriangleTouchesTheLeftEdgeTopToBottom() throws {
        let rect = CGRect(x: 0, y: 0, width: 8, height: 16)
        let (path, _) = try #require(PowerlineGlyph.path(for: "\u{E0B0}", in: rect, thickness: 1))
        #expect(path.contains(CGPoint(x: 0.1, y: 0.5)))
        #expect(path.contains(CGPoint(x: 0.1, y: 15.5)))
        #expect(path.contains(CGPoint(x: 7.5, y: 8)))
        #expect(!path.contains(CGPoint(x: 7.5, y: 1)))
        // 左指的是镜像。
        let (left, _) = try #require(PowerlineGlyph.path(for: "\u{E0B2}", in: rect, thickness: 1))
        #expect(left.contains(CGPoint(x: 7.9, y: 0.5)))
        #expect(!left.contains(CGPoint(x: 0.5, y: 1)))
    }

    @Test func thinSeparatorsAreStroked() throws {
        let rect = CGRect(x: 0, y: 0, width: 8, height: 16)
        for value: UInt32 in [0xE0B1, 0xE0B3, 0xE0B5, 0xE0B7, 0xE0B9, 0xE0BB, 0xE0BD, 0xE0BF] {
            let (_, paint) = try #require(PowerlineGlyph.path(for: Unicode.Scalar(value)!, in: rect, thickness: 1.5))
            guard case .stroke(let width) = paint else {
                Issue.record("U+\(String(value, radix: 16)) should be stroked")
                continue
            }
            #expect(width == 1.5)
        }
    }

    @Test func otherCharactersAreLeftToFonts() {
        #expect(!PowerlineGlyph.handles("\u{E0A0}"))
        #expect(!PowerlineGlyph.handles("A"))
        #expect(PowerlineGlyph.path(for: "\u{F113}", in: CGRect(x: 0, y: 0, width: 8, height: 16), thickness: 1) == nil)
    }
}

@Suite struct BoxDrawingTests {
    let rect = CGRect(x: 7.83, y: 16, width: 7.83, height: 16)

    /// `─` 从格子左边画到右边，两头对齐设备像素：相邻两格算出同一条边，一排接起来没有缝。
    @Test func horizontalLinesSpanTheCellOnPixelEdges() throws {
        let scale: CGFloat = 3
        let path = try #require(BoxDrawing.path(for: "\u{2500}", in: rect, thickness: 1, scale: scale))
        let box = path.boundingBoxOfPath
        #expect(abs(box.minX - (rect.minX * scale).rounded() / scale) < 0.0001)
        #expect(abs(box.maxX - (rect.maxX * scale).rounded() / scale) < 0.0001)
        let next = try #require(
            BoxDrawing.path(for: "\u{2500}", in: rect.offsetBy(dx: rect.width, dy: 0), thickness: 1, scale: scale))
        #expect(abs(next.boundingBoxOfPath.minX - box.maxX) < 0.0001)
        #expect(box.height >= 1 / scale)
        #expect(box.midY > rect.minY && box.midY < rect.maxY)
    }

    @Test func cornersReachTheirTwoEdges() throws {
        let path = try #require(BoxDrawing.path(for: "\u{250C}", in: rect, thickness: 1, scale: 2))
        let box = path.boundingBoxOfPath
        // ┌ 往右、往下：碰到右边和下边，碰不到左边和上边。
        #expect(abs(box.maxX - (rect.maxX * 2).rounded() / 2) < 0.0001)
        #expect(abs(box.maxY - (rect.maxY * 2).rounded() / 2) < 0.0001)
        #expect(box.minX > rect.minX + 1)
        #expect(box.minY > rect.minY + 1)
    }

    @Test func heavyLinesAreThicker() throws {
        let light = try #require(BoxDrawing.path(for: "\u{2500}", in: rect, thickness: 1, scale: 3))
        let heavy = try #require(BoxDrawing.path(for: "\u{2501}", in: rect, thickness: 1, scale: 3))
        #expect(heavy.boundingBoxOfPath.height > light.boundingBoxOfPath.height)
    }

    @Test func roundedCornersAndUnknowns() throws {
        for value: UInt32 in [0x256D, 0x256E, 0x256F, 0x2570] {
            let path = try #require(BoxDrawing.path(for: Unicode.Scalar(value)!, in: rect, thickness: 1, scale: 3))
            #expect(!path.boundingBoxOfPath.isEmpty)
        }
        #expect(!BoxDrawing.handles("A"))
        #expect(BoxDrawing.path(for: "\u{2550}", in: rect, thickness: 1, scale: 3) == nil)
    }
}

@Suite struct SymbolFallbackTests {
    /// 预览和终端视图用同一个判断决定哪些字退到符号字体：私用区（含辅助平面的）和几个电源符号，
    /// 中文、emoji 照旧交给系统的后备字体。
    @Test func privateUseDetection() {
        for scalar in ["\u{E0A0}", "\u{E0B0}", "\u{F113}", "\u{F0001}", "\u{23FB}"] as [Unicode.Scalar] {
            #expect(SymbolFont.isPrivateUse(scalar), "\(scalar)")
        }
        for scalar in ["A", "中", "✅", "\u{2500}"] as [Unicode.Scalar] {
            #expect(!SymbolFont.isPrivateUse(scalar), "\(scalar)")
        }
    }

    /// 符号字体注册到了本进程，SwiftUI 能按 PostScript 名字找到它。
    @Test func bundledSymbolFontIsRegisteredByName() throws {
        let name = try #require(SymbolFont.postScriptName)
        let font = CTFontCreateWithName(name as CFString, 12, nil)
        #expect(CTFontCopyPostScriptName(font) as String == name)
    }

    /// 默认按文字画的 `⏺`、`✳` 补上文字样式选择符；默认就是 emoji 的 `❗`、ASCII、已经带了选择符的不动。
    @Test func textPresentationMarksTextDefaultEmoji() {
        #expect(TextPresentation.apply(to: "⏺ 改好了") == "⏺\u{FE0E} 改好了")
        #expect(TextPresentation.apply(to: "✳") == "✳\u{FE0E}")
        #expect(TextPresentation.apply(to: "❗ #1 *") == "❗ #1 *")
        #expect(TextPresentation.apply(to: "✔\u{FE0F}") == "✔\u{FE0F}")
    }
}
