//! 这台机器叫什么、有哪些地址：配对 URI、`RemoteChallenge` 和 Bonjour 公布时用。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// 给人看的主机名：macOS 上是「系统设置 › 通用 › 共享」里的电脑名（比如「Ethan 的 MacBook Pro」），
/// 取不到时（以及别的系统上）用系统的主机名去掉 `.local`。
pub fn host_name() -> String {
    #[cfg(target_os = "macos")]
    if let Some(name) = computer_name::get() {
        return name;
    }
    let name = system_host_name();
    let name = name.strip_suffix(".local").unwrap_or(&name).to_owned();
    if name.is_empty() { "runode".into() } else { name }
}

/// `gethostname`；取不到时为空。
#[cfg(unix)]
fn system_host_name() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: 缓冲是本地数组，长度如实给出；返回后按 NUL 截断。
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
    if !ok {
        return String::new();
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..len]).into_owned()
}

/// 「设置 › 系统 › 系统信息」里的设备名（DNS 主机名，大小写照用户起的）；取不到时为空。
#[cfg(windows)]
fn system_host_name() -> String {
    use windows_sys::Win32::System::SystemInformation::{ComputerNameDnsHostname, GetComputerNameExW};
    let mut buf = [0u16; 256];
    let mut len = buf.len() as u32;
    // SAFETY: 缓冲是本地数组，长度如实给出；成功时 `len` 是写进去的字数，不含结尾的 NUL。
    let ok = unsafe { GetComputerNameExW(ComputerNameDnsHostname, buf.as_mut_ptr(), &raw mut len) } != 0;
    if !ok {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..len as usize])
}

#[cfg(target_os = "macos")]
mod computer_name {
    use std::ffi::{c_char, c_void};

    type CFStringRef = *const c_void;
    type CFIndex = isize;
    const UTF8: u32 = 0x0800_0100;

    #[link(name = "SystemConfiguration", kind = "framework")]
    unsafe extern "C" {
        fn SCDynamicStoreCopyComputerName(store: *const c_void, encoding: *mut u32) -> CFStringRef;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringGetLength(string: CFStringRef) -> CFIndex;
        fn CFStringGetMaximumSizeForEncoding(length: CFIndex, encoding: u32) -> CFIndex;
        fn CFStringGetCString(string: CFStringRef, buffer: *mut c_char, size: CFIndex, encoding: u32) -> u8;
        fn CFRelease(object: *const c_void);
    }

    /// 电脑名；没设、取不到时为 `None`。
    pub(super) fn get() -> Option<String> {
        // SAFETY: 不传 store 时用一个临时的；编码的输出参数可以为空。返回的字符串归调用方，用完释放。
        let string = unsafe { SCDynamicStoreCopyComputerName(std::ptr::null(), std::ptr::null_mut()) };
        if string.is_null() {
            return None;
        }
        // SAFETY: `string` 是上面拿到的有效 CFString；缓冲按 UTF-8 的最大长度加结尾的 NUL 分配。
        let name = unsafe {
            let size = CFStringGetMaximumSizeForEncoding(CFStringGetLength(string), UTF8) + 1;
            let mut buf = vec![0u8; usize::try_from(size).unwrap_or(0)];
            let ok = CFStringGetCString(string, buf.as_mut_ptr().cast(), size, UTF8) != 0;
            CFRelease(string);
            ok.then(|| {
                let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                String::from_utf8_lossy(&buf[..len]).into_owned()
            })
        };
        name.filter(|name| !name.trim().is_empty())
    }
}

/// 这台机器现在所有开着的网卡上的地址，配对 URI 里给手机挨个试：去掉回环和链路本地的（换一个
/// 网络就到不了），IPv4 在前，各自按网卡的顺序，不重复。
pub fn local_addresses() -> Vec<IpAddr> {
    local_interfaces().into_iter().map(|(_, addr)| addr).collect()
}

/// 和 `local_addresses` 一样的地址，各带着所在网卡的名字（`en0`、`utun3`，Windows 上是「WLAN」
/// 这样的网络连接名），给人挑用哪个网络。
pub fn local_interfaces() -> Vec<(String, IpAddr)> {
    let mut addrs: Vec<(String, IpAddr)> = Vec::new();
    for (name, addr) in up_interfaces() {
        if reachable(addr) && !addrs.iter().any(|(_, seen)| *seen == addr) {
            addrs.push((name, addr));
        }
    }
    addrs.sort_by_key(|(_, addr)| addr.is_ipv6());
    addrs
}

/// 开着的、不是回环的网卡上的地址，按网卡的顺序。
#[cfg(unix)]
fn up_interfaces() -> Vec<(String, IpAddr)> {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: 输出参数指向本地变量；成功时返回的链表下面用完释放。
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        tracing::warn!("cannot list the network addresses: {}", std::io::Error::last_os_error());
        return Vec::new();
    }
    let mut addrs = Vec::new();
    let mut next = list;
    while !next.is_null() {
        // SAFETY: `next` 是 getifaddrs 给的链表里的一项，释放前一直有效。
        let entry = unsafe { &*next };
        next = entry.ifa_next;
        let flags = entry.ifa_flags;
        let up = flags & libc::IFF_UP as u32 != 0 && flags & libc::IFF_RUNNING as u32 != 0;
        if !up || flags & libc::IFF_LOOPBACK as u32 != 0 || entry.ifa_addr.is_null() {
            continue;
        }
        // SAFETY: `ifa_addr` 不为空，按它的地址族读成对应的结构。
        let addr = unsafe {
            match i32::from((*entry.ifa_addr).sa_family) {
                libc::AF_INET => {
                    let sin = &*entry.ifa_addr.cast::<libc::sockaddr_in>();
                    IpAddr::V4(Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)))
                }
                libc::AF_INET6 => {
                    let sin6 = &*entry.ifa_addr.cast::<libc::sockaddr_in6>();
                    IpAddr::V6(Ipv6Addr::from(sin6.sin6_addr.s6_addr))
                }
                _ => continue,
            }
        };
        // SAFETY: `ifa_name` 是 getifaddrs 给的以 NUL 结尾的网卡名，释放前一直有效。
        let name = unsafe { std::ffi::CStr::from_ptr(entry.ifa_name) }.to_string_lossy().into_owned();
        addrs.push((name, addr));
    }
    // SAFETY: 释放上面 getifaddrs 给的链表，之后不再用。
    unsafe { libc::freeifaddrs(list) };
    addrs
}

/// 开着的、不是回环的网卡上的地址，按网卡的顺序。
#[cfg(windows)]
fn up_interfaces() -> Vec<(String, IpAddr)> {
    use windows_sys::Win32::{
        Foundation::{ERROR_BUFFER_OVERFLOW, NO_ERROR},
        NetworkManagement::{
            IpHelper::{
                GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses,
                IF_TYPE_SOFTWARE_LOOPBACK, IP_ADAPTER_ADDRESSES_LH,
            },
            Ndis::IfOperStatusUp,
        },
        Networking::WinSock::{AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR_IN, SOCKADDR_IN6},
    };

    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size: u32 = 16 << 10;
    // 按 8 字节对齐分配：结构里有 64 位的字段。网卡在两次调用之间变多时再要一次。
    let mut buf: Vec<u64>;
    loop {
        buf = vec![0; (size as usize).div_ceil(8)];
        let first = buf.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        // SAFETY: 缓冲有 `size` 字节，可写；不够时函数只改 `size`。
        let result = unsafe { GetAdaptersAddresses(u32::from(AF_UNSPEC), flags, std::ptr::null(), first, &mut size) };
        match result {
            NO_ERROR => break,
            ERROR_BUFFER_OVERFLOW => continue,
            _ => {
                tracing::warn!(
                    "cannot list the network addresses: {}",
                    std::io::Error::from_raw_os_error(result as i32)
                );
                return Vec::new();
            }
        }
    }
    let mut addrs = Vec::new();
    let mut adapter = buf.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
    // SAFETY: 链表、名字和地址都在 `buf` 里，函数按文档填好；`buf` 在这期间一直活着。
    unsafe {
        while let Some(entry) = adapter.as_ref() {
            adapter = entry.Next;
            if entry.OperStatus != IfOperStatusUp || entry.IfType == IF_TYPE_SOFTWARE_LOOPBACK {
                continue;
            }
            let name_len = (0..).take_while(|&i| *entry.FriendlyName.add(i) != 0).count();
            let name = String::from_utf16_lossy(std::slice::from_raw_parts(entry.FriendlyName, name_len));
            let mut unicast = entry.FirstUnicastAddress;
            while let Some(address) = unicast.as_ref() {
                unicast = address.Next;
                let sockaddr = address.Address.lpSockaddr;
                let addr = match (*sockaddr).sa_family {
                    AF_INET => {
                        IpAddr::V4(Ipv4Addr::from(u32::from_be((*sockaddr.cast::<SOCKADDR_IN>()).sin_addr.S_un.S_addr)))
                    }
                    AF_INET6 => IpAddr::V6(Ipv6Addr::from((*sockaddr.cast::<SOCKADDR_IN6>()).sin6_addr.u.Byte)),
                    _ => continue,
                };
                addrs.push((name.clone(), addr));
            }
        }
    }
    addrs
}

/// 别的机器有可能连得到的地址：不是回环、链路本地、未指定或组播。
fn reachable(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => !(v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() || v4.is_multicast()),
        IpAddr::V6(v6) => !(v6.is_loopback() || v6.is_unicast_link_local() || v6.is_unspecified() || v6.is_multicast()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_addresses_other_machines_can_reach() {
        for addr in ["127.0.0.1", "169.254.3.4", "::1", "fe80::1", "0.0.0.0", "224.0.0.251"] {
            assert!(!reachable(addr.parse().unwrap()), "{addr}");
        }
        for addr in ["192.168.1.20", "100.101.102.103", "fd7a:115c:a1e0::1", "2001:db8::1"] {
            assert!(reachable(addr.parse().unwrap()), "{addr}");
        }
        let addrs = local_addresses();
        assert!(addrs.iter().all(|addr| reachable(*addr)), "{addrs:?}");
        assert!(local_interfaces().iter().all(|(name, _)| !name.is_empty()));
        assert!(!host_name().is_empty());
    }
}
