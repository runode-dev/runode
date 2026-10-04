//! 界面语言。翻译放在 locales 目录，每种语言一个 `<语言标签>.yml`，编译时嵌进二进制；
//! 加一种语言只要加一个文件，代码里不列语言。
//!
//! 默认跟随系统偏好的语言，没有对应翻译时用英文；配置里的 `language` 可以指定。
//! 界面文字用 `rust_i18n::t!` 按键名取，某种语言缺了某个键时取英文。

use std::borrow::Cow;

/// 没有对应翻译时用的语言，也是所有键都必须有的那份。
pub const FALLBACK: &str = "en";

/// 中文只写了地区、没写文字时按地区补上文字，再去找翻译：(语言, 地区, 文字)，
/// 地区为空的一项是其余地区的默认。
const LIKELY_SCRIPTS: &[(&str, &str, &str)] = &[
    ("zh", "tw", "hant"),
    ("zh", "hk", "hant"),
    ("zh", "mo", "hant"),
    ("zh", "", "hans"),
];

/// 有翻译的语言标签，按名字排序。
pub fn available() -> Vec<Cow<'static, str>> {
    rust_i18n::available_locales!()
}

/// 在有翻译的语言里找和这个标签最接近的。认 BCP 47 和 POSIX 写法，比如 `zh-Hans-CN`、
/// `zh_TW.UTF-8`、`en-US`；先按原样找，再逐级去掉末尾的子标签。找不到返回 `None`。
pub fn resolve(tag: &str) -> Option<String> {
    let tag = tag.split(['.', '@']).next().unwrap_or_default().replace('_', "-").to_ascii_lowercase();
    let mut tag = with_likely_script(&tag);
    let available = available();
    loop {
        if let Some(found) = available.iter().find(|l| l.eq_ignore_ascii_case(&tag)) {
            return Some(found.to_string());
        }
        tag.truncate(tag.rfind('-')?);
    }
}

fn with_likely_script(tag: &str) -> String {
    let parts: Vec<&str> = tag.split('-').filter(|p| !p.is_empty()).collect();
    let Some((&language, rest)) = parts.split_first() else {
        return tag.to_owned();
    };
    // 四个字母的子标签是文字，写了就不补。
    if rest.iter().any(|p| p.len() == 4 && p.bytes().all(|b| b.is_ascii_alphabetic())) {
        return tag.to_owned();
    }
    let region = rest.first().copied().unwrap_or_default();
    let script = LIKELY_SCRIPTS
        .iter()
        .find(|(l, r, _)| *l == language && *r == region)
        .or_else(|| LIKELY_SCRIPTS.iter().find(|(l, r, _)| *l == language && r.is_empty()))
        .map(|(_, _, script)| *script);
    match script {
        Some(script) => [language, script].into_iter().chain(rest.iter().copied()).collect::<Vec<_>>().join("-"),
        None => tag.to_owned(),
    }
}

/// 系统偏好的语言里第一个有翻译的，都没有时用英文。
pub fn system() -> String {
    preferred_languages().iter().find_map(|tag| resolve(tag)).unwrap_or_else(|| FALLBACK.to_owned())
}

/// 当前的界面语言。
pub fn current() -> String {
    rust_i18n::locale().to_string()
}

/// 换界面语言。由加载配置时调用，之后重画的界面和重设的菜单都用新语言。
pub fn set(locale: &str) {
    rust_i18n::set_locale(locale);
}

#[cfg(target_os = "macos")]
fn preferred_languages() -> Vec<String> {
    objc2_foundation::NSLocale::preferredLanguages().iter().map(|tag| tag.to_string()).collect()
}

/// 按 POSIX 的优先级读语言环境变量；`LANGUAGE` 可以是冒号分隔的多个语言。
#[cfg(not(target_os = "macos"))]
fn preferred_languages() -> Vec<String> {
    ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .flat_map(|value| value.split(':').map(str::to_owned).collect::<Vec<_>>())
        .filter(|tag| !tag.is_empty() && tag != "C" && tag != "POSIX")
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_language_tags() {
        let resolve = |tag| resolve(tag);
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

    /// 英文是兜底，其余每种语言都要有英文的全部键，新增文字时漏翻译会在这里失败。
    #[test]
    fn every_locale_has_every_key() {
        let backend = crate::_rust_i18n_backend();
        let keys = |locale: &str| -> Vec<String> {
            let mut keys: Vec<String> = backend
                .messages_for_locale(locale)
                .unwrap_or_default()
                .into_iter()
                .map(|(key, _)| key.into_owned())
                .collect();
            keys.sort();
            keys
        };
        let english = keys(FALLBACK);
        assert!(english.len() > 100, "{}", english.len());
        for locale in available() {
            let have = keys(&locale);
            let missing: Vec<_> = english.iter().filter(|k| !have.contains(k)).collect();
            assert!(missing.is_empty(), "{locale} misses {missing:?}");
            let extra: Vec<_> = have.iter().filter(|k| !english.contains(k)).collect();
            assert!(extra.is_empty(), "{locale} has keys English lacks: {extra:?}");
        }
    }
}
