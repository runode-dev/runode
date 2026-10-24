//! Windows 上的远程桌面：DXGI 桌面复制采集一块屏幕，D3D11 的视频处理器缩放、转成 NV12，Media
//! Foundation 调显卡的硬件 HEVC 编码器编码（`Capture`）；`SendInput` 注入键鼠（`Injector`）。不用授权，
//! 但普通权限的进程注入不了以管理员身份跑的窗口，也采不到、注入不了锁屏和 UAC 提示所在的安全桌面。
//! 坐标都按物理像素，进程要先设成按显示器感知缩放（`dpi_aware`）。

mod awake;
mod capture;
mod display;
mod encoder;
mod input;

use std::{io, sync::Once};

use windows::Win32::{
    System::RemoteDesktop::{
        WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION, WTS_SESSIONSTATE_LOCK, WTSFreeMemory, WTSINFOEXW,
        WTSQuerySessionInformationW, WTSSessionInfoEx,
    },
    UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
};

pub use awake::KeepAwake;
pub use capture::{Capture, Keyframes};
pub(crate) use display::displays;
pub use input::Injector;

/// 把进程设成按显示器感知缩放，屏幕的坐标和尺寸都按物理像素报；设过了（比如界面进程里）不再设。
fn dpi_aware() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: 只改本进程的设置。
        if let Err(err) = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) } {
            tracing::debug!("cannot make the process per-monitor DPI aware: {err}");
        }
    });
}

pub(crate) fn screen_locked() -> bool {
    let mut buffer = windows::core::PWSTR::null();
    let mut bytes = 0u32;
    // SAFETY: 输出参数指向本地变量；成功时缓冲由系统分配，下面读完用 `WTSFreeMemory` 放掉。
    let queried = unsafe {
        WTSQuerySessionInformationW(
            Some(WTS_CURRENT_SERVER_HANDLE),
            WTS_CURRENT_SESSION,
            WTSSessionInfoEx,
            &mut buffer,
            &mut bytes,
        )
    };
    if queried.is_err() || buffer.is_null() {
        return false;
    }
    // SAFETY: `WTSSessionInfoEx` 返回的缓冲开头是一个 `WTSINFOEXW`，第一级的数据在 `Level` 为 1 时有效。
    let locked = unsafe {
        let info = &*buffer.0.cast::<WTSINFOEXW>();
        info.Level == 1 && info.Data.WTSInfoExLevel1.SessionFlags == WTS_SESSIONSTATE_LOCK as i32
    };
    // SAFETY: 缓冲是上面查询时系统分配的，只放一次。
    unsafe { WTSFreeMemory(buffer.0.cast()) };
    locked
}

fn other(err: windows::core::Error) -> io::Error {
    io::Error::other(err.to_string())
}
