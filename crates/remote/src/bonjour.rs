//! 用 Bonjour 公布远程访问的服务（`BONJOUR_SERVICE`），手机按存下的证书指纹找这台电脑现在的
//! 地址，见 `runode_protocol::remote`。macOS 上直接调 libSystem 里的 `DNSServiceRegister`（交给系统的
//! mDNSResponder 去应答）；别的系统上没有人人都装着的 mDNS 服务，用 mdns-sd 在进程里自己应答，地址
//! 变了跟着重新公布。

use std::io;

/// 一次公布，丢掉时撤回。
pub(crate) struct Registration {
    /// 只为丢掉时撤回，见 `imp::ServiceRef`、`responder::Responder`。
    #[cfg(target_os = "macos")]
    _service: imp::ServiceRef,
    #[cfg(not(target_os = "macos"))]
    _responder: responder::Responder,
}

/// 以 `name` 为实例名公布端口 `port`，TXT 记录是 `txt` 里的各项（`key=value`）。
#[cfg(target_os = "macos")]
pub(crate) fn register(name: &str, port: u16, txt: &[(&str, &str)]) -> io::Result<Registration> {
    imp::register(name, port, txt).map(|service| Registration { _service: service })
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn register(name: &str, port: u16, txt: &[(&str, &str)]) -> io::Result<Registration> {
    responder::register(name, port, txt).map(|responder| Registration { _responder: responder })
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

/// SRV 记录指向的主机名：`runode-<主机名的第一段>.local.`，只留字母、数字和 `-`。不用系统的主机名本身：
/// Avahi、Windows 自己的 mDNS 也在应答它，两边公布的地址有一点不同就算名字冲突，系统那边会改名。
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn responder_host(host: &str) -> String {
    let label: String = host
        .split('.')
        .next()
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let label = label.trim_matches('-');
    let label = if label.is_empty() { "host" } else { label };
    // 加上前缀共 63 字节以内。
    let label = &label[..label.len().min(63 - "runode-".len())];
    format!("runode-{}.local.", label.trim_end_matches('-'))
}

#[cfg(not(target_os = "macos"))]
mod responder {
    use std::{io, time::Duration};

    use mdns_sd::{ServiceDaemon, ServiceInfo};
    use runode_protocol::remote::BONJOUR_SERVICE;

    /// 撤回时等告别包发出去最多这么久，免得手机一直记着已经不在的服务。
    const GOODBYE_WAIT: Duration = Duration::from_millis(500);

    /// 在后台线程里应答 mDNS 的守护，丢掉时撤回公布、停下线程。
    pub(super) struct Responder {
        daemon: ServiceDaemon,
        fullname: String,
    }

    impl Drop for Responder {
        fn drop(&mut self) {
            if let Ok(done) = self.daemon.unregister(&self.fullname) {
                let _ = done.recv_timeout(GOODBYE_WAIT);
            }
            let _ = self.daemon.shutdown();
        }
    }

    pub(super) fn register(name: &str, port: u16, txt: &[(&str, &str)]) -> io::Result<Responder> {
        let daemon = ServiceDaemon::new().map_err(io::Error::other)?;
        let host = super::responder_host(&crate::addrs::host_name());
        let service = format!("{BONJOUR_SERVICE}.local.");
        // 地址留空、交给守护跟着网卡的地址填：换了地址、插拔网线后照样公布现在的地址。
        let info = ServiceInfo::new(&service, super::instance_name(name), &host, "", port, txt)
            .map_err(io::Error::other)?
            .enable_addr_auto();
        let fullname = info.get_fullname().to_owned();
        daemon.register(info).map_err(io::Error::other)?;
        Ok(Responder { daemon, fullname })
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

    #[test]
    fn the_responder_has_its_own_host_name() {
        assert_eq!(responder_host("debian"), "runode-debian.local.");
        assert_eq!(responder_host("My_PC.corp.example"), "runode-my-pc.local.");
        assert_eq!(responder_host("李的电脑"), "runode-host.local.");
        assert_eq!(responder_host(&"a".repeat(80)).len(), "runode-.local.".len() + 56);
    }

    /// 公布出去的服务另起一个 mDNS 客户端解析得到：实例名、端口和 TXT 都对。要本机的网卡能收发组播，
    /// CI 的容器里未必行，手动跑：`cargo test -p runode-remote --lib bonjour -- --ignored`。
    #[cfg(not(target_os = "macos"))]
    #[test]
    #[ignore = "needs multicast on a real network interface"]
    fn advertised_services_resolve() {
        use std::time::{Duration, Instant};

        use mdns_sd::{ServiceDaemon, ServiceEvent};
        use runode_protocol::remote::BONJOUR_SERVICE;

        let name = format!("runode-test-{}", std::process::id());
        let _registration = register(&name, 47123, &[("v", "1"), ("fp", "abcd")]).unwrap();
        let browser = ServiceDaemon::new().unwrap();
        let events = browser.browse(&format!("{BONJOUR_SERVICE}.local.")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let resolved = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match events.recv_timeout(left) {
                Ok(ServiceEvent::ServiceResolved(service)) if service.get_fullname().starts_with(&name) => {
                    break service;
                }
                Ok(_) => {}
                Err(err) => panic!("{name} never resolved: {err}"),
            }
        };
        assert_eq!(resolved.get_port(), 47123);
        assert_eq!(resolved.get_property_val_str("fp"), Some("abcd"));
        assert_eq!(resolved.get_property_val_str("v"), Some("1"));
        assert!(!resolved.get_addresses().is_empty());
        let _ = browser.shutdown();
    }
}
