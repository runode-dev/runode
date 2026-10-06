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
    }
}

/// 设置存在哪里。
@MainActor
public protocol PreferencesStore {
    func load() -> AppPreferences
    func save(_ preferences: AppPreferences)
}

/// 只记在内存里，测试和演示模式用。
@MainActor
public final class MemoryPreferencesStore: PreferencesStore {
    private var preferences: AppPreferences

    public init(_ preferences: AppPreferences = AppPreferences()) {
        self.preferences = preferences
    }

    public func load() -> AppPreferences { preferences }
    public func save(_ preferences: AppPreferences) { self.preferences = preferences }
}

/// 记在 `UserDefaults` 里。里面没有秘密。
@MainActor
public final class UserDefaultsPreferencesStore: PreferencesStore {
    private let defaults: UserDefaults
    private let key = "preferences"

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func load() -> AppPreferences {
        defaults.data(forKey: key).flatMap { try? JSONDecoder().decode(AppPreferences.self, from: $0) }
            ?? AppPreferences()
    }

    public func save(_ preferences: AppPreferences) {
        defaults.set(try? JSONEncoder().encode(preferences), forKey: key)
    }
}

/// 设置页的视图模型：改了马上存；设备名变了告诉 `AppModel`，由它转给各台电脑的连接。
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
        }
    }

    /// 系统给的设备名，没自己起名字时用它。
    public let systemDeviceName: String
    @ObservationIgnored private let store: any PreferencesStore
    @ObservationIgnored var deviceNameDidChange: @MainActor (String) -> Void = { _ in }

    public init(store: any PreferencesStore, systemDeviceName: String) {
        self.store = store
        self.systemDeviceName = systemDeviceName
        preferences = store.load()
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
