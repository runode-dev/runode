//! 经 `IFileOperation` 把文件移到 Windows 的回收站。进不了回收站的（网络共享、U 盘、关了回收站的盘、超过回收站
//! 容量的）`IFileOperation` 会直接删掉，没有桌面的会话里也不问，所以挂上 `Guard`，删之前看系统打算怎么删。

use std::{
    io,
    os::windows::ffi::OsStrExt as _,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use windows::{
    Win32::{
        System::Com::{
            CLSCTX_ALL, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        },
        UI::Shell::{
            FOF_ALLOWUNDO, FOF_NO_UI, FOFX_EARLYFAILURE, FileOperation, IFileOperation, IFileOperationProgressSink,
            IShellItem, SHCreateItemFromParsingName,
        },
    },
    core::PCWSTR,
};

use self::guard::Guard;

/// 把 `path` 移到回收站；系统要直接删掉它时什么都不做，报 `Unsupported`。
pub(super) fn trash(path: &Path) -> io::Result<()> {
    // 只能给绝对路径，相对路径按进程的当前目录算。
    let wide: Vec<u16> = std::path::absolute(path)?.as_os_str().encode_wide().chain([0]).collect();
    // 在自己的线程里进单线程套间：调用方的线程可能已经按别的方式初始化过 COM。
    thread::Builder::new()
        .name("recycle-bin".into())
        .spawn(move || in_apartment(&wide))?
        .join()
        .unwrap_or_else(|_| Err(io::Error::other("the recycle bin thread panicked")))
}

fn in_apartment(path: &[u16]) -> io::Result<()> {
    // SAFETY: 这个线程是刚起的，没有初始化过 COM；成功时下面配对地 `CoUninitialize`。
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }.ok().map_err(io::Error::from)?;
    let result = recycle(path);
    // SAFETY: 和上面成功的 `CoInitializeEx` 配对；COM 对象都已在 `recycle` 里放掉。
    unsafe { CoUninitialize() };
    result
}

fn recycle(path: &[u16]) -> io::Result<()> {
    let refused = Arc::new(AtomicBool::new(false));
    let guard: IFileOperationProgressSink = Guard { refused: refused.clone() }.into();
    // SAFETY: `path` 以 NUL 结尾，在调用期间一直活着；接口都由 windows 按引用计数管着。
    let performed = unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(path.as_ptr()), None)?;
        let operation: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
        operation.SetOperationFlags(FOF_ALLOWUNDO | FOF_NO_UI | FOFX_EARLYFAILURE)?;
        operation.DeleteItem(&item, &guard)?;
        operation.PerformOperations().and_then(|()| operation.GetAnyOperationsAborted())
    };
    if refused.load(Ordering::Relaxed) {
        return Err(io::ErrorKind::Unsupported.into());
    }
    match performed {
        Ok(aborted) if aborted.as_bool() => Err(io::ErrorKind::Interrupted.into()),
        Ok(_) => Ok(()),
        Err(err) => Err(err.into()),
    }
}

#[expect(trivial_casts, reason = "`implement` 展开出的 vtable 指针转换")]
mod guard {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use windows::{
        Win32::{
            Foundation::E_ABORT,
            UI::Shell::{
                IFileOperationProgressSink, IFileOperationProgressSink_Impl, IShellItem, TSF_DELETE_RECYCLE_IF_POSSIBLE,
            },
        },
        core::{HRESULT, PCWSTR, Ref, implement},
    };

    /// 删每一项之前系统交来打算怎么删：不带 `TSF_DELETE_RECYCLE_IF_POSSIBLE` 的是要直接删掉，中止整个操作。
    #[implement(IFileOperationProgressSink)]
    pub(super) struct Guard {
        pub(super) refused: Arc<AtomicBool>,
    }

    impl IFileOperationProgressSink_Impl for Guard_Impl {
        fn PreDeleteItem(&self, flags: u32, _: Ref<IShellItem>) -> windows::core::Result<()> {
            if flags & TSF_DELETE_RECYCLE_IF_POSSIBLE.0.cast_unsigned() != 0 {
                return Ok(());
            }
            self.refused.store(true, Ordering::Relaxed);
            Err(E_ABORT.into())
        }

        fn StartOperations(&self) -> windows::core::Result<()> {
            Ok(())
        }

        fn FinishOperations(&self, _: HRESULT) -> windows::core::Result<()> {
            Ok(())
        }

        fn PreRenameItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> windows::core::Result<()> {
            Ok(())
        }

        fn PostRenameItem(
            &self,
            _: u32,
            _: Ref<IShellItem>,
            _: &PCWSTR,
            _: HRESULT,
            _: Ref<IShellItem>,
        ) -> windows::core::Result<()> {
            Ok(())
        }

        fn PreMoveItem(&self, _: u32, _: Ref<IShellItem>, _: Ref<IShellItem>, _: &PCWSTR) -> windows::core::Result<()> {
            Ok(())
        }

        fn PostMoveItem(
            &self,
            _: u32,
            _: Ref<IShellItem>,
            _: Ref<IShellItem>,
            _: &PCWSTR,
            _: HRESULT,
            _: Ref<IShellItem>,
        ) -> windows::core::Result<()> {
            Ok(())
        }

        fn PreCopyItem(&self, _: u32, _: Ref<IShellItem>, _: Ref<IShellItem>, _: &PCWSTR) -> windows::core::Result<()> {
            Ok(())
        }

        fn PostCopyItem(
            &self,
            _: u32,
            _: Ref<IShellItem>,
            _: Ref<IShellItem>,
            _: &PCWSTR,
            _: HRESULT,
            _: Ref<IShellItem>,
        ) -> windows::core::Result<()> {
            Ok(())
        }

        fn PostDeleteItem(
            &self,
            _: u32,
            _: Ref<IShellItem>,
            _: HRESULT,
            _: Ref<IShellItem>,
        ) -> windows::core::Result<()> {
            Ok(())
        }

        fn PreNewItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> windows::core::Result<()> {
            Ok(())
        }

        fn PostNewItem(
            &self,
            _: u32,
            _: Ref<IShellItem>,
            _: &PCWSTR,
            _: &PCWSTR,
            _: u32,
            _: HRESULT,
            _: Ref<IShellItem>,
        ) -> windows::core::Result<()> {
            Ok(())
        }

        fn UpdateProgress(&self, _: u32, _: u32) -> windows::core::Result<()> {
            Ok(())
        }

        fn ResetTimer(&self) -> windows::core::Result<()> {
            Ok(())
        }

        fn PauseTimer(&self) -> windows::core::Result<()> {
            Ok(())
        }

        fn ResumeTimer(&self) -> windows::core::Result<()> {
            Ok(())
        }
    }
}
