//! macOS 和 Linux 上文件的访问控制表：覆盖写文件时把原文件的 ACL 搬到临时文件上（`copy_acl`）。

use std::{
    ffi::CString,
    fs::File,
    io::{Error, Result},
    os::{fd::AsRawFd as _, unix::ffi::OsStrExt as _},
    path::Path,
};

/// 把 `from` 的 ACL 设到已打开的 `to` 上。`from` 没有 ACL、所在的卷不支持或读不出来时什么都不做；读到了却设不上
/// 时报错，调用方不能把少了这些限制的文件换上去。Linux 上设 ACL 会按它的 mask 改写属组的权限位，所以在搬权限位
/// 之后调。经描述符设，ACL 不让属主写时已经打开的 `to` 照样写得进内容。
pub(crate) fn copy_acl(from: &Path, to: &File) -> Result<()> {
    let from = CString::new(from.as_os_str().as_bytes()).map_err(Error::other)?;
    platform::copy_acl(&from, to.as_raw_fd())
}

#[cfg(target_os = "macos")]
mod platform {
    use std::{
        ffi::{CStr, c_char, c_int, c_void},
        io::{Error, Result},
        os::fd::RawFd,
    };

    /// `acl_type_t` 的 `ACL_TYPE_EXTENDED`，macOS 上文件唯一的 ACL 类型。
    const ACL_TYPE_EXTENDED: c_int = 0x0000_0100;

    unsafe extern "C" {
        fn acl_get_file(path: *const c_char, kind: c_int) -> *mut c_void;
        fn acl_set_fd_np(fd: c_int, acl: *mut c_void, kind: c_int) -> c_int;
        fn acl_free(obj: *mut c_void) -> c_int;
    }

    /// `acl_get_file` 给出的 ACL，丢掉时交还。
    struct Acl(*mut c_void);

    impl Drop for Acl {
        fn drop(&mut self) {
            // SAFETY: 指针由 `acl_get_file` 给出、不为空，只在这里交还一次。
            unsafe { acl_free(self.0) };
        }
    }

    pub(super) fn copy_acl(from: &CStr, to: RawFd) -> Result<()> {
        // SAFETY: 路径以 NUL 结尾。
        let acl = unsafe { acl_get_file(from.as_ptr(), ACL_TYPE_EXTENDED) };
        if acl.is_null() {
            return Ok(());
        }
        let acl = Acl(acl);
        // SAFETY: `to` 是调用方开着的描述符，`acl` 还活着。
        if unsafe { acl_set_fd_np(to, acl.0, ACL_TYPE_EXTENDED) } != 0 {
            return Err(Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::{
        ffi::{CStr, c_char, c_int, c_void},
        io::{Error, Result},
        os::fd::RawFd,
    };

    /// 文件的 POSIX ACL 存在这个扩展属性里，内容是内核定的二进制格式，原样搬就行。
    const ACCESS_ACL: &CStr = c"system.posix_acl_access";

    unsafe extern "C" {
        fn getxattr(path: *const c_char, name: *const c_char, value: *mut c_void, size: usize) -> isize;
        fn fsetxattr(fd: c_int, name: *const c_char, value: *const c_void, size: usize, flags: c_int) -> c_int;
    }

    pub(super) fn copy_acl(from: &CStr, to: RawFd) -> Result<()> {
        let Some(acl) = read(from) else { return Ok(()) };
        // SAFETY: 名字以 NUL 结尾，`to` 是调用方开着的描述符，`acl` 有 `acl.len()` 个字节。
        if unsafe { fsetxattr(to, ACCESS_ACL.as_ptr(), acl.as_ptr().cast(), acl.len(), 0) } != 0 {
            return Err(Error::last_os_error());
        }
        Ok(())
    }

    /// `path` 的 ACL 属性的内容；没有、文件系统不支持或读不出时为空。先问长度再取，两次之间被改长了就重来。
    fn read(path: &CStr) -> Option<Vec<u8>> {
        loop {
            // SAFETY: 两个字符串都以 NUL 结尾；缓冲区传空、长度 0 时只问长度。
            let len = unsafe { getxattr(path.as_ptr(), ACCESS_ACL.as_ptr(), std::ptr::null_mut(), 0) };
            let len = usize::try_from(len).ok().filter(|&len| len > 0)?;
            let mut buf = vec![0u8; len];
            // SAFETY: 两个字符串都以 NUL 结尾，`buf` 有 `len` 个字节可写。
            let got = unsafe { getxattr(path.as_ptr(), ACCESS_ACL.as_ptr(), buf.as_mut_ptr().cast(), len) };
            match usize::try_from(got) {
                Ok(0) => return None,
                Ok(got) => {
                    buf.truncate(got);
                    return Some(buf);
                }
                Err(_) if Error::last_os_error().raw_os_error() == Some(ERANGE) => continue,
                Err(_) => return None,
            }
        }
    }

    /// errno 的 `ERANGE`：缓冲区比属性短。
    const ERANGE: c_int = 34;
}
