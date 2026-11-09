//! 用 Bonjour 公布远程访问的服务（`BONJOUR_SERVICE`），手机按存下的证书指纹找这台电脑现在的
//! 地址，见 `runode_protocol::remote`。macOS 上直接调 libSystem 里的 `DNSServiceRegister`（交给系统的
//! mDNSResponder 去应答）；别的系统上没有人人都装着的 mDNS 服务，用 mdns-sd 在进程里自己应答，地址
//! 变了跟着重新公布。
//!
//! 查找别的电脑（`browse`）所有系统上都用 mdns-sd：它和 mDNSResponder 一样带 `SO_REUSEPORT` 绑 5353，
//! 两边都收得到组播的应答，本机经 mDNSResponder 公布的也查得到。

use std::{
    collections::HashMap,
    io,
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

use mdns_sd::{ServiceDaemon, ServiceEvent};
use runode_protocol::remote::{BONJOUR_FINGERPRINT_KEY, BONJOUR_SERVICE, FINGERPRINT_LEN, decode_base64url};

use crate::client::Found;

/// 一次公布，丢掉时撤回。
pub(crate) struct Registration {
    /// 只为丢掉时撤回，见 `imp::ServiceRef`、`responder::Responder`。
    #[cfg(target_os = "macos")]
    _service: imp::ServiceRef,
    #[cfg(not(target_os = "macos"))]
    _responder: responder::Responder,
}

/// 以 `name` 为实例名公布端口 `port`，TXT 记录是 `txt` 里的各项（`key=value`）。`only` 有值时只在这个
/// 地址所在的网卡上公布：macOS 上那块网卡的各个地址都公布出去，别的系统上只公布这一个地址。
#[cfg(target_os = "macos")]
pub(crate) fn register(name: &str, port: u16, txt: &[(&str, &str)], only: Option<IpAddr>) -> io::Result<Registration> {
    imp::register(name, port, txt, only).map(|service| Registration { _service: service })
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn register(name: &str, port: u16, txt: &[(&str, &str)], only: Option<IpAddr>) -> io::Result<Registration> {
    responder::register(name, port, txt, only).map(|responder| Registration { _responder: responder })
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

/// 开着的查找，丢掉时停下 mdns-sd 的守护和读事件的线程；丢掉以后不再调 `changed`。
pub(crate) struct Browsing {
    daemon: ServiceDaemon,
    stopped: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Browsing {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        // 守护停下时关掉事件的通道，读事件的线程随之结束。
        let _ = self.daemon.shutdown();
        if let Some(thread) = self.thread.take()
            && thread.thread().id() != thread::current().id()
        {
            let _ = thread.join();
        }
    }
}

/// 查 `BONJOUR_SERVICE`，解析出来的列表变了时把当前完整的列表交给 `changed`（在读事件的线程里调）。
/// TXT 里没有能解成指纹的 `fp` 的不算。
pub(crate) fn browse(changed: impl Fn(Vec<Found>) + Send + 'static) -> io::Result<Browsing> {
    let daemon = ServiceDaemon::new().map_err(io::Error::other)?;
    let events = daemon.browse(&format!("{BONJOUR_SERVICE}.local.")).map_err(io::Error::other)?;
    let stopped = Arc::new(AtomicBool::new(false));
    let thread = {
        let stopped = stopped.clone();
        thread::Builder::new().name("remote-browse".into()).spawn(move || {
            let mut found: HashMap<String, Found> = HashMap::new();
            while let Ok(event) = events.recv() {
                let changed_now = match event {
                    ServiceEvent::ServiceResolved(service) => {
                        let fullname = service.get_fullname().to_owned();
                        match resolved(&service) {
                            Some(machine) => found.insert(fullname, machine.clone()) != Some(machine),
                            None => found.remove(&fullname).is_some(),
                        }
                    }
                    ServiceEvent::ServiceRemoved(_, fullname) => found.remove(&fullname).is_some(),
                    _ => false,
                };
                if stopped.load(Ordering::Relaxed) {
                    break;
                }
                if changed_now {
                    let mut list: Vec<Found> = found.values().cloned().collect();
                    list.sort_by(|a, b| (&a.name, a.port).cmp(&(&b.name, b.port)));
                    changed(list);
                }
            }
        })?
    };
    Ok(Browsing { daemon, stopped, thread: Some(thread) })
}

/// mdns-sd 解析出的一个服务。IPv6 的链路本地地址不要：`Found` 不带网卡，连不上。
fn resolved(service: &mdns_sd::ResolvedService) -> Option<Found> {
    let fingerprint = decode_base64url(service.get_property_val_str(BONJOUR_FINGERPRINT_KEY)?).ok()?;
    let fingerprint: [u8; FINGERPRINT_LEN] = fingerprint.try_into().ok()?;
    let mut addrs: Vec<IpAddr> = service
        .get_addresses()
        .iter()
        .map(mdns_sd::ScopedIp::to_ip_addr)
        .filter(|addr| !matches!(addr, IpAddr::V6(v6) if v6.is_unicast_link_local()))
        .collect();
    if addrs.is_empty() {
        return None;
    }
    // IPv4 在前：同一网段里它最常连得通。
    addrs.sort_by_key(|addr| (addr.is_ipv6(), *addr));
    let suffix = format!(".{BONJOUR_SERVICE}.local.");
    let fullname = service.get_fullname();
    let name = fullname.strip_suffix(&suffix).unwrap_or(fullname);
    Some(Found { name: unescape(name), fingerprint, addrs, port: service.get_port() })
}

/// 实例名里 DNS 转义的字符（`\.`、`\\`）还原。
fn unescape(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut chars = name.chars();
    while let Some(c) = chars.next() {
        let escaped = if c == '\\' { chars.next() } else { None };
        out.push(escaped.unwrap_or(c));
    }
    out
}

#[cfg(target_os = "macos")]
mod imp {
    use std::{
        ffi::{CString, c_char, c_void},
        io,
        net::IpAddr,
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

    pub(super) fn register(
        name: &str,
        port: u16,
        txt: &[(&str, &str)],
        only: Option<IpAddr>,
    ) -> io::Result<ServiceRef> {
        // 0 是所有网卡。
        let interface = match only {
            None => 0,
            Some(addr) => {
                let ifname = crate::addrs::interface_of(addr).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::AddrNotAvailable, format!("no interface has {addr}"))
                })?;
                let ifname = CString::new(ifname).map_err(io::Error::other)?;
                // SAFETY: 以 NUL 结尾的网卡名，调用期间活着。
                match unsafe { libc::if_nametoindex(ifname.as_ptr()) } {
                    0 => return Err(io::Error::last_os_error()),
                    index => index,
                }
            }
        };
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
                interface,
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
    use std::{io, net::IpAddr, time::Duration};

    use mdns_sd::{IfKind, ServiceDaemon, ServiceInfo};
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

    pub(super) fn register(name: &str, port: u16, txt: &[(&str, &str)], only: Option<IpAddr>) -> io::Result<Responder> {
        let daemon = ServiceDaemon::new().map_err(io::Error::other)?;
        let host = super::responder_host(&crate::addrs::host_name());
        let service = format!("{BONJOUR_SERVICE}.local.");
        let ip = only.map(|addr| addr.to_string()).unwrap_or_default();
        let info = ServiceInfo::new(&service, super::instance_name(name), &host, ip.as_str(), port, txt)
            .map_err(io::Error::other)?;
        let info = match only {
            // 地址留空、交给守护跟着网卡的地址填：换了地址、插拔网线后照样公布现在的地址。
            None => info.enable_addr_auto(),
            // 后加的规则盖过先加的：先关掉所有网卡，再只开这个地址所在的那块。
            Some(addr) => {
                daemon.disable_interface(IfKind::All).map_err(io::Error::other)?;
                daemon.enable_interface(IfKind::Addr(addr)).map_err(io::Error::other)?;
                info
            }
        };
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

    #[test]
    fn escaped_instance_names_are_restored() {
        assert_eq!(unescape(r"Ethan\.s Mac"), "Ethan.s Mac");
        assert_eq!(unescape(r"a\\b"), r"a\b");
        assert_eq!(unescape("李的电脑"), "李的电脑");
    }

    /// 本机公布的服务（macOS 上经 mDNSResponder）`browse` 查得到，指纹从 TXT 里解出来。要本机的网卡能收发
    /// 组播，手动跑：`cargo test -p runode-remote --lib bonjour -- --ignored`。
    #[test]
    #[ignore = "needs multicast on a real network interface"]
    fn browsing_finds_a_local_advertisement() {
        use std::{sync::mpsc, time::Duration};

        use runode_protocol::remote::{bonjour_txt, encode_base64url};

        let name = format!("runode-browse-{}", std::process::id());
        let fingerprint = [7u8; FINGERPRINT_LEN];
        let txt = bonjour_txt(&fingerprint);
        let txt: Vec<(&str, &str)> = txt.iter().map(|(key, value)| (*key, value.as_str())).collect();
        let _registration = register(&name, 47124, &txt, None).unwrap();
        let (tx, rx) = mpsc::channel();
        let _browsing = browse(move |list| {
            let _ = tx.send(list);
        })
        .unwrap();
        let found = loop {
            let list = rx.recv_timeout(Duration::from_secs(10)).expect("never found the advertisement");
            if let Some(found) = list.into_iter().find(|found| found.name == name) {
                break found;
            }
        };
        assert_eq!((found.port, found.fingerprint), (47124, fingerprint), "{}", encode_base64url(&fingerprint));
        assert!(!found.addrs.is_empty());
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
        let _registration = register(&name, 47123, &[("v", "1"), ("fp", "abcd")], None).unwrap();
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

    /// 只在一个地址上公布时，解析出来只有这个地址。要本机的网卡能收发组播，手动跑：
    /// `cargo test -p runode-remote --lib bonjour -- --ignored`。
    #[cfg(not(target_os = "macos"))]
    #[test]
    #[ignore = "needs multicast on a real network interface"]
    fn advertising_on_one_address_publishes_only_it() {
        use std::time::{Duration, Instant};

        use mdns_sd::{ServiceDaemon, ServiceEvent};
        use runode_protocol::remote::BONJOUR_SERVICE;

        let only = *crate::addrs::local_addresses().iter().find(|addr| addr.is_ipv4()).expect("no IPv4 address");
        let name = format!("runode-only-{}", std::process::id());
        let _registration = register(&name, 47125, &[("v", "1")], Some(only)).unwrap();
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
        let addrs: Vec<IpAddr> = resolved.get_addresses().iter().map(mdns_sd::ScopedIp::to_ip_addr).collect();
        assert_eq!(addrs, [only]);
        let _ = browser.shutdown();
    }
}
