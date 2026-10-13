//! 界面语言：把系统或配置里的语言标签换成有翻译的一种。有哪些翻译只有界面那边（桌面 app 的
//! locales）知道，由调用方传进来；配置项 `language` 只照抄，不在这里校验。

/// 没有对应翻译时用的语言，也是所有键都必须有的那份。
pub const FALLBACK: &str = "en";

/// 中文只写了地区、没写文字时按地区补上文字，再去找翻译：(语言, 地区, 文字)，
/// 地区为空的一项是其余地区的默认。
const LIKELY_SCRIPTS: &[(&str, &str, &str)] =
    &[("zh", "tw", "hant"), ("zh", "hk", "hant"), ("zh", "mo", "hant"), ("zh", "", "hans")];

/// 在有翻译的语言 `available` 里找和这个标签最接近的。认 BCP 47 和 POSIX 写法，比如
/// `zh-Hans-CN`、`zh_TW.UTF-8`、`en-US`；先按原样找，再逐级去掉末尾的子标签。找不到返回 `None`。
pub fn resolve(tag: &str, available: &[impl AsRef<str>]) -> Option<String> {
    let tag = tag.split(['.', '@']).next().unwrap_or_default().replace('_', "-").to_ascii_lowercase();
    let mut tag = with_likely_script(&tag);
    loop {
        if let Some(found) = available.iter().map(AsRef::as_ref).find(|l| l.eq_ignore_ascii_case(&tag)) {
            return Some(found.to_owned());
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
