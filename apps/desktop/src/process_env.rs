//! 启动时校正从拉起方继承的环境变量。宿主进程和各个终端都继承 app 的环境，这里补对了，终端里就和
//! Terminal.app 开的一样。

/// 在 `main` 最开头调用：要改环境变量，那时还没有别的线程。
pub fn fix() {
    use_login_shell();
    #[cfg(target_os = "macos")]
    ensure_lang();
}

/// 父进程是 launchd（从程序坞、Finder 或 `open` 打开，登录项，登录时自启的宿主）时照 Terminal.app 改用
/// 账户记录里的登录 shell。`open` 会把调用方的环境带给 app：在 chsh 之前起的终端里打开时带进来的是旧
/// shell，宿主和各个终端都继承它，在这些终端里再重启 app 又带下去，一直换不过来。别的进程拉起的
/// （从终端里直接运行的，app 拉起的宿主）沿用父进程给的 `SHELL`。
fn use_login_shell() {
    // SAFETY: getppid 总是成功，没有副作用。
    if unsafe { libc::getppid() } != 1 {
        return;
    }
    if let Some(shell) = login_shell() {
        // SAFETY: 由 `fix` 在 `main` 最开头调用，还没有别的线程读写环境变量。
        unsafe { std::env::set_var("SHELL", shell) };
    }
}

/// 账户记录（passwd）里的登录 shell，没有时为 `None`。
fn login_shell() -> Option<std::ffi::OsString> {
    use std::os::unix::ffi::OsStrExt as _;
    // SAFETY: `passwd` 只有整数和指针，全零是合法的值。
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0; 4096];
    let mut found = std::ptr::null_mut();
    // SAFETY: 只往传进去的 `entry` 和 `buf` 里写，`entry` 里的字符串都指向 `buf`。
    let code = unsafe { libc::getpwuid_r(libc::getuid(), &raw mut entry, buf.as_mut_ptr(), buf.len(), &raw mut found) };
    if code != 0 || found.is_null() || entry.pw_shell.is_null() {
        return None;
    }
    // SAFETY: 成功时 `pw_shell` 是 `buf` 里以 NUL 结尾的字符串，`buf` 还活着。
    let shell = unsafe { std::ffi::CStr::from_ptr(entry.pw_shell) }.to_bytes();
    (!shell.is_empty()).then(|| std::ffi::OsStr::from_bytes(shell).to_owned())
}

/// 从程序坞、Finder 打开的 app 环境里没有 `LANG`，终端里就成了 C 语言环境：zsh 把中文当成一个个字节，
/// 程序也不出译文。照 Terminal.app 按系统的语言和地区补上；已经设了语言环境变量时不动。
#[cfg(target_os = "macos")]
// 换掉 countryCode 的 regionCode 要 macOS 14 才有。
#[allow(deprecated)]
fn ensure_lang() {
    let set = |name| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    if ["LC_ALL", "LC_CTYPE", "LANG"].into_iter().any(set) {
        return;
    }
    let locale = objc2_foundation::NSLocale::currentLocale();
    let installed: Vec<String> = std::fs::read_dir("/usr/share/locale")
        .map(|dir| dir.flatten().filter_map(|entry| entry.file_name().into_string().ok()).collect())
        .unwrap_or_default();
    let lang = utf8_locale(
        &locale.languageCode().to_string(),
        locale.countryCode().map(|code| code.to_string()).as_deref(),
        locale.scriptCode().map(|code| code.to_string()).as_deref(),
        &installed,
    );
    // SAFETY: 由 `fix` 在 `main` 最开头调用，还没有别的线程读写环境变量。
    unsafe { std::env::set_var("LANG", lang) };
}

/// `language` 在本机装着的 UTF-8 语言环境里对应的那个：先找 `region` 的，再找这种语言的本土（中文按
/// `script` 分简繁），再找它随便哪个地区的，都没有时是 `en_US.UTF-8`。`zh`、`SG` 没有 `zh_SG.UTF-8`，
/// 得到 `zh_CN.UTF-8`，和 Terminal.app 一样。
fn utf8_locale(language: &str, region: Option<&str>, script: Option<&str>, installed: &[String]) -> String {
    let home = match language {
        "zh" if script == Some("Hant") => "TW".to_owned(),
        "zh" => "CN".to_owned(),
        "en" => "US".to_owned(),
        _ => language.to_uppercase(),
    };
    let prefix = format!("{language}_");
    region
        .into_iter()
        .chain([home.as_str()])
        .map(|region| format!("{language}_{region}.UTF-8"))
        .find(|name| installed.contains(name))
        .or_else(|| {
            installed.iter().filter(|name| name.starts_with(&prefix) && name.ends_with(".UTF-8")).min().cloned()
        })
        .unwrap_or_else(|| "en_US.UTF-8".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_an_installed_utf8_locale_for_the_system_language() {
        let installed: Vec<String> =
            ["de_AT.UTF-8", "de_DE.UTF-8", "en_US.UTF-8", "ja_JP.UTF-8", "zh_CN.UTF-8", "zh_HK.UTF-8", "zh_TW.UTF-8"]
                .map(str::to_owned)
                .into();
        let pick = |language, region, script| utf8_locale(language, region, script, &installed);
        assert_eq!(pick("zh", Some("SG"), None), "zh_CN.UTF-8");
        assert_eq!(pick("zh", Some("HK"), None), "zh_HK.UTF-8");
        assert_eq!(pick("zh", Some("SG"), Some("Hant")), "zh_TW.UTF-8");
        assert_eq!(pick("de", Some("CH"), None), "de_DE.UTF-8");
        assert_eq!(pick("de", Some("AT"), None), "de_AT.UTF-8");
        assert_eq!(pick("en", Some("SG"), None), "en_US.UTF-8");
        assert_eq!(pick("ja", None, None), "ja_JP.UTF-8");
        assert_eq!(pick("xx", Some("YY"), None), "en_US.UTF-8");
    }
}
