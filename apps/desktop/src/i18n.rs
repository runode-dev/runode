//! 界面语言。翻译放在 locales 目录，每种语言一个 `<语言标签>.yml`，编译时嵌进二进制；
//! 加一种语言只要加一个文件，代码里不列语言。配置模板和动作说明的翻译在 `runode_config`
//! 自己的 locales 里，语言的解析和切换也在那边（见 `runode_config::i18n`），这里只读系统
//! 偏好的语言。两边的语言集合必须一致。

use runode_config::i18n::{FALLBACK, resolve};
pub use runode_config::i18n::{current, set};

/// 当前语言里 `key` 的翻译，菜单和设置窗口用。
pub fn tr(key: &str) -> String {
    rust_i18n::t!(key).into_owned()
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

    /// 界面的翻译和配置模板的翻译分在两处，语言要一样多，否则选了某种语言后有一半文字是英文。
    #[test]
    fn same_locales_as_the_config() {
        let ui: Vec<String> = rust_i18n::available_locales!().into_iter().map(|l| l.into_owned()).collect();
        let config: Vec<String> = runode_config::i18n::available().into_iter().map(|l| l.into_owned()).collect();
        assert_eq!(ui, config);
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
        assert!(english.len() > 80, "{}", english.len());
        for locale in rust_i18n::available_locales!() {
            let have = keys(&locale);
            let missing: Vec<_> = english.iter().filter(|k| !have.contains(k)).collect();
            assert!(missing.is_empty(), "{locale} misses {missing:?}");
            let extra: Vec<_> = have.iter().filter(|k| !english.contains(k)).collect();
            assert!(extra.is_empty(), "{locale} has keys English lacks: {extra:?}");
        }
    }
}
