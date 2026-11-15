import Foundation
import Observation

/// 设置页里能改的东西，整份存下来。
public struct AppPreferences: Hashable, Sendable, Codable {
    /// 新打开的终端页用哪种尺寸方式；终端页上还能临时换。
    public var defaultSize: SizePreference = .automatic
    /// 终端的字号（点）；为空时跟随系统的动态字体。
    public var fontSize: Double?
    /// 终端里的程序响铃时震一下。
    public var bellHaptics = true
    /// 报给电脑的设备名；为空时用系统给的名字。
    public var deviceName: String?
    /// agent 停下来等回答时，由电脑经推送发通知提醒。关掉时向各台电脑注销推送。
    public var alertsBlockedAgents = true
    public var dictationSource: DictationSource = .phone
    /// 界面用哪套配色；终端本身总按各自会话的主题。
    public var appearance: AppAppearance = .computer

    public init() {}

    /// 手动字号能调的范围。
    public static let fontSizes: ClosedRange<Double> = 9...28

    // 字段缺了就用默认值，不让整份设置读不出来。
    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        defaultSize = try container.decodeIfPresent(SizePreference.self, forKey: .defaultSize) ?? .automatic
        fontSize = try container.decodeIfPresent(Double.self, forKey: .fontSize)
        bellHaptics = try container.decodeIfPresent(Bool.self, forKey: .bellHaptics) ?? true
        deviceName = try container.decodeIfPresent(String.self, forKey: .deviceName)
        alertsBlockedAgents = try container.decodeIfPresent(Bool.self, forKey: .alertsBlockedAgents) ?? true
        dictationSource = try container.decodeIfPresent(DictationSource.self, forKey: .dictationSource) ?? .phone
        appearance = try container.decodeIfPresent(AppAppearance.self, forKey: .appearance) ?? .computer
    }
}

/// 设置里的「主题」：界面用哪套配色，终端本身除外。
public enum AppAppearance: String, Hashable, Sendable, CaseIterable, Codable {
    /// 从电脑上终端的主题推出来（`AppModel.computerTheme`）。
    case computer
    /// 内置的浅色主题 `AppTheme.light`。
    case light
    /// 内置的深色主题 `AppTheme.dark`。
    case dark
    /// 系统是深色模式时用 `AppTheme.dark`，否则用 `AppTheme.light`。
    case system
}

/// 设置里的「语言」：界面文字用哪种语言，经 `AppLocalization` 存取，下次打开 App 时生效。
public enum AppLanguage: String, Hashable, Sendable, CaseIterable, Codable {
    /// 按系统的语言偏好挑 App 有译文的语言。
    case system
    case en
    case zhHans = "zh-Hans"
    case zhHant = "zh-Hant"
}

/// 设置页的视图模型：改了马上存；设备名变了告诉 `AppModel`，由它转给各台电脑的连接；等回答提醒的开关
/// 变了也告诉它，由它向各台电脑登记或注销推送。
@Observable
@MainActor
public final class SettingsModel {
    public var preferences: AppPreferences {
        didSet {
            guard preferences != oldValue else { return }
            store.save(preferences)
            if preferences.deviceName != oldValue.deviceName {
                deviceNameDidChange(deviceName)
            }
            if preferences.alertsBlockedAgents != oldValue.alertsBlockedAgents {
                alertsDidChange(preferences.alertsBlockedAgents)
            }
        }
    }

    /// 系统给的设备名，没自己起名字时用它。
    public let systemDeviceName: String
    @ObservationIgnored private let store: DefaultsStore<AppPreferences>
    @ObservationIgnored var deviceNameDidChange: @MainActor (String) -> Void = { _ in }
    @ObservationIgnored var alertsDidChange: @MainActor (Bool) -> Void = { _ in }

    public init(store: DefaultsStore<AppPreferences>, systemDeviceName: String) {
        self.store = store
        self.systemDeviceName = systemDeviceName
        preferences = store.load() ?? AppPreferences()
    }

    /// 报给电脑的设备名。
    public var deviceName: String {
        preferences.deviceName ?? systemDeviceName
    }

    /// 改设备名：去掉首尾空白，空的或者和系统名字一样就是不自己起名字。
    public func setDeviceName(_ name: String) {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        preferences.deviceName = trimmed.isEmpty || trimmed == systemDeviceName ? nil : trimmed
    }

    /// 手动字号；为空时跟随系统。超出范围的拉回范围里。
    public func setFontSize(_ size: Double?) {
        preferences.fontSize = size.map { min(max($0.rounded(), AppPreferences.fontSizes.lowerBound), AppPreferences.fontSizes.upperBound) }
    }
}
