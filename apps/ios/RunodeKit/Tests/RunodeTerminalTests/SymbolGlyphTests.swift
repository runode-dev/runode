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
