import CoreGraphics
import CoreText
import Foundation

/// 打包在 app 里的 Nerd Fonts「Symbols Only」等宽版（Symbols Nerd Font Mono，Nerd Fonts v3.5.1，
/// 字体本身 MIT 许可；各图标集的来源和许可写在随字体一起打包的 README 和 LICENSE 里）。zsh 提示符里的 Powerline、
/// Git 分支这类私用区图标主字体里没有，画单元格时退到这里。
enum SymbolFont {
    /// 字体的 PostScript 名字；注册失败（资源缺了）时为空。第一次用时把字体注册到本进程。
    static let postScriptName: String? = {
        guard
            let url = Bundle.module.url(
                forResource: "SymbolsNerdFontMono-Regular", withExtension: "ttf", subdirectory: "NerdFontsSymbolsOnly")
        else { return nil }
        // 已经注册过（比如测试里跑了两遍）会报错，不影响使用。
        CTFontManagerRegisterFontsForURL(url as CFURL, .process, nil)
        guard let descriptors = CTFontManagerCreateFontDescriptorsFromURL(url as CFURL) as? [CTFontDescriptor],
            let descriptor = descriptors.first
        else { return nil }
        return CTFontCopyPostScriptName(CTFontCreateWithFontDescriptor(descriptor, 12, nil)) as String
    }()

    /// 这个字号的符号字体；字体没打包进来时为空。
    static func font(size: CGFloat) -> CTFont? {
        guard let name = postScriptName else { return nil }
        let font = CTFontCreateWithName(name as CFString, size, nil)
        // 找不到这个名字时 CoreText 会换成别的字体，那就当作没有。
        guard CTFontCopyPostScriptName(font) as String == name else { return nil }
        return font
    }
}

/// Powerline 和 Powerline Extra 里几何形状的分隔符（U+E0B0–U+E0BF、U+E0D2、U+E0D4），照 Ghostty 的
/// 做法（它的 `drawE0B0` 这一组函数）不用字体，按单元格的矩形自己画：形状正好填满格子，和相邻格子的
/// 背景色之间不会有缝。坐标系是 UIKit 的（原点在左上、y 向下），和 Ghostty 画布的一样。
enum PowerlineGlyph {
    enum Paint {
        case fill
        /// 描边，带线宽。
        case stroke(CGFloat)
    }

    static func handles(_ scalar: Unicode.Scalar) -> Bool {
        (0xE0B0...0xE0BF).contains(scalar.value) || scalar.value == 0xE0D2 || scalar.value == 0xE0D4
    }

    /// `scalar` 在 `rect` 这个格子里的形状；不是这里画的字符返回 `nil`。`thickness` 是细线的线宽。
    static func path(for scalar: Unicode.Scalar, in rect: CGRect, thickness: CGFloat) -> (CGPath, Paint)? {
        let w = rect.width
        let h = rect.height
        let path = CGMutablePath()
        var paint = Paint.fill
        var flip = false
        switch scalar.value {
        case 0xE0B0:  // 右指的实心三角
            path.addLines(between: [CGPoint(x: 0, y: 0), CGPoint(x: w, y: h / 2), CGPoint(x: 0, y: h)])
            path.closeSubpath()
        case 0xE0B2:  // 左指的实心三角
            path.addLines(between: [CGPoint(x: w, y: 0), CGPoint(x: 0, y: h / 2), CGPoint(x: w, y: h)])
            path.closeSubpath()
        case 0xE0B1, 0xE0B3:  // 右指、左指的细折线
            path.addLines(between: [CGPoint(x: 0, y: 0), CGPoint(x: w, y: h / 2), CGPoint(x: 0, y: h)])
            paint = .stroke(thickness)
            flip = scalar.value == 0xE0B3
        case 0xE0B4, 0xE0B6:  // 右半圆、左半圆，实心
            addHalfCircle(to: path, width: w, height: h, closed: true)
            flip = scalar.value == 0xE0B6
        case 0xE0B5, 0xE0B7:  // 右半圆、左半圆，细线
            addHalfCircle(to: path, width: w, height: h, closed: false)
            paint = .stroke(thickness)
            flip = scalar.value == 0xE0B7
        case 0xE0B8:  // 左下三角
            path.addLines(between: [CGPoint(x: 0, y: 0), CGPoint(x: w, y: h), CGPoint(x: 0, y: h)])
            path.closeSubpath()
        case 0xE0BA:  // 右下三角
            path.addLines(between: [CGPoint(x: w, y: 0), CGPoint(x: w, y: h), CGPoint(x: 0, y: h)])
            path.closeSubpath()
        case 0xE0BC:  // 左上三角
            path.addLines(between: [CGPoint(x: 0, y: 0), CGPoint(x: w, y: 0), CGPoint(x: 0, y: h)])
            path.closeSubpath()
        case 0xE0BE:  // 右上三角
            path.addLines(between: [CGPoint(x: 0, y: 0), CGPoint(x: w, y: 0), CGPoint(x: w, y: h)])
            path.closeSubpath()
        case 0xE0B9, 0xE0BF:  // 左上到右下的细斜线
            path.addLines(between: [CGPoint(x: 0, y: 0), CGPoint(x: w, y: h)])
            paint = .stroke(thickness)
        case 0xE0BB, 0xE0BD:  // 右上到左下的细斜线
            path.addLines(between: [CGPoint(x: w, y: 0), CGPoint(x: 0, y: h)])
            paint = .stroke(thickness)
        case 0xE0D2, 0xE0D4:  // 上下两块梯形夹一条缝
            path.addLines(between: [
                CGPoint(x: 0, y: 0), CGPoint(x: w, y: 0), CGPoint(x: w / 2, y: h / 2 - thickness / 2),
                CGPoint(x: 0, y: h / 2 - thickness / 2),
            ])
            path.closeSubpath()
            path.addLines(between: [
                CGPoint(x: 0, y: h), CGPoint(x: w, y: h), CGPoint(x: w / 2, y: h / 2 + thickness / 2),
                CGPoint(x: 0, y: h / 2 + thickness / 2),
            ])
            path.closeSubpath()
            flip = scalar.value == 0xE0D4
        default:
            return nil
        }
        var transform = CGAffineTransform(translationX: rect.minX, y: rect.minY)
        if flip {
            transform = transform.translatedBy(x: w, y: 0).scaledBy(x: -1, y: 1)
        }
        return (path.copy(using: &transform) ?? path, paint)
    }

    /// 贴着左边的半圆（右半边是弧），半径取宽度和半高里小的那个，和 Ghostty 的一样用贝塞尔近似圆弧。
    private static func addHalfCircle(to path: CGMutablePath, width w: CGFloat, height h: CGFloat, closed: Bool) {
        let c = (sqrt(2) - 1) * 4 / 3
        let r = min(w, h / 2)
        path.move(to: CGPoint(x: 0, y: 0))
        path.addCurve(to: CGPoint(x: r, y: r), control1: CGPoint(x: r * c, y: 0), control2: CGPoint(x: r, y: r - r * c))
        path.addLine(to: CGPoint(x: r, y: h - r))
        path.addCurve(
            to: CGPoint(x: 0, y: h), control1: CGPoint(x: r, y: h - r + r * c), control2: CGPoint(x: r * c, y: h))
        if closed { path.closeSubpath() }
    }
}
