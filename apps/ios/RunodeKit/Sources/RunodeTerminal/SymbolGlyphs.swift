import CoreGraphics
import CoreText
import Foundation

/// 打包在 app 里的 Nerd Fonts「Symbols Only」等宽版（Symbols Nerd Font Mono，Nerd Fonts v3.5.1，
/// 字体本身 MIT 许可；各图标集的来源和许可写在随字体一起打包的 README 和 LICENSE 里）。zsh 提示符里的 Powerline、
/// Git 分支这类私用区图标主字体里没有，画单元格时退到这里。
public enum SymbolFont {
    /// 字体的 PostScript 名字；注册失败（资源缺了）时为空。第一次用时把字体注册到本进程。
    public static let postScriptName: String? = {
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

    /// 私用区等码位：Powerline、Nerd Font 的图标都在这里，主字体缺字时退到符号字体。终端视图和会话列表的
    /// 预览用同一个判断。
    public static func isPrivateUse(_ scalar: Unicode.Scalar) -> Bool {
        (0xE000...0xF8FF).contains(scalar.value) || (0xF0000...0xFFFFD).contains(scalar.value)
            || (0x23FB...0x23FE).contains(scalar.value) || scalar.value == 0x2B58
    }

    /// 这个字号的符号字体；字体没打包进来时为空。
    static func font(size: CGFloat) -> CTFont? {
        guard let name = postScriptName else { return nil }
        let font = CTFontCreateWithName(name as CFString, size, nil)
        // 找不到这个名字时 CoreText 会换成别的字体，那就当作没有。
        guard CTFontCopyPostScriptName(font) as String == name else { return nil }
        return font
    }
}

/// 默认按文字画、却也有 emoji 样子的字（Claude Code 的 `⏺`、转圈的 `✳`、`✔`、`⚠` 这些）。等宽字体里
/// 没有时，Core Text 按码位找后备字体会挑中 Apple Color Emoji，画成不跟前景色的彩色方块；后面跟上
/// U+FE0E（文字样式选择符）它才去找有这个字的普通字体（STIX Two Math、Menlo 等）。`❗` 这类默认就是
/// emoji 的不算，照旧画 emoji。
public enum TextPresentation {
    public static let selector: Character = "\u{FE0E}"

    public static func prefersText(_ scalar: Unicode.Scalar) -> Bool {
        // ASCII 里的数字、`#`、`*` 也算 emoji 字符，排除掉。
        scalar.value > 0x7F && scalar.properties.isEmoji && !scalar.properties.isEmojiPresentation
    }

    /// 给 `text` 里单码位的这类字后面补上文字样式选择符，交给 SwiftUI `Text` 这类自己找后备字体的地方。
    public static func apply(to text: String) -> String {
        var result = ""
        for character in text {
            result.append(character)
            if character.unicodeScalars.count == 1, let scalar = character.unicodeScalars.first, prefersText(scalar) {
                result.append(selector)
            }
        }
        return result
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

/// 制表符（U+2500–U+257F 里常用的那些：细线、粗线、直角和圆角、T 形、十字、半截线），照 Ghostty 的
/// 做法不用字体按格子自己画。字体里的制表符画不满格子，一排 `─` 会成虚线；这里每条线从格子中心
/// 画到边上，边按设备像素对齐，相邻格子算出同一条边界，接起来没有缝。
enum BoxDrawing {
    /// 一个字符四个方向上的线：0 没有，1 细，2 粗。
    private struct Arms {
        var up: Int, right: Int, down: Int, left: Int
    }

    private static let arms: [UInt32: Arms] = [
        0x2500: Arms(up: 0, right: 1, down: 0, left: 1), 0x2501: Arms(up: 0, right: 2, down: 0, left: 2),
        0x2502: Arms(up: 1, right: 0, down: 1, left: 0), 0x2503: Arms(up: 2, right: 0, down: 2, left: 0),
        0x250C: Arms(up: 0, right: 1, down: 1, left: 0), 0x250F: Arms(up: 0, right: 2, down: 2, left: 0),
        0x2510: Arms(up: 0, right: 0, down: 1, left: 1), 0x2513: Arms(up: 0, right: 0, down: 2, left: 2),
        0x2514: Arms(up: 1, right: 1, down: 0, left: 0), 0x2517: Arms(up: 2, right: 2, down: 0, left: 0),
        0x2518: Arms(up: 1, right: 0, down: 0, left: 1), 0x251B: Arms(up: 2, right: 0, down: 0, left: 2),
        0x251C: Arms(up: 1, right: 1, down: 1, left: 0), 0x2523: Arms(up: 2, right: 2, down: 2, left: 0),
        0x2524: Arms(up: 1, right: 0, down: 1, left: 1), 0x252B: Arms(up: 2, right: 0, down: 2, left: 2),
        0x252C: Arms(up: 0, right: 1, down: 1, left: 1), 0x2533: Arms(up: 0, right: 2, down: 2, left: 2),
        0x2534: Arms(up: 1, right: 1, down: 0, left: 1), 0x253B: Arms(up: 2, right: 2, down: 0, left: 2),
        0x253C: Arms(up: 1, right: 1, down: 1, left: 1), 0x254B: Arms(up: 2, right: 2, down: 2, left: 2),
        0x2574: Arms(up: 0, right: 0, down: 0, left: 1), 0x2575: Arms(up: 1, right: 0, down: 0, left: 0),
        0x2576: Arms(up: 0, right: 1, down: 0, left: 0), 0x2577: Arms(up: 0, right: 0, down: 1, left: 0),
        0x2578: Arms(up: 0, right: 0, down: 0, left: 2), 0x2579: Arms(up: 2, right: 0, down: 0, left: 0),
        0x257A: Arms(up: 0, right: 2, down: 0, left: 0), 0x257B: Arms(up: 0, right: 0, down: 2, left: 0),
    ]

    static func handles(_ scalar: Unicode.Scalar) -> Bool {
        arms[scalar.value] != nil || (0x256D...0x2570).contains(scalar.value)
    }

    /// `scalar` 在 `rect` 这个格子里要填的形状；不是这里画的字符返回 `nil`。`thickness` 是细线的粗细，
    /// `scale` 是设备像素和点的比例，边按它对齐。
    static func path(for scalar: Unicode.Scalar, in rect: CGRect, thickness: CGFloat, scale: CGFloat) -> CGPath? {
        let snap = { (value: CGFloat) -> CGFloat in (value * scale).rounded() / scale }
        let light = max(snap(thickness), 1 / scale)
        let path = CGMutablePath()
        let left = snap(rect.minX)
        let right = snap(rect.maxX)
        let top = snap(rect.minY)
        let bottom = snap(rect.maxY)
        /// 中线两边对称的一条带子：粗细 `width`，起止对齐像素。
        func band(center: CGFloat, width: CGFloat) -> (CGFloat, CGFloat) {
            let start = snap(center - width / 2)
            return (start, start + max(snap(width), 1 / scale))
        }
        if let arms = arms[scalar.value] {
            let widest = CGFloat(max(arms.up, arms.down, arms.left, arms.right)) * light
            let (vx0, _) = band(center: rect.midX, width: widest)
            let (hy0, _) = band(center: rect.midY, width: widest)
            // 横线、竖线各自伸到中心那块的另一边，拐角和十字处补满。
            func horizontal(_ weight: Int, from x0: CGFloat, to x1: CGFloat) {
                guard weight > 0 else { return }
                let (y0, y1) = band(center: rect.midY, width: CGFloat(weight) * light)
                path.addRect(CGRect(x: x0, y: y0, width: x1 - x0, height: y1 - y0))
            }
            func vertical(_ weight: Int, from y0: CGFloat, to y1: CGFloat) {
                guard weight > 0 else { return }
                let (x0, x1) = band(center: rect.midX, width: CGFloat(weight) * light)
                path.addRect(CGRect(x: x0, y: y0, width: x1 - x0, height: y1 - y0))
            }
            let joinRight = vx0 + max(snap(widest), 1 / scale)
            let joinBottom = hy0 + max(snap(widest), 1 / scale)
            horizontal(arms.left, from: left, to: arms.right > 0 ? right : joinRight)
            if arms.right != arms.left { horizontal(arms.right, from: vx0, to: right) }
            vertical(arms.up, from: top, to: arms.down > 0 ? bottom : joinBottom)
            if arms.down != arms.up { vertical(arms.down, from: hy0, to: bottom) }
            return path
        }
        // 圆角：从一条边的中点画四分之一圆弧到另一条边的中点，描成细线，外轮廓转成可以填的形状。
        let center = CGPoint(x: (left + right) / 2, y: (top + bottom) / 2)
        let radius = min(rect.width, rect.height) / 2
        let arc = CGMutablePath()
        switch scalar.value {
        case 0x256D...0x2570:
            // ╭ ╮ 向下，╯ ╰ 向上；╭ ╰ 向右，╮ ╯ 向左。
            let down = scalar.value <= 0x256E
            let toRight = scalar.value == 0x256D || scalar.value == 0x2570
            let side = toRight ? right : left
            arc.move(to: CGPoint(x: center.x, y: down ? bottom : top))
            arc.addLine(to: CGPoint(x: center.x, y: down ? center.y + radius : center.y - radius))
            arc.addArc(tangent1End: center, tangent2End: CGPoint(x: side, y: center.y), radius: radius)
            arc.addLine(to: CGPoint(x: side, y: center.y))
        default:
            return nil
        }
        return arc.copy(strokingWithWidth: light, lineCap: .butt, lineJoin: .round, miterLimit: 1)
    }
}

/// 块元素（U+2580–U+259F：半块、八分之几的块、整块、阴影 `░▒▓`、象限块），和桌面一样不用字体按格子
/// 自己画：块正好铺满格子，阴影用前景色加 1/4、1/2、3/4 的不透明度铺满，不画字体里的点阵，相邻格子
/// 接起来没有花纹和缝。
enum BlockElement {
    /// 象限块 U+2596–U+259F 的组成：1 左上、2 右上、4 左下、8 右下。
    private static let quadrants: [UInt8] = [4, 8, 1, 13, 9, 7, 11, 2, 6, 14]

    static func handles(_ scalar: Unicode.Scalar) -> Bool {
        (0x2580...0x259F).contains(scalar.value)
    }

    /// `scalar` 在 `rect` 这个格子里要填的形状和填色的不透明度；不是这里画的字符返回 `nil`。边按 `scale`
    /// （设备像素和点的比例）对齐。
    static func shape(for scalar: Unicode.Scalar, in rect: CGRect, scale: CGFloat) -> (path: CGPath, alpha: CGFloat)? {
        let snap = { (value: CGFloat) -> CGFloat in (value * scale).rounded() / scale }
        /// 格子里按比例取的一块，x、y 都在 0...1。
        func part(_ x0: CGFloat, _ x1: CGFloat, _ y0: CGFloat, _ y1: CGFloat) -> CGRect {
            let left = snap(rect.minX + rect.width * x0)
            let right = snap(rect.minX + rect.width * x1)
            let top = snap(rect.minY + rect.height * y0)
            let bottom = snap(rect.minY + rect.height * y1)
            return CGRect(x: left, y: top, width: right - left, height: bottom - top)
        }
        let value = scalar.value
        var alpha: CGFloat = 1
        let rects: [CGRect]
        switch value {
        case 0x2580: rects = [part(0, 1, 0, 0.5)]
        // ▁▂▃▄▅▆▇█：下方 1/8 到整格。
        case 0x2581...0x2588: rects = [part(0, 1, 1 - CGFloat(value - 0x2580) / 8, 1)]
        // ▉▊▋▌▍▎▏：左侧 7/8 到 1/8。
        case 0x2589...0x258F: rects = [part(0, CGFloat(0x2590 - value) / 8, 0, 1)]
        case 0x2590: rects = [part(0.5, 1, 0, 1)]
        case 0x2591...0x2593:
            rects = [part(0, 1, 0, 1)]
            alpha = CGFloat(value - 0x2590) / 4
        case 0x2594: rects = [part(0, 1, 0, 0.125)]
        case 0x2595: rects = [part(0.875, 1, 0, 1)]
        case 0x2596...0x259F:
            let mask = quadrants[Int(value - 0x2596)]
            rects = (0..<4).filter { mask >> $0 & 1 != 0 }.map { index in
                let x = CGFloat(index % 2) / 2
                let y = CGFloat(index / 2) / 2
                return part(x, x + 0.5, y, y + 0.5)
            }
        default:
            return nil
        }
        let path = CGMutablePath()
        path.addRects(rects)
        return (path, alpha)
    }
}
