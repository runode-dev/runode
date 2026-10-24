//! 列出接在桌面上的屏幕：DXGI 按显卡和它的输出枚举，`Display::id` 是两者的序号拼成的数，同一套硬件
//! 上不变。

use std::io;

use windows::{
    Win32::{
        Foundation::RECT,
        Graphics::{
            Dxgi::{CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1},
            Gdi::{GetMonitorInfoW, MONITORINFO},
        },
        UI::{
            HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            WindowsAndMessaging::{GetSystemMetrics, MONITORINFOF_PRIMARY, SM_CXSCREEN, SM_CYSCREEN},
        },
    },
    core::Interface as _,
};

use crate::Display;

/// 系统缩放 100% 时的 DPI。
const BASE_DPI: u32 = 96;

/// 一块屏幕和采集、注入它要用的东西。
pub(crate) struct Output {
    pub(crate) display: Display,
    pub(crate) adapter: IDXGIAdapter1,
    pub(crate) output: IDXGIOutput1,
    /// 在整个虚拟桌面里的位置，物理像素。
    pub(crate) rect: RECT,
}

pub(crate) fn displays() -> Vec<Display> {
    let mut displays: Vec<Display> = outputs().into_iter().map(|output| output.display).collect();
    if displays.is_empty() {
        // SAFETY: 只读系统指标。
        let (width, height) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
        let (width, height) = (width.max(1) as u32, height.max(1) as u32);
        displays.push(Display { id: 0, width, height, pixel_width: width, pixel_height: height });
    }
    displays
}

/// 编号是 `id` 的屏幕。
pub(crate) fn find(id: u32) -> io::Result<Output> {
    outputs()
        .into_iter()
        .find(|output| output.display.id == id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no display {id}")))
}

/// 接在桌面上的各块屏幕，主屏排第一。
pub(crate) fn outputs() -> Vec<Output> {
    super::dpi_aware();
    // SAFETY: 建 DXGI 工厂，失败时返回错误。
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else { return Vec::new() };
    let mut outputs = Vec::new();
    for adapter_index in 0.. {
        // SAFETY: 序号越界时返回 `DXGI_ERROR_NOT_FOUND`，到那里为止。
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(adapter_index) }) else { break };
        for output_index in 0.. {
            // SAFETY: 同上。
            let Ok(output) = (unsafe { adapter.EnumOutputs(output_index) }) else { break };
            let Ok(output) = output.cast::<IDXGIOutput1>() else { continue };
            // SAFETY: 读输出的描述。
            let Ok(desc) = (unsafe { output.GetDesc() }) else { continue };
            if !desc.AttachedToDesktop.as_bool() {
                continue;
            }
            let rect = desc.DesktopCoordinates;
            // 复制模式下几个输出显示的是同一块桌面，坐标一样，只列一次。
            if outputs.iter().any(|seen: &Output| seen.rect == rect) {
                continue;
            }
            let (pixel_width, pixel_height) = ((rect.right - rect.left) as u32, (rect.bottom - rect.top) as u32);
            let (mut dpi, mut dpi_y) = (BASE_DPI, BASE_DPI);
            // SAFETY: 显示器句柄来自输出的描述，输出参数指向本地变量。
            if unsafe { GetDpiForMonitor(desc.Monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y) }.is_err() || dpi == 0 {
                dpi = BASE_DPI;
            }
            let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            // SAFETY: 同上，`cbSize` 已经填好。
            let primary = unsafe { GetMonitorInfoW(desc.Monitor, &mut info) }.as_bool()
                && info.dwFlags & MONITORINFOF_PRIMARY != 0;
            let display = Display {
                id: (adapter_index << 8) | output_index,
                width: pixel_width * BASE_DPI / dpi,
                height: pixel_height * BASE_DPI / dpi,
                pixel_width,
                pixel_height,
            };
            let output = Output { display, adapter: adapter.clone(), output, rect };
            if primary { outputs.insert(0, output) } else { outputs.push(output) }
        }
    }
    outputs
}
