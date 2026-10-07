//! 用 Bonjour 公布远程访问的服务（`BONJOUR_SERVICE`），手机按存下的证书指纹找这台 Mac 现在的
//! 地址，见 `runode_protocol::remote`。macOS 上直接调 libSystem 里的 `DNSServiceRegister`（交给系统的
//! mDNSResponder 去应答），不另带 mDNS 的实现；别的系统上什么都不做。

use std::io;

/// 一次公布，丢掉时撤回。
pub(crate) struct Registration {
    /// 只为丢掉时释放，见 `imp::ServiceRef`。
    #[cfg(target_os = "macos")]
    _service: imp::ServiceRef,
}

/// 以 `name` 为实例名公布端口 `port`，TXT 记录是 `txt` 里的各项（`key=value`）。
#[cfg(target_os = "macos")]
pub(crate) fn register(name: &str, port: u16, txt: &[(&str, &str)]) -> io::Result<Registration> {
    imp::register(name, port, txt).map(|service| Registration { _service: service })
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn register(_name: &str, _port: u16, _txt: &[(&str, &str)]) -> io::Result<Registration> {
    Ok(Registration {})
}

/// DNS-SD 的 TXT 记录：每项一个字节的长度再跟内容，一项最长 255 字节。
// 只有 macOS 用得上，别的系统上只给测试用。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn txt_record(items: &[(&str, &str)]) -> Vec<u8> {
    let mut record = Vec::new();
    for (key, value) in items {
        let item = format!("{key}={value}");
        let len = item.len().min(255);
        record.push(len as u8);
        record.extend_from_slice(&item.as_bytes()[..len]);
    }
    record
}

/// DNS 的一个标签最长 63 字节，实例名超过时在字符边界上截短。
// 只有 macOS 用得上，别的系统上只给测试用。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn instance_name(name: &str) -> &str {
    let mut end = name.len().min(63);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

#[cfg(target_os = "macos")]
mod imp {
    use std::{
        ffi::{CString, c_char, c_void},
        io,
    };

    use runode_protocol::remote::BONJOUR_SERVICE;

    type DnsServiceRef = *mut c_void;

    // dns_sd.h，在 libSystem 里，不用另外链接。
    unsafe extern "C" {
        fn DNSServiceRegister(
            sd_ref: *mut DnsServiceRef,
            flags: u32,
            interface_index: u32,
            name: *const c_char,
            regtype: *const c_char,
            domain: *const c_char,
            host: *const c_char,
            port: u16,
            txt_len: u16,
            txt_record: *const c_void,
            callback: *const c_void,
            context: *mut c_void,
        ) -> i32;
        fn DNSServiceRefDeallocate(sd_ref: DnsServiceRef);
    }

    /// `DNSServiceRegister` 给的引用，丢掉时释放，公布随之撤回。
    pub(super) struct ServiceRef(DnsServiceRef);

    // SAFETY: 这个引用只在丢掉时用一次，不会在两个线程里同时用。
    unsafe impl Send for ServiceRef {}

    impl Drop for ServiceRef {
        fn drop(&mut self) {
            // SAFETY: 由 `register` 拿到，只释放这一次。
            unsafe { DNSServiceRefDeallocate(self.0) };
        }
    }

    pub(super) fn register(name: &str, port: u16, txt: &[(&str, &str)]) -> io::Result<ServiceRef> {
        let name = CString::new(super::instance_name(name)).map_err(io::Error::other)?;
        let regtype = CString::new(BONJOUR_SERVICE).map_err(io::Error::other)?;
        let record = super::txt_record(txt);
        let txt_len = u16::try_from(record.len()).map_err(io::Error::other)?;
        let mut service: DnsServiceRef = std::ptr::null_mut();
        // SAFETY: 字符串都以 NUL 结尾，TXT 记录的长度如实给出，调用期间都活着；回调为空是 dns_sd.h
        // 明说可以的（不关心结果和之后的错误），之后也就不必处理这个引用上的回话。实例名重了时由系统
        // 自动改名（没给 `kDNSServiceFlagsNoAutoRename`）。
        let error = unsafe {
            DNSServiceRegister(
                &mut service,
                0,
                0,
                name.as_ptr(),
                regtype.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                port.to_be(),
                txt_len,
                record.as_ptr().cast(),
                std::ptr::null(),
                std::ptr::null_mut(),
            )
        };
        if error != 0 || service.is_null() {
            return Err(io::Error::other(format!("DNSServiceRegister failed with {error}")));
        }
        Ok(ServiceRef(service))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn txt_records_are_length_prefixed() {
        assert_eq!(txt_record(&[("v", "1"), ("fp", "ab")]), b"\x03v=1\x05fp=ab");
        assert_eq!(instance_name("短"), "短");
        let long = "长".repeat(30);
        assert_eq!(instance_name(&long).len(), 63);
    }
}
