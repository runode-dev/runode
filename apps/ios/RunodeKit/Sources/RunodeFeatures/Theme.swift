import Foundation
import RunodeProtocol

/// 终端页以外的界面（首页、会话列表、设置、配对）用的配色，从电脑上终端的主题推出来，进出终端页时
/// 底色和明暗不跳。深色主题照系统深色模式的样子：页面铺终端的底色，卡片往前景色提亮一点；浅色主题
/// 照系统浅色模式：卡片是终端的底色，页面往前景色压暗一点。字、强调色仍用系统的，按 `isDark` 选
/// 深色或浅色模式；状态的颜色取 `status`。
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
    public var card: Rgb { isDark ? background.mixed(with: foreground, by: 0.05) : background }

    /// 卡片里凸起的一块：按键、选项、标签。
    public var raised: Rgb { background.mixed(with: foreground, by: isDark ? 0.09 : 0.06) }

    /// 凹下去的底槽：终端摘录、输入框、分段控件的底。深色时比页面还暗，浅色时比卡片略灰。
    public var well: Rgb {
        isDark ? background.mixed(with: Rgb(0, 0, 0), by: 0.35) : background.mixed(with: foreground, by: 0.04)
    }

    /// 卡片的边和分隔线。
    public var hairline: Rgb { background.mixed(with: foreground, by: 0.12) }

    /// 比 `hairline` 显眼的线：次要按钮的边、发送键的底。
    public var strongHairline: Rgb { background.mixed(with: foreground, by: isDark ? 0.16 : 0.18) }

    /// 首页卡片底下露出来的那截后一张卡的边。
    public var stackEdge: Rgb { background.mixed(with: foreground, by: isDark ? 0.07 : 0.02) }

    /// 表示状态的几种颜色，按明暗选一组。
    public var status: StatusPalette { isDark ? .dark : .light }
}

/// 只用来表示状态的颜色：琥珀是等你回答，蓝是在干活，绿是成功，红是失败；界面上别的地方不用彩色。
/// 浅色那组的字色在浅色页面上要有 4.5:1 的对比度。
public struct StatusPalette: Hashable, Sendable {
    /// 等你回答的标记和字。
    public var waiting: Rgb
    /// 等你回答时实心按钮的底。
    public var waitingFill: Rgb
    /// `waitingFill` 上的字。
    public var onWaitingFill: Rgb
    public var working: Rgb
    public var success: Rgb
    public var failure: Rgb
    /// diff 里加的行。
    public var diffAdded: Rgb
    /// diff 里删的行。
    public var diffRemoved: Rgb

    public static let dark = StatusPalette(
        waiting: Rgb(hex: 0xF5A83C), waitingFill: Rgb(hex: 0xF5A83C), onWaitingFill: Rgb(hex: 0x1B1203),
        working: Rgb(hex: 0x7FA9FF), success: Rgb(hex: 0x7FD49B), failure: Rgb(hex: 0xF0958A),
        diffAdded: Rgb(hex: 0x9BE0B2), diffRemoved: Rgb(hex: 0xF0958A))

    public static let light = StatusPalette(
        waiting: Rgb(hex: 0x9C5B00), waitingFill: Rgb(hex: 0xF5A83C), onWaitingFill: Rgb(hex: 0x1B1203),
        working: Rgb(hex: 0x2F66D0), success: Rgb(hex: 0x197A42), failure: Rgb(hex: 0xC23A2E),
        diffAdded: Rgb(hex: 0x197A42), diffRemoved: Rgb(hex: 0xC23A2E))
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
