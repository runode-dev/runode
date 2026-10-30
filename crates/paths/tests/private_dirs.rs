//! Windows 上的私有目录（宿主的运行目录、远程访问的目录）只让自己、SYSTEM 和管理员进，不继承上一级的权限。

#![cfg(windows)]

use std::{
    ffi::OsString,
    os::windows::ffi::OsStrExt as _,
    path::{Path, PathBuf},
    process::Command,
    ptr,
};

use runode_paths::Dirs;
use windows_sys::Win32::{
    Foundation::{ERROR_SUCCESS, LocalFree},
    Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
        Authorization::{ConvertSidToStringSidW, GetNamedSecurityInfoW, SE_FILE_OBJECT},
        DACL_SECURITY_INFORMATION, GetAce, GetAclInformation, GetSecurityDescriptorControl, PSECURITY_DESCRIPTOR,
        SE_DACL_PROTECTED,
    },
};

const EVERYONE: &str = "S-1-1-0";
const SYSTEM: &str = "S-1-5-18";
const ADMINISTRATORS: &str = "S-1-5-32-544";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("runode-acl-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn dirs(home: &Path) -> Dirs {
    let home = OsString::from(home);
    Dirs::from_vars(|key| (key == "USERPROFILE").then(|| home.clone()))
}

/// `path` 的 DACL 是不是受保护的，和其中各项的 SID。
fn dacl(path: &Path) -> (bool, Vec<String>) {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut acl: *mut ACL = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: 路径以 NUL 结尾，只要 DACL；描述符最后释放。
    let code = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut acl,
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    assert_eq!(code, ERROR_SUCCESS, "{}", path.display());
    let (mut control, mut revision) = (0u16, 0u32);
    let mut info = ACL_SIZE_INFORMATION { AceCount: 0, AclBytesInUse: 0, AclBytesFree: 0 };
    let mut sids = Vec::new();
    // SAFETY: `acl` 指在 `descriptor` 里；每一项都是允许访问的项（`restrict` 只写这一种，继承来的也是）。
    unsafe {
        assert_ne!(GetSecurityDescriptorControl(descriptor, &mut control, &mut revision), 0);
        assert_ne!(
            GetAclInformation(
                acl,
                (&raw mut info).cast(),
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation
            ),
            0
        );
        for index in 0..info.AceCount {
            let mut ace = ptr::null_mut();
            assert_ne!(GetAce(acl, index, &mut ace), 0);
            let sid = (&raw mut (*ace.cast::<ACCESS_ALLOWED_ACE>()).SidStart).cast();
            let mut text = ptr::null_mut();
            assert_ne!(ConvertSidToStringSidW(sid, &mut text), 0);
            let len = (0..).take_while(|&i| *text.add(i) != 0).count();
            sids.push(String::from_utf16_lossy(std::slice::from_raw_parts(text, len)));
            LocalFree(text.cast());
        }
        LocalFree(descriptor);
    }
    (control & SE_DACL_PROTECTED != 0, sids)
}

/// 上一级让所有人都能读（家目录被改到 `D:\home` 这类地方时就是这样）：建出来的私有目录不继承这一项，
/// 只有本人、SYSTEM 和管理员三项，里面新建的文件也一样。
#[test]
fn private_dirs_do_not_inherit_wider_access() {
    let home = scratch("inherit");
    let data = home.join(".runode");
    std::fs::create_dir_all(&data).unwrap();
    let granted = Command::new("icacls").arg(&data).args(["/grant", "*S-1-1-0:(OI)(CI)R"]).output().unwrap();
    assert!(granted.status.success(), "{granted:?}");
    assert!(dacl(&data).1.iter().any(|sid| sid == EVERYONE), "the parent should let everyone read");

    let dirs = dirs(&home);
    for dir in [dirs.create_runtime_dir().unwrap(), dirs.create_remote_access_dir().unwrap()] {
        let (protected, sids) = dacl(&dir);
        assert!(protected, "{}", dir.display());
        assert_eq!(sids.len(), 3, "{}: {sids:?}", dir.display());
        assert!(sids.iter().any(|sid| sid == SYSTEM) && sids.iter().any(|sid| sid == ADMINISTRATORS), "{sids:?}");
        assert!(!sids.iter().any(|sid| sid == EVERYONE), "{sids:?}");
        let file = dir.join("secret");
        std::fs::write(&file, "x").unwrap();
        assert!(!dacl(&file).1.iter().any(|sid| sid == EVERYONE), "{}", file.display());
    }
    // 再建一次是空操作。
    assert_eq!(dirs.create_runtime_dir().unwrap(), dirs.runtime_dir().unwrap());
    let _ = std::fs::remove_dir_all(&home);
}

/// 目录的 DACL 已经受保护、却明确让所有人都能读：照样换成只有三项的那份。
#[test]
fn a_protected_dacl_that_grants_everyone_is_replaced() {
    let home = scratch("protected");
    let dirs = dirs(&home);
    let runtime = dirs.runtime_dir().unwrap();
    std::fs::create_dir_all(&runtime).unwrap();
    let granted = Command::new("icacls")
        .arg(&runtime)
        .args(["/inheritance:r", "/grant", "*S-1-1-0:(OI)(CI)R", "/grant", "*S-1-5-18:(OI)(CI)F"])
        .output()
        .unwrap();
    assert!(granted.status.success(), "{granted:?}");
    let (protected, sids) = dacl(&runtime);
    assert!(protected && sids.iter().any(|sid| sid == EVERYONE), "{sids:?}");

    dirs.create_runtime_dir().unwrap();
    let (protected, sids) = dacl(&runtime);
    assert!(protected, "{}", runtime.display());
    assert_eq!(sids.len(), 3, "{sids:?}");
    assert!(!sids.iter().any(|sid| sid == EVERYONE), "{sids:?}");
    let _ = std::fs::remove_dir_all(&home);
}
