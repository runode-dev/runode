//! portable-pty 加载不到 conpty.dll 时会静默退回系统 ConPTY。旧版 Windows 10 的 conhost
//! 在调整终端尺寸时可能崩溃，连带结束整个会话；这里先加载随 app 分发的运行库，不允许回退。

use std::{os::windows::ffi::OsStrExt as _, sync::OnceLock};

use anyhow::{Context as _, Result, anyhow, bail};
use windows_sys::Win32::{
    Foundation::FreeLibrary,
    System::LibraryLoader::{
        GetProcAddress, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LoadLibraryExW,
    },
};

pub(super) fn load() -> Result<()> {
    static LOADED: OnceLock<Result<(), String>> = OnceLock::new();
    LOADED
        .get_or_init(|| load_bundled().map_err(|err| format!("{err:#}")))
        .as_ref()
        .copied()
        .map_err(|err| anyhow!("{err}"))
}

fn load_bundled() -> Result<()> {
    let executable = std::env::current_exe().context("cannot locate the executable for the bundled ConPTY runtime")?;
    let directory = executable.parent().context("the executable has no parent directory")?;
    let console = directory.join("OpenConsole.exe");
    if !console.is_file() {
        bail!("bundled ConPTY runtime is incomplete: {} is missing; reinstall Runode", console.display());
    }
    let library = directory.join("conpty.dll");
    let path: Vec<u16> = library.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: path 是以 NUL 结尾的绝对路径，只在此处加载一次。保留成功加载的引用到进程结束，
    // portable-pty 随后按 conpty.dll 这个名字打开的就是同一份模块，不会静默退回 kernel32。
    let module = unsafe {
        LoadLibraryExW(
            path.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        )
    };
    if module.is_null() {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("cannot load bundled ConPTY runtime {}; reinstall Runode", library.display()));
    }
    for name in [c"CreatePseudoConsole", c"ResizePseudoConsole", c"ClosePseudoConsole"] {
        // SAFETY: module 已经加载；name 是以 NUL 结尾的函数名，只查询地址，不调用。
        if unsafe { GetProcAddress(module, name.as_ptr().cast()) }.is_none() {
            // SAFETY: portable-pty 还没用过它，释放本次加载取得的唯一引用。
            unsafe { FreeLibrary(module) };
            bail!("bundled ConPTY runtime {} has no {}; reinstall Runode", library.display(), name.to_string_lossy());
        }
    }
    tracing::info!("using bundled ConPTY runtime {}", library.display());
    Ok(())
}
