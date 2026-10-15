//! 把系统的语言标签换成有翻译的语言。

use runode_config::i18n::resolve;

#[test]
fn resolves_language_tags() {
    let resolve = |tag| resolve(tag, &["en", "zh-Hans", "zh-Hant"]);
    assert_eq!(resolve("en-US").as_deref(), Some("en"));
    assert_eq!(resolve("en_GB.UTF-8").as_deref(), Some("en"));
    assert_eq!(resolve("zh-Hans-SG").as_deref(), Some("zh-Hans"));
    assert_eq!(resolve("zh_CN.UTF-8").as_deref(), Some("zh-Hans"));
    assert_eq!(resolve("zh").as_deref(), Some("zh-Hans"));
    assert_eq!(resolve("zh-hant").as_deref(), Some("zh-Hant"));
    assert_eq!(resolve("zh-Hant-HK").as_deref(), Some("zh-Hant"));
    assert_eq!(resolve("zh_TW").as_deref(), Some("zh-Hant"));
    assert_eq!(resolve("zh-HK").as_deref(), Some("zh-Hant"));
    assert_eq!(resolve("ja-JP"), None);
    assert_eq!(resolve(""), None);
}

/// 有地区的翻译时先用它，不补上文字去找别的。
#[test]
fn exact_regional_tags_win_over_the_likely_script() {
    assert_eq!(resolve("zh_TW.UTF-8", &["zh-TW", "zh-Hant"]).as_deref(), Some("zh-TW"));
    assert_eq!(resolve("zh-TW", &["en", "zh-TW"]).as_deref(), Some("zh-TW"));
    assert_eq!(resolve("zh-HK", &["zh-TW", "zh-Hant"]).as_deref(), Some("zh-Hant"));
}
