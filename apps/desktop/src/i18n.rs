//! 界面语言。翻译放在 locales 目录，每种语言一个 `<语言标签>.yml`，编译时嵌进二进制；
//! 加一种语言只要加一个文件，代码里不列语言。语言标签怎么换成有翻译的一种在
//! `runode_config::i18n`，有哪些翻译由这里告诉它。配置模板只有英文，不经翻译。
//!
//! 当前语言是整个进程一份，文字用 `rust_i18n::t!` 按键名取，某种语言缺了某个键时取英文。

use runode_config::i18n::FALLBACK;

/// 有翻译的语言标签，按名字排序。
pub fn available() -> Vec<String> {
    rust_i18n::available_locales!().into_iter().map(|l| l.into_owned()).collect()
}

/// 有翻译的语言里和 `tag` 最接近的，没有时 `None`。
pub fn resolve(tag: &str) -> Option<String> {
    runode_config::i18n::resolve(tag, &available())
}

/// 配置项 `language` 里写的标签对应的语言：照 `resolve` 换成有翻译的那种（`zh_CN` 是
/// `zh-Hans`），没有翻译的用英文。设置页显示和选中当前语言也用它，和实际用的语言一致。
pub fn configured(tag: &str) -> String {
    resolve(tag).unwrap_or_else(|| FALLBACK.to_owned())
}

/// 配置项 `language` 定下的语言：没写时跟随系统；写了没有翻译的语言时用英文，
/// 并记一条警告，免得写错了不知道。
pub fn from_config(language: Option<&str>) -> String {
    match language {
        None => system(),
        Some(tag) => {
            let locale = configured(tag);
            if locale == FALLBACK && resolve(tag).is_none() {
                tracing::warn!("no translation for language {tag:?}, using {FALLBACK}");
            }
            locale
        }
    }
}

/// 当前的界面语言。
pub fn current() -> String {
    rust_i18n::locale().to_string()
}

/// 换界面语言。由加载配置时调用，之后重画的界面和重设的菜单都用新语言。
pub fn set(locale: &str) {
    rust_i18n::set_locale(locale);
}

/// 当前语言里 `key` 的翻译，菜单和设置页用。
pub fn tr(key: &str) -> String {
    rust_i18n::t!(key).into_owned()
}

/// 快捷键动作的说明：当前语言里 `action.<name>` 的翻译，英文和缺了翻译的用动作表里的英文。
/// 英文不放在 locales 里，免得和动作表写两遍。
pub fn describe(name: &str) -> String {
    crate::_rust_i18n_try_translate(&current(), format!("action.{name}"))
        .map(|text| text.into_owned())
        .or_else(|| runode_config::keybind::ACTIONS.iter().find(|a| a.name == name).map(|a| a.doc.to_owned()))
        .unwrap_or_else(|| name.to_owned())
}

/// 系统偏好的语言里第一个有翻译的，都没有时用英文。
pub fn system() -> String {
    preferred_languages().iter().find_map(|tag| resolve(tag)).unwrap_or_else(|| FALLBACK.to_owned())
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
    fn config_language_falls_back_to_english() {
        assert_eq!(from_config(Some("zh_CN.UTF-8")), "zh-Hans");
        assert_eq!(configured("zh_CN.UTF-8"), "zh-Hans");
        assert_eq!(from_config(Some("ja")), FALLBACK);
        assert_eq!(configured("ja"), FALLBACK);
        assert!(available().contains(&from_config(None)));
    }

    /// 英文是兜底，其余每种语言都要有英文的全部键，新增文字时漏翻译会在这里失败。动作的说明
    /// （`action.*`）英文在动作表里，locales 里只有别的语言的，另在 `every_action_is_translated` 核对。
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
        assert!(english.len() > 80, "{}", english.len());
        for locale in rust_i18n::available_locales!() {
            let have = keys(&locale);
            let missing: Vec<_> = english.iter().filter(|k| !have.contains(k)).collect();
            assert!(missing.is_empty(), "{locale} misses {missing:?}");
            let extra: Vec<_> = have.iter().filter(|k| !k.starts_with("action.") && !english.contains(k)).collect();
            assert!(extra.is_empty(), "{locale} has keys English lacks: {extra:?}");
        }
        assert!(english.iter().all(|k| !k.starts_with("action.")), "English action docs belong in ACTIONS");
    }

    /// 每种语言都给动作表里的每个动作翻译了说明，也没有动作表里没有的。
    #[test]
    fn every_action_is_translated() {
        let actions: Vec<String> =
            runode_config::keybind::ACTIONS.iter().map(|a| format!("action.{}", a.name)).collect();
        for locale in rust_i18n::available_locales!().into_iter().filter(|l| l != FALLBACK) {
            let mut have: Vec<String> = crate::_rust_i18n_backend()
                .messages_for_locale(&locale)
                .unwrap_or_default()
                .into_iter()
                .map(|(key, _)| key.into_owned())
                .filter(|k| k.starts_with("action."))
                .collect();
            have.sort();
            let mut want = actions.clone();
            want.sort();
            assert_eq!(have, want, "{locale}");
        }
    }
}
