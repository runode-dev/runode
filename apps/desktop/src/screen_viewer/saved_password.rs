//! 记住的远端登录密码：远端锁屏时拿它自动解锁。按主机名各存一份，macOS 上存进钥匙串（通用密码，服务名
//! `SERVICE`），Windows 上存进凭据管理器（普通凭据，目标名是 `SERVICE/主机名`），别的系统不存。读写系统的密码库
//! 可能要等系统弹框问用户，在后台线程上调。

use std::io;

/// 钥匙串里的服务名、凭据管理器里目标名的前缀。
const SERVICE: &str = "dev.runode.screen-unlock";

/// `host` 记住的密码；没记过、读不到时为空。
pub fn load(host: &str) -> Option<String> {
    let result = platform::load(host);
    if let Err(err) = &result {
        tracing::warn!("cannot read the saved password for {host}: {err}");
    }
    result.ok().flatten()
}

/// 记住 `host` 的密码，记过的换成新的。
pub fn save(host: &str, password: &str) -> io::Result<()> {
    platform::save(host, password)
}

/// 忘掉 `host` 的密码；没记过时什么都不做。
pub fn forget(host: &str) -> io::Result<()> {
    platform::forget(host)
}

/// 记没记着 `host` 的密码，不读出密码本身（钥匙串不为这个弹框）。
pub fn exists(host: &str) -> bool {
    platform::exists(host)
}

#[cfg(target_os = "macos")]
mod platform {
    use std::{io, ptr};

    use objc2_core_foundation::{CFBoolean, CFData, CFDictionary, CFRetained, CFString, CFType};
    use objc2_security::{
        SecItemAdd, SecItemCopyMatching, SecItemDelete, SecItemUpdate, errSecDuplicateItem, errSecItemNotFound,
        kSecAttrAccount, kSecAttrLabel, kSecAttrService, kSecClass, kSecClassGenericPassword, kSecMatchLimit,
        kSecMatchLimitOne, kSecReturnAttributes, kSecReturnData, kSecValueData,
    };

    use super::SERVICE;

    /// 找 `host` 那一项的条件，另加 `extra` 里的键值。
    fn query(host: &str, extra: &[(&CFString, &CFType)]) -> CFRetained<CFDictionary<CFString, CFType>> {
        let service = CFString::from_str(SERVICE);
        let account = CFString::from_str(host);
        // SAFETY: 读 Security 框架导出的常量。
        let (class_key, class, service_key, account_key) =
            unsafe { (kSecClass, kSecClassGenericPassword, kSecAttrService, kSecAttrAccount) };
        let mut keys: Vec<&CFString> = vec![class_key, service_key, account_key];
        let mut values: Vec<&CFType> = vec![class.as_ref(), service.as_ref(), account.as_ref()];
        for (key, value) in extra {
            keys.push(key);
            values.push(value);
        }
        CFDictionary::from_slices(&keys, &values)
    }

    fn failed(what: &str, status: i32) -> io::Error {
        io::Error::other(format!("cannot {what} the keychain item ({status})"))
    }

    pub(super) fn load(host: &str) -> io::Result<Option<String>> {
        // SAFETY: 读 Security 框架导出的常量。
        let (return_data, limit, one) = unsafe { (kSecReturnData, kSecMatchLimit, kSecMatchLimitOne) };
        let query = query(host, &[(return_data, CFBoolean::new(true).as_ref()), (limit, one.as_ref())]);
        let mut result: *const CFType = ptr::null();
        // SAFETY: 条件里的键值都是 Security 要的类型；输出参数指向本地变量，拿到的按 Create 规则归这里。
        let status = unsafe { SecItemCopyMatching(query.as_opaque(), &mut result) };
        if status == errSecItemNotFound {
            return Ok(None);
        }
        if status != 0 || result.is_null() {
            return Err(failed("read", status));
        }
        // SAFETY: 要的是数据，返回的是 CFData，所有权交给 `data`。
        let data: CFRetained<CFData> = unsafe { CFRetained::from_raw(ptr::NonNull::from(&*result.cast::<CFData>())) };
        Ok(String::from_utf8(data.to_vec()).ok())
    }

    pub(super) fn save(host: &str, password: &str) -> io::Result<()> {
        let data = CFData::from_bytes(password.as_bytes());
        // SAFETY: 读 Security 框架导出的常量。
        let value_key = unsafe { kSecValueData };
        let update = CFDictionary::<CFString, CFType>::from_slices(&[value_key], &[data.as_ref()]);
        // SAFETY: 条件和要改的键值都是 Security 要的类型。
        let status = unsafe { SecItemUpdate(query(host, &[]).as_opaque(), update.as_opaque()) };
        if status == 0 {
            return Ok(());
        }
        if status != errSecItemNotFound {
            return Err(failed("update", status));
        }
        let label = CFString::from_str(&format!("runode remote desktop: {host}"));
        // SAFETY: 读 Security 框架导出的常量。
        let label_key = unsafe { kSecAttrLabel };
        let item = query(host, &[(value_key, data.as_ref()), (label_key, label.as_ref())]);
        // SAFETY: 新项的键值都是 Security 要的类型；不要返回值。
        match unsafe { SecItemAdd(item.as_opaque(), ptr::null_mut()) } {
            0 => Ok(()),
            // 别的线程刚加了同一项，再改一次。
            status if status == errSecDuplicateItem => {
                // SAFETY: 同上。
                match unsafe { SecItemUpdate(query(host, &[]).as_opaque(), update.as_opaque()) } {
                    0 => Ok(()),
                    status => Err(failed("update", status)),
                }
            }
            status => Err(failed("add", status)),
        }
    }

    pub(super) fn forget(host: &str) -> io::Result<()> {
        // SAFETY: 条件里的键值都是 Security 要的类型。
        match unsafe { SecItemDelete(query(host, &[]).as_opaque()) } {
            0 => Ok(()),
            status if status == errSecItemNotFound => Ok(()),
            status => Err(failed("delete", status)),
        }
    }

    pub(super) fn exists(host: &str) -> bool {
        // SAFETY: 读 Security 框架导出的常量。
        let (return_attributes, limit, one) = unsafe { (kSecReturnAttributes, kSecMatchLimit, kSecMatchLimitOne) };
        let query = query(host, &[(return_attributes, CFBoolean::new(true).as_ref()), (limit, one.as_ref())]);
        let mut result: *const CFType = ptr::null();
        // SAFETY: 同 `load`；拿到的属性字典马上放掉。
        let status = unsafe { SecItemCopyMatching(query.as_opaque(), &mut result) };
        if let Some(result) = ptr::NonNull::new(result.cast_mut()) {
            // SAFETY: Create 规则，归这里，放掉。
            drop(unsafe { CFRetained::from_raw(result) });
        }
        status == 0
    }
}

#[cfg(windows)]
mod platform {
    use std::{io, ptr};

    use windows_sys::Win32::{
        Foundation::{ERROR_NOT_FOUND, GetLastError},
        Security::Credentials::{
            CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW, CredWriteW,
        },
    };

    use super::SERVICE;

    fn target(host: &str) -> Vec<u16> {
        format!("{SERVICE}/{host}").encode_utf16().chain([0]).collect()
    }

    /// 读 `host` 的凭据，交给 `with`；没有时为空。
    fn read<T>(host: &str, with: impl FnOnce(&CREDENTIALW) -> T) -> io::Result<Option<T>> {
        let target = target(host);
        let mut credential: *mut CREDENTIALW = ptr::null_mut();
        // SAFETY: 目标名以 0 结尾；成功时拿到的凭据用完 `CredFree`。
        unsafe {
            if CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) == 0 {
                return match GetLastError() {
                    ERROR_NOT_FOUND => Ok(None),
                    _ => Err(io::Error::last_os_error()),
                };
            }
            let value = with(&*credential);
            CredFree(credential.cast());
            Ok(Some(value))
        }
    }

    pub(super) fn load(host: &str) -> io::Result<Option<String>> {
        let password = read(host, |credential| {
            // 内容是空的时候指针可能为空，别的程序也可能改过这一项。
            if credential.CredentialBlob.is_null() || credential.CredentialBlobSize == 0 {
                return None;
            }
            // SAFETY: 凭据的内容有 `CredentialBlobSize` 字节，`save` 存的是 UTF-8。
            let blob = unsafe {
                std::slice::from_raw_parts(credential.CredentialBlob, credential.CredentialBlobSize as usize)
            };
            String::from_utf8(blob.to_vec()).ok()
        })?;
        Ok(password.flatten())
    }

    pub(super) fn save(host: &str, password: &str) -> io::Result<()> {
        let mut target = target(host);
        let mut user: Vec<u16> = host.encode_utf16().chain([0]).collect();
        let mut blob = password.as_bytes().to_vec();
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: target.as_mut_ptr(),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            UserName: user.as_mut_ptr(),
            // SAFETY: 其余字段全零是合法的空值。
            ..unsafe { std::mem::zeroed() }
        };
        // SAFETY: 凭据里的指针在调用期间都有效。
        if unsafe { CredWriteW(&credential, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn forget(host: &str) -> io::Result<()> {
        let target = target(host);
        // SAFETY: 目标名以 0 结尾。
        if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
            // SAFETY: 紧接着失败的调用取错误码。
            if unsafe { GetLastError() } == ERROR_NOT_FOUND {
                return Ok(());
            }
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn exists(host: &str) -> bool {
        read(host, |_| ()).is_ok_and(|found| found.is_some())
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use std::io;

    pub(super) fn load(_: &str) -> io::Result<Option<String>> {
        Ok(None)
    }

    pub(super) fn save(_: &str, _: &str) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "no password store on this system"))
    }

    pub(super) fn forget(_: &str) -> io::Result<()> {
        Ok(())
    }

    pub(super) fn exists(_: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 存进系统的密码库、读回来、换掉、再忘掉。要读写本机的钥匙串或凭据管理器，所以 `#[ignore]`。
    #[test]
    #[ignore = "writes to the system password store"]
    fn saves_loads_and_forgets() {
        let host = format!("test-{}@runode.invalid", std::process::id());
        assert!(!exists(&host));
        assert_eq!(load(&host), None);
        save(&host, "first 密码").unwrap();
        assert!(exists(&host));
        assert_eq!(load(&host).as_deref(), Some("first 密码"));
        save(&host, "second").unwrap();
        assert_eq!(load(&host).as_deref(), Some("second"));
        forget(&host).unwrap();
        assert!(!exists(&host));
        forget(&host).unwrap();
    }
}
