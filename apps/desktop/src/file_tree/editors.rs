//! 本机装没装文件树能拿来打开项目的编辑器：它们都经自己装时向系统登记的链接前缀（`vscode://` 这类）打开，
//! 所以看系统里有没有程序认这个前缀。编辑器总是在本机跑，项目在 SSH 主机上时也一样。问不出来时当作装着，
//! 菜单项照旧给出。

/// 系统里有程序认链接前缀 `scheme`（不带 `:`）。macOS 和 Windows 上当场问；Linux 上要起 xdg-mime，
/// 用上一次 `refresh` 问到的，还没问过时当作认。
pub(super) fn handles(scheme: &str) -> bool {
    imp::handles(scheme)
}

/// Linux 上在后台重新问一遍 `schemes`，给之后的 `handles` 用；别的系统上什么都不做。
pub(super) fn refresh(schemes: &'static [&'static str]) {
    imp::refresh(schemes);
}

#[cfg(target_os = "macos")]
mod imp {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSString, NSURL};

    pub(super) fn handles(scheme: &str) -> bool {
        let Some(url) = NSURL::URLWithString(&NSString::from_str(&format!("{scheme}:"))) else { return true };
        NSWorkspace::sharedWorkspace().URLForApplicationToOpenURL(&url).is_some()
    }

    pub(super) fn refresh(_: &'static [&'static str]) {}
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::{
        Foundation::S_OK,
        UI::Shell::{ASSOCF_INIT_IGNOREUNKNOWN, ASSOCF_IS_PROTOCOL, ASSOCSTR_COMMAND, AssocQueryStringW},
    };

    pub(super) fn handles(scheme: &str) -> bool {
        let name: Vec<u16> = scheme.encode_utf16().chain(std::iter::once(0)).collect();
        let mut command = [0u16; 2048];
        let mut len = command.len() as u32;
        // SAFETY: `name` 以 0 结尾；`command` 有 `len` 个 u16 的空间，调用只往里写这么多。
        let result = unsafe {
            AssocQueryStringW(
                ASSOCF_IS_PROTOCOL | ASSOCF_INIT_IGNOREUNKNOWN,
                ASSOCSTR_COMMAND,
                name.as_ptr(),
                std::ptr::null(),
                command.as_mut_ptr(),
                &mut len,
            )
        };
        result == S_OK
    }

    pub(super) fn refresh(_: &'static [&'static str]) {}
}

#[cfg(target_os = "linux")]
mod imp {
    use std::{
        collections::HashMap,
        process::{Command, Stdio},
        sync::{
            Mutex, PoisonError,
            atomic::{AtomicBool, Ordering},
        },
    };

    /// 上一次问到的各前缀认不认。
    static KNOWN: Mutex<Option<HashMap<&'static str, bool>>> = Mutex::new(None);
    /// 有一次 `refresh` 在跑。
    static RUNNING: AtomicBool = AtomicBool::new(false);

    pub(super) fn handles(scheme: &str) -> bool {
        let known = KNOWN.lock().unwrap_or_else(PoisonError::into_inner);
        known.as_ref().and_then(|known| known.get(scheme).copied()).unwrap_or(true)
    }

    pub(super) fn refresh(schemes: &'static [&'static str]) {
        if RUNNING.swap(true, Ordering::AcqRel) {
            return;
        }
        let spawned = std::thread::Builder::new().name("editor-probe".into()).spawn(move || {
            let answers: HashMap<_, _> = schemes.iter().filter_map(|&scheme| Some((scheme, query(scheme)?))).collect();
            KNOWN.lock().unwrap_or_else(PoisonError::into_inner).get_or_insert_default().extend(answers);
            RUNNING.store(false, Ordering::Release);
        });
        if spawned.is_err() {
            RUNNING.store(false, Ordering::Release);
        }
    }

    /// xdg-mime 说的 `scheme` 链接默认用哪个程序打开：有就是认；没装 xdg-mime、它出错时问不出来。
    fn query(scheme: &str) -> Option<bool> {
        let output = Command::new("xdg-mime")
            .args(["query", "default", &format!("x-scheme-handler/{scheme}")])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        output.status.success().then(|| !output.stdout.trim_ascii().is_empty())
    }
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
mod imp {
    pub(super) fn handles(_: &str) -> bool {
        true
    }

    pub(super) fn refresh(_: &'static [&'static str]) {}
}

#[cfg(all(test, any(target_os = "macos", windows)))]
mod tests {
    /// 浏览器总认 `https`，没人登记的前缀没人认。
    #[test]
    fn tells_registered_schemes_from_unknown_ones() {
        assert!(super::handles("https"));
        assert!(!super::handles("runode-no-such-scheme"));
    }
}
