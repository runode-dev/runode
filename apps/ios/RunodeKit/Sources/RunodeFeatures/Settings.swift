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
    /// agent 停下来等回答时，由电脑经推送在灵动岛和锁屏上提醒（Live Activity）。关掉时向各台电脑注销推送。
    public var alertsBlockedAgents = true

    public init() {}

    /// 手动字号能调的范围。
    public static let fontSizes: ClosedRange<Double> = 9...28

    // 新加的字段在旧的存档里没有：缺了就用默认值，不让整份设置读不出来。
    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        defaultSize = try container.decodeIfPresent(SizePreference.self, forKey: .defaultSize) ?? .automatic
        fontSize = try container.decodeIfPresent(Double.self, forKey: .fontSize)
        bellHaptics = try container.decodeIfPresent(Bool.self, forKey: .bellHaptics) ?? true
        deviceName = try container.decodeIfPresent(String.self, forKey: .deviceName)
        // 以前的版本在 App 里自己显示 agent 状态，开关叫 `showsAgentActivity`：关过的照旧关着。
        let legacy = try decoder.container(keyedBy: LegacyKeys.self)
        alertsBlockedAgents =
            try container.decodeIfPresent(Bool.self, forKey: .alertsBlockedAgents)
            ?? legacy.decodeIfPresent(Bool.self, forKey: .showsAgentActivity) ?? true
    }

    /// 旧存档里还有、现在不再写的键。
    private enum LegacyKeys: String, CodingKey {
        case showsAgentActivity
    }
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
