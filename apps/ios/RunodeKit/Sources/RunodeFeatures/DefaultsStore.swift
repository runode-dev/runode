import Foundation

/// 存成 JSON 记在 `UserDefaults` 里的一份值：上次打开的终端、设置、上次用的主题各一份，里面都没有秘密。
/// 读不出来（没存过、格式变了）时为空。
@MainActor
public final class DefaultsStore<Value: Codable> {
    private let defaults: UserDefaults
    private let key: String

    public init(_ key: String, defaults: UserDefaults = .standard) {
        self.key = key
        self.defaults = defaults
    }

    public func load() -> Value? {
        defaults.data(forKey: key).flatMap { try? JSONDecoder().decode(Value.self, from: $0) }
    }

    public func save(_ value: Value) {
        defaults.set(try? JSONEncoder().encode(value), forKey: key)
    }
}

extension UserDefaults {
    /// 一份新的、空的 `UserDefaults`，文件在临时目录下，不和 App 正式的混在一起；测试和演示模式用。
    /// suite 名字给成路径时存在那个路径上，不给路径会落到 `~/Library/Preferences` 里留下来。
    public static func temporary() -> UserDefaults {
        UserDefaults(suiteName: URL.temporaryDirectory.appending(path: "runode-\(UUID().uuidString)").path)!
    }
}
