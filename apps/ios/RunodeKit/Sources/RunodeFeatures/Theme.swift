import Foundation
import RunodeProtocol

/// 终端页以外的界面（首页、会话列表、设置、配对）用的配色，从电脑上终端的主题推出来，进出终端页时
/// 底色和明暗不跳。深色主题照系统深色模式的样子：页面铺终端的底色，卡片往前景色提亮一点；浅色主题
/// 照系统浅色模式：卡片是终端的底色，页面往前景色压暗一点。字、强调色仍用系统的，按 `isDark` 选
/// 深色或浅色模式。
public struct AppTheme: Hashable, Sendable, Codable {
    /// 终端的底色和前景色。
    public var background: Rgb
    public var foreground: Rgb

    public init(background: Rgb, foreground: Rgb) {
        self.background = background
        self.foreground = foreground
    }

    public init(_ settings: TermSettings) {
        self.init(background: settings.background, foreground: settings.foreground)
    }

    public var isDark: Bool { background.isDark }

    /// 页面的底色，相当于系统的分组列表底色。
    public var page: Rgb { isDark ? background : background.mixed(with: foreground, by: 0.05) }

    /// 卡片的底色，相当于系统分组列表里一行的底色。
    public var card: Rgb { isDark ? background.mixed(with: foreground, by: 0.08) : background }
}

extension Rgb {
    /// 按相对亮度看是深色还是浅色背景，决定上面的导航栏、底栏用深色还是浅色模式。
    public var isDark: Bool {
        let luminance = 0.2126 * Double(r) + 0.7152 * Double(g) + 0.0722 * Double(b)
        return luminance < 128
    }

    /// 往 `other` 靠 `amount`（0 到 1）的颜色。
    public func mixed(with other: Rgb, by amount: Double) -> Rgb {
        func channel(_ a: UInt8, _ b: UInt8) -> UInt8 {
            UInt8((Double(a) + (Double(b) - Double(a)) * amount).rounded())
        }
        return Rgb(channel(r, other.r), channel(g, other.g), channel(b, other.b))
    }
}
