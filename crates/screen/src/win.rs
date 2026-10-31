//! Windows 上的远程桌面：DXGI 桌面复制采集一块屏幕，D3D11 的视频处理器缩放、转成 NV12，Media
//! Foundation 调显卡的硬件 HEVC 编码器编码（`Capture`）；`SendInput` 注入键鼠（`Injector`）。不用授权，
//! 但普通权限的进程注入不了以管理员身份跑的窗口，也采不到、注入不了锁屏和 UAC 提示所在的安全桌面。
//! 坐标都按物理像素，进程要先设成按显示器感知缩放（`dpi_aware`）。

mod awake;
mod capture;
mod decoder;
mod display;
mod encoder;
mod input;

use std::{io, sync::Once};

use windows::Win32::{
    Media::MediaFoundation::{MF_VERSION, MFSTARTUP_FULL, MFStartup},
    System::{
        RemoteDesktop::{
            ProcessIdToSessionId, WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION, WTS_INFO_CLASS,
            WTS_SESSIONSTATE_LOCK, WTSFreeMemory, WTSGetActiveConsoleSessionId, WTSINFOEXW,
            WTSQuerySessionInformationW, WTSSessionInfoEx, WTSUserName,
        },
        Threading::GetCurrentProcessId,
    },
    UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
};

pub use awake::KeepAwake;
pub use capture::{Capture, Keyframes};
pub use decoder::{Decoded, Decoder};
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

/// 本进程在服务的会话（会话 0）里，比如经 ssh 起的：这里没有桌面，采不到屏幕，也注入不了输入。
pub fn in_service_session() -> bool {
    let mut session = 0;
    // SAFETY: 输出参数指向本地变量。
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) }.is_ok_and(|()| session == 0)
}

/// 显示在这台电脑屏幕上的那个会话（控制台会话）里登录着的用户名；没人登录时为空，查不到时也当没人登录、记一条日志。
pub fn console_user() -> Option<String> {
    // SAFETY: 没有参数。
    let session = unsafe { WTSGetActiveConsoleSessionId() };
    // 没有接着屏幕的会话（正在切换）时是这个值。
    if session == u32::MAX {
        return None;
    }
    let user = query_session(session, WTSUserName, |buffer, bytes| {
        let units: &[u16] =
            // SAFETY: `WTSUserName` 交回的是以 NUL 结尾的 UTF-16 字符串，`bytes` 是它连同 NUL 的字节数。
            unsafe { std::slice::from_raw_parts(buffer.cast::<u16>(), bytes as usize / 2) };
        String::from_utf16_lossy(units).trim_end_matches('\0').to_owned()
    });
    match user {
        Ok(user) => (!user.is_empty()).then_some(user),
        Err(err) => {
            tracing::warn!("cannot query who is signed in to console session {session}: {err}");
            None
        }
    }
}

/// 查会话 `session` 的 `class` 信息，在系统分配的缓冲上调 `read(缓冲, 字节数)`，读完放掉缓冲；查不到时交回原因。
fn query_session<T>(
    session: u32,
    class: WTS_INFO_CLASS,
    read: impl FnOnce(*const u8, u32) -> T,
) -> windows::core::Result<T> {
    let mut buffer = windows::core::PWSTR::null();
    let mut bytes = 0u32;
    // SAFETY: 输出参数指向本地变量；成功时缓冲由系统分配，下面读完用 `WTSFreeMemory` 放掉。
    let queried = unsafe {
        WTSQuerySessionInformationW(Some(WTS_CURRENT_SERVER_HANDLE), session, class, &mut buffer, &mut bytes)
    };
    queried?;
    if buffer.is_null() {
        return Err(windows::Win32::Foundation::E_POINTER.into());
    }
    let value = read(buffer.0.cast_const().cast(), bytes);
    // SAFETY: 缓冲是上面查询时系统分配的，只放一次。
    unsafe { WTSFreeMemory(buffer.0.cast()) };
    Ok(value)
}

pub(crate) fn screen_locked() -> bool {
    query_session(WTS_CURRENT_SESSION, WTSSessionInfoEx, |buffer, _| {
        // SAFETY: `WTSSessionInfoEx` 返回的缓冲开头是一个 `WTSINFOEXW`，第一级的数据在 `Level` 为 1 时有效。
        unsafe {
            let info = &*buffer.cast::<WTSINFOEXW>();
            info.Level == 1 && info.Data.WTSInfoExLevel1.SessionFlags == WTS_SESSIONSTATE_LOCK as i32
        }
    })
    .unwrap_or(false)
}

/// 启动 Media Foundation；进程里只启动一次，之后一直开着。
fn start_media_foundation() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: 进程里只启动一次。
        if let Err(err) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) } {
            tracing::warn!("cannot start Media Foundation: {err}");
        }
    });
}

fn other(err: windows::core::Error) -> io::Error {
    io::Error::other(err.to_string())
}
