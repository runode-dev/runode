import Foundation
import Testing

@testable import RunodeFeatures

@Suite struct LocalizationTests {
    @Test func preferredLanguagesMapToTheSetting() {
        #expect(AppLocalization.language(forPreferred: "en") == .en)
        #expect(AppLocalization.language(forPreferred: "en-GB") == .en)
        #expect(AppLocalization.language(forPreferred: "zh-Hans") == .zhHans)
        #expect(AppLocalization.language(forPreferred: "zh-Hans-HK") == .zhHans)
        #expect(AppLocalization.language(forPreferred: "zh-CN") == .zhHans)
        #expect(AppLocalization.language(forPreferred: "zh") == .zhHans)
        #expect(AppLocalization.language(forPreferred: "zh-Hant") == .zhHant)
        #expect(AppLocalization.language(forPreferred: "zh-Hant-CN") == .zhHant)
        #expect(AppLocalization.language(forPreferred: "zh_TW") == .zhHant)
        #expect(AppLocalization.language(forPreferred: "zh-HK") == .zhHant)
        #expect(AppLocalization.language(forPreferred: "zh-MO") == .zhHant)
        // App 没有译文的语言算跟随系统。
        #expect(AppLocalization.language(forPreferred: "fr-FR") == .system)
        #expect(AppLocalization.language(forPreferred: "yue-Hant-HK") == .system)
    }

    @Test func storedLanguageRoundTrips() throws {
        let domain = URL.temporaryDirectory.appending(path: "runode-\(UUID().uuidString)").path
        let defaults = try #require(UserDefaults(suiteName: domain))
        defer { defaults.removePersistentDomain(forName: domain) }

        #expect(AppLocalization.stored(defaults: defaults, domain: domain) == .system)
        for language in [AppLanguage.en, .zhHant, .zhHans] {
            AppLocalization.store(language, defaults: defaults)
            #expect(defaults.persistentDomain(forName: domain)?["AppleLanguages"] as? [String] == [language.rawValue])
            #expect(AppLocalization.stored(defaults: defaults, domain: domain) == language)
        }
        AppLocalization.store(.system, defaults: defaults)
        #expect(defaults.persistentDomain(forName: domain)?["AppleLanguages"] == nil)
        #expect(AppLocalization.stored(defaults: defaults, domain: domain) == .system)
    }

    /// 别处（比如系统设置）写进来的 `AppleLanguages` 也认，只看第一项。
    @Test func storedLanguageReadsTheFirstPreference() throws {
        let domain = URL.temporaryDirectory.appending(path: "runode-\(UUID().uuidString)").path
        let defaults = try #require(UserDefaults(suiteName: domain))
        defer { defaults.removePersistentDomain(forName: domain) }

        defaults.set(["zh-Hant-TW", "en"], forKey: "AppleLanguages")
        #expect(AppLocalization.stored(defaults: defaults, domain: domain) == .zhHant)
        #expect(AppLocalization.stored(defaults: defaults, domain: nil) == .system)
    }

    @Test func relaunchIsNeededOnlyWhenTheInterfaceWouldChange() {
        func needs(_ language: AppLanguage, from launched: AppLanguage, running: String) -> Bool {
            AppLocalization.needsRelaunch(choosing: language, launchedWith: launched, running: running)
        }
        // 没改，或改回启动时的选择。
        #expect(!needs(.system, from: .system, running: "zh-Hans"))
        #expect(!needs(.en, from: .en, running: "en"))
        // 选的就是这次界面用的那套。
        #expect(!needs(.zhHans, from: .system, running: "zh-Hans"))
        #expect(needs(.en, from: .system, running: "zh-Hans"))
        #expect(needs(.zhHant, from: .en, running: "en"))
        // 改成跟随系统时读不到系统语言，总提示。
        #expect(needs(.system, from: .en, running: "en"))
    }
}
