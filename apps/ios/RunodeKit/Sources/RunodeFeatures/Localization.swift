import Foundation

/// 界面语言的设置。选的语言存在 App 自己的持久域里的 `AppleLanguages`，系统设置里给 Runode 单独选语言
/// 写的也是它；系统在 App 启动时按它挑本地化，所以改了要重新打开 App 才生效。
public enum AppLocalization {
    private static let languagesKey = "AppleLanguages"

    /// 设置里现在选的语言。只读 `domain` 这个持久域：启动参数里的 `-AppleLanguages` 和全局域的系统语言
    /// 都不算用户在 App 里选的。
    public static func stored(
        defaults: UserDefaults = .standard, domain: String? = Bundle.main.bundleIdentifier
    ) -> AppLanguage {
        guard let domain, let languages = defaults.persistentDomain(forName: domain)?[languagesKey] as? [String],
            let first = languages.first
        else { return .system }
        return language(forPreferred: first)
    }

    /// 存下设置里选的语言；跟随系统时删掉这一项。
    public static func store(_ language: AppLanguage, defaults: UserDefaults = .standard) {
        _ = launchLanguage
        if language == .system {
            defaults.removeObject(forKey: languagesKey)
        } else {
            defaults.set([language.rawValue], forKey: languagesKey)
        }
    }

    /// 这次启动界面实际用的本地化（`.lproj` 的名字）。
    public static var running: String {
        Bundle.main.preferredLocalizations.first ?? Bundle.main.developmentLocalization ?? "en"
    }

    /// 这次启动时设置里的语言，界面就是按它挑的本地化；`store` 第一次写之前取下。
    private static let launchLanguage = stored()

    /// 选了 `language` 后要重新打开 App 界面才换成它。
    public static func needsRelaunch(for language: AppLanguage) -> Bool {
        needsRelaunch(choosing: language, launchedWith: launchLanguage, running: running)
    }

    /// 改回启动时的选择不用重开；改成跟随系统时 App 读不到系统语言（`Locale.preferredLanguages` 带着这次启动时
    /// App 自己选的语言，沙盒里也读不到全局域），宁可提示；选了具体语言时看它是不是这次界面用的那套。
    static func needsRelaunch(choosing language: AppLanguage, launchedWith: AppLanguage, running: String) -> Bool {
        if language == launchedWith { return false }
        if language == .system { return true }
        return language.rawValue != running
    }

    /// `AppleLanguages` 的一项对应设置里的哪种语言；App 没有译文的语言算跟随系统。
    static func language(forPreferred identifier: String) -> AppLanguage {
        let parts = identifier.split(whereSeparator: { $0 == "-" || $0 == "_" }).map { $0.lowercased() }
        switch parts.first {
        case "en":
            return .en
        case "zh":
            if parts.contains("hant") { return .zhHant }
            if parts.contains("hans") { return .zhHans }
            return parts.contains { ["tw", "hk", "mo"].contains($0) } ? .zhHant : .zhHans
        default:
            return .system
        }
    }
}
