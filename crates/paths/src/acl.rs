//! Windows 上把私有目录的访问控制收紧：`create_private_dir` 建好目录后调 `restrict`。

use std::{
    ffi::c_void,
    io::{Error, ErrorKind, Result},
    os::windows::ffi::OsStrExt as _,
    path::Path,
    ptr,
};

use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, LocalFree, WIN32_ERROR},
    Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
        Authorization::{
            EXPLICIT_ACCESS_W, GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SET_ACCESS,
            SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
        },
        CreateWellKnownSid, DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation,
        GetSecurityDescriptorControl, GetTokenInformation, OWNER_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED, SECURITY_MAX_SID_SIZE,
        SUB_CONTAINERS_AND_OBJECTS_INHERIT, TOKEN_OWNER, TOKEN_QUERY, TOKEN_USER, TokenOwner, TokenUser,
        WELL_KNOWN_SID_TYPE, WinBuiltinAdministratorsSid, WinLocalSystemSid,
    },
    Storage::FileSystem::FILE_ALL_ACCESS,
    System::{
        SystemServices::{ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE},
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
};

/// 把 `dir` 的 DACL 换成受保护的一份：只有当前用户、SYSTEM 和 Administrators 完全控制，子项继承，不再继承
/// 上一级的。家目录被改到别处（比如 `D:\home`，`D:\` 根目录默认让所有登录用户都能改）时，继承来的权限会让
/// 别的用户读到私钥和配对口令。
///
/// 属主不是当前用户、它新建对象时的默认属主、SYSTEM 或 Administrators 组时拒绝，不去改：那多半是别人
/// 布下的。提权跑过一次的 runode 建的目录属主是 Administrators 组，之后不提权时照样要能用。DACL 已经受保护、允许访问的项都只给这三方时不再改，免得每次都把权限往下传一遍。
pub(crate) fn restrict(dir: &Path) -> Result<()> {
    let token = Token::current()?;
    let user = token.info::<TOKEN_USER>(TokenUser)?;
    let default_owner = token.info::<TOKEN_OWNER>(TokenOwner)?;
    // SAFETY: 两份信息都由 `GetTokenInformation` 填好，SID 指针指在各自的缓冲里，缓冲活到函数结束。
    let (user_sid, owner_sid) = unsafe { ((*user.as_ptr()).User.Sid, (*default_owner.as_ptr()).Owner) };

    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
    let (owner, dacl, descriptor) = security_info(&wide)?;
    let system = well_known(WinLocalSystemSid)?;
    let admins = well_known(WinBuiltinAdministratorsSid)?;
    let trusted = [user_sid, system.as_ptr().cast_mut().cast(), admins.as_ptr().cast_mut().cast()];
    // SAFETY: 属主指针指在 `descriptor` 里，它这时还活着；比较的 SID 也都活着。
    let ours = unsafe { trusted.iter().chain([&owner_sid]).any(|&sid| EqualSid(owner, sid) != 0) };
    if !ours {
        return Err(Error::new(ErrorKind::PermissionDenied, format!("{} belongs to another user", dir.display())));
    }
    let (mut control, mut revision) = (0u16, 0u32);
    // SAFETY: `descriptor` 由 `GetNamedSecurityInfoW` 给出。
    if unsafe { GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) } == 0 {
        return Err(Error::last_os_error());
    }
    // SAFETY: `dacl` 指在 `descriptor` 里，它这时还活着；三个 SID 也都活着。
    if control & SE_DACL_PROTECTED != 0 && unsafe { only_allows(dacl, &trusted) }? {
        return Ok(());
    }

    let grant = |sid: PSID| EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_ALL_ACCESS,
        grfAccessMode: SET_ACCESS,
        grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid.cast(),
        },
    };
    let entries = trusted.map(grant);
    let mut acl: *mut ACL = ptr::null_mut();
    // SAFETY: 三项都指向这时还活着的 SID；没有旧的 ACL 要并进来。
    check(unsafe { SetEntriesInAclW(entries.len() as u32, entries.as_ptr(), ptr::null(), &mut acl) })?;
    let acl = Local(acl.cast());
    // SAFETY: 路径以 NUL 结尾；只设 DACL，属主和组不动。
    check(unsafe {
        SetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            acl.0.cast(),
            ptr::null(),
        )
    })
}

/// `wide` 这个文件的属主、DACL 和装着它们的安全描述符（两个指针都指在描述符里，描述符丢掉时一起释放）。
fn security_info(wide: &[u16]) -> Result<(PSID, *mut ACL, Local)> {
    let mut owner: PSID = ptr::null_mut();
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: 路径以 NUL 结尾；不要的几项传空。
    check(unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut descriptor,
        )
    })?;
    Ok((owner, dacl, Local(descriptor)))
}

/// `dacl` 里允许访问的项是不是都只给 `trusted` 里的 SID。空 DACL 让所有人都能进，算不是；认不出的项也算不是。
///
/// # Safety
///
/// `dacl` 为空或指向一份有效的 ACL，`trusted` 里的指针都指向有效的 SID。
unsafe fn only_allows(dacl: *const ACL, trusted: &[PSID]) -> Result<bool> {
    if dacl.is_null() {
        return Ok(false);
    }
    let mut info = ACL_SIZE_INFORMATION { AceCount: 0, AclBytesInUse: 0, AclBytesFree: 0 };
    // SAFETY: 调用方保证 `dacl` 有效；输出指向本地变量，长度如实给出。
    let ok = unsafe {
        GetAclInformation(dacl, (&raw mut info).cast(), size_of::<ACL_SIZE_INFORMATION>() as u32, AclSizeInformation)
    };
    if ok == 0 {
        return Err(Error::last_os_error());
    }
    for index in 0..info.AceCount {
        let mut ace: *mut c_void = ptr::null_mut();
        // SAFETY: 下标在项数以内。
        if unsafe { GetAce(dacl, index, &mut ace) } == 0 {
            return Err(Error::last_os_error());
        }
        // SAFETY: 每一项都以 `ACE_HEADER` 开头。
        let kind = unsafe { (*ace.cast::<ACE_HEADER>()).AceType };
        if u32::from(kind) == ACCESS_DENIED_ACE_TYPE {
            continue;
        }
        if u32::from(kind) != ACCESS_ALLOWED_ACE_TYPE {
            return Ok(false);
        }
        // SAFETY: 这一项是 `ACCESS_ALLOWED_ACE`，SID 从 `SidStart` 起。
        let sid: PSID = unsafe { (&raw mut (*ace.cast::<ACCESS_ALLOWED_ACE>()).SidStart).cast() };
        // SAFETY: 两边都是有效的 SID。
        if !trusted.iter().any(|&ours| unsafe { EqualSid(sid, ours) } != 0) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// 系统内置的 SID（SYSTEM、Administrators）。
fn well_known(kind: WELL_KNOWN_SID_TYPE) -> Result<Vec<u8>> {
    let mut sid = vec![0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut size = sid.len() as u32;
    // SAFETY: 缓冲的长度如实给出，不需要域 SID。
    if unsafe { CreateWellKnownSid(kind, ptr::null_mut(), sid.as_mut_ptr().cast(), &mut size) } == 0 {
        return Err(Error::last_os_error());
    }
    sid.truncate(size as usize);
    Ok(sid)
}

fn check(code: WIN32_ERROR) -> Result<()> {
    if code == ERROR_SUCCESS { Ok(()) } else { Err(Error::from_raw_os_error(code as i32)) }
}

/// 系统用 `LocalAlloc` 分配、要调用方 `LocalFree` 的内存。
struct Local(*mut c_void);

impl Drop for Local {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: 指针由系统分配给我们，只释放这一次。
            unsafe { LocalFree(self.0) };
        }
    }
}

/// 当前进程的访问令牌，丢掉时关闭。
struct Token(HANDLE);

impl Token {
    fn current() -> Result<Self> {
        let mut handle: HANDLE = ptr::null_mut();
        // SAFETY: 当前进程的伪句柄不用关；拿到的令牌句柄由 `Token` 关。
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut handle) } == 0 {
            return Err(Error::last_os_error());
        }
        Ok(Self(handle))
    }

    /// 令牌的一项信息，放在按 `T` 对齐的缓冲里：里面的 SID 指针指向缓冲自己的后半段。
    fn info<T>(&self, class: i32) -> Result<Aligned<T>> {
        let mut size = 0u32;
        // SAFETY: 先问要多大的缓冲，这次调用按约定失败。
        unsafe { GetTokenInformation(self.0, class, ptr::null_mut(), 0, &mut size) };
        let words = (size as usize).div_ceil(size_of::<u64>()).max(1);
        let mut buffer = vec![0u64; words];
        // SAFETY: 缓冲至少 `size` 字节，按 8 字节对齐，够 `T` 用。
        if unsafe { GetTokenInformation(self.0, class, buffer.as_mut_ptr().cast(), size, &mut size) } == 0 {
            return Err(Error::last_os_error());
        }
        Ok(Aligned { buffer, _kind: std::marker::PhantomData })
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        // SAFETY: 句柄由 `OpenProcessToken` 给出，只关这一次。
        unsafe { CloseHandle(self.0) };
    }
}

/// 开头是一个 `T` 的缓冲。
struct Aligned<T> {
    buffer: Vec<u64>,
    _kind: std::marker::PhantomData<T>,
}

impl<T> Aligned<T> {
    fn as_ptr(&self) -> *const T {
        self.buffer.as_ptr().cast()
    }
}
