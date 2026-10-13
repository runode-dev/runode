//! app 自己的登录项：`SMAppService.mainApp`（macOS 13 起有）。系统设置的「登录项」里列的是 Runode
//! 的名字和图标，用户在那里关掉也算没装。登记的是调用进程所在的 `.app`，所以只能在 app 里的可执行文件
//! 里调。

use std::io;

use objc2::runtime::AnyClass;
use objc2_service_management::{SMAppService, SMAppServiceStatus};

/// 系统有没有 SMAppService；没有时（macOS 12 及更早）退回写 LaunchAgent。
pub(crate) fn available() -> bool {
    AnyClass::get(c"SMAppService").is_some()
}

fn status() -> SMAppServiceStatus {
    // SAFETY: `available` 为真时才调，类和方法都在。
    unsafe { SMAppService::mainAppService().status() }
}

/// 登记了、用户也没在系统设置里关掉。
pub(crate) fn is_enabled() -> bool {
    status() == SMAppServiceStatus::Enabled
}

/// 登记过，不管用户关没关。
pub(crate) fn is_registered() -> bool {
    matches!(status(), SMAppServiceStatus::Enabled | SMAppServiceStatus::RequiresApproval)
}

pub(crate) fn register() -> io::Result<()> {
    // SAFETY: 同 `status`。
    let result = unsafe { SMAppService::mainAppService().registerAndReturnError() };
    // 用户在系统设置里关过它时，登记不报错或报错都有可能，状态停在要批准；只有用户自己能在那里打开。
    if status() == SMAppServiceStatus::RequiresApproval {
        // SAFETY: 同 `status`。
        unsafe { SMAppService::openSystemSettingsLoginItems() };
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "turn Runode on in System Settings > General > Login Items",
        ));
    }
    result.map_err(|err| io::Error::other(err.localizedDescription().to_string()))
}

pub(crate) fn unregister() -> io::Result<()> {
    // SAFETY: 同 `status`。
    unsafe { SMAppService::mainAppService().unregisterAndReturnError() }
        .map_err(|err| io::Error::other(err.localizedDescription().to_string()))
}
