//! 确认框。GPUI 的 `Window::prompt` 在 Windows 上把级别名（Warning、Info、Critical）写死成
//! 窗口标题，不是产品名、也不随界面语言变，所以 Windows 上自己弹 TaskDialog，标题写 Runode；
//! Linux 上没有原生的，画在窗口里（`in_window`）；macOS 交给 GPUI 弹原生的。

use futures::channel::oneshot;
use gpui::{App, PromptButton, PromptLevel, Window};

/// 在 `window` 上弹模态于它的确认框：图标随 `level`，`message` 是主说明，`detail` 是详细说明，按钮按
/// `answers` 的顺序排、第一个是默认按钮，收到的是按下的按钮在 `answers` 里的序号。有
/// `PromptButton::Cancel` 的按钮时 Esc 和关闭按钮按的是它，没有时两样都不起作用。框没答就没了（窗口关了）
/// 时收的那头得到 `Canceled`；收的那头丢掉时框跟着关掉。
pub fn show<T>(
    window: &mut Window,
    level: PromptLevel,
    message: &str,
    detail: Option<&str>,
    answers: &[T],
    cx: &mut App,
) -> oneshot::Receiver<usize>
where
    T: Clone + Into<PromptButton>,
{
    #[cfg(windows)]
    {
        let buttons = answers.iter().cloned().map(Into::into).collect();
        if let Some(answer) = task_dialog::show(window, level, message, detail, buttons, cx) {
            return answer;
        }
    }
    window.prompt(level, message, detail, answers, cx)
}

#[cfg(target_os = "linux")]
mod in_window;

/// 启动时调一次：没有原生确认框的平台上，`Window::prompt` 改弹 `in_window` 画的那个。
pub fn install(cx: &mut App) {
    #[cfg(target_os = "linux")]
    cx.set_prompt_builder(in_window::build);
    #[cfg(not(target_os = "linux"))]
    let _ = cx;
}

#[cfg(windows)]
mod task_dialog {
    use std::{
        cell::RefCell,
        collections::HashMap,
        ptr,
        rc::Rc,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    use futures::{
        channel::oneshot,
        future::{Either, select},
        lock::Mutex,
    };
    use gpui::{App, PromptButton, PromptLevel, Window};
    use raw_window_handle::RawWindowHandle;
    use windows_sys::{
        Win32::{
            Foundation::{HWND, LPARAM, S_FALSE, S_OK, WPARAM},
            UI::{
                Controls::{
                    TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TD_ERROR_ICON, TD_INFORMATION_ICON,
                    TD_WARNING_ICON, TDF_ALLOW_DIALOG_CANCELLATION, TDF_CALLBACK_TIMER, TDN_BUTTON_CLICKED, TDN_TIMER,
                    TaskDialogIndirect,
                },
                WindowsAndMessaging::{IDCANCEL, IDOK, IsWindow, PostMessageW, WM_CLOSE},
            },
        },
        core::HRESULT,
    };

    /// 框的窗口标题：产品名，不翻译。
    const TITLE: &str = "Runode";

    thread_local! {
        /// 每个所有者窗口（按 HWND）一把锁，同一个窗口上的框一个接一个弹：几个模态循环叠在一起时，先关的
        /// 那个会把所有者重新启用，后面的框就不再挡着窗口。没人用了就从表里拿掉。
        static OWNERS: RefCell<HashMap<isize, Rc<Mutex<()>>>> = RefCell::default();
    }

    /// 弹框的线程和主线程共用的状态。
    struct Dialog {
        /// 所有者窗口的 HWND。
        owner: isize,
        /// 有没有 `PromptButton::Cancel` 的按钮，没有时不让 Esc 和关闭按钮关框。
        has_cancel: bool,
        /// 不要这个框了：收的那头丢了，或者所有者窗口关了。弹框的线程据此关掉框、不交回结果。
        cancelled: AtomicBool,
    }

    impl Dialog {
        fn cancelled(&self) -> bool {
            self.cancelled.load(Ordering::Acquire)
        }

        fn cancel(&self) {
            self.cancelled.store(true, Ordering::Release);
        }
    }

    /// 取不到 Win32 的窗口句柄时为 `None`，由调用方交给 GPUI。TaskDialogIndirect 一直阻塞到框关掉，
    /// 而它的模态循环会同步地给所有者窗口发消息，所以框在单独的线程里弹，主线程照常处理消息，只在
    /// 前台执行器上等结果。
    pub(super) fn show(
        window: &mut Window,
        level: PromptLevel,
        message: &str,
        detail: Option<&str>,
        answers: Vec<PromptButton>,
        cx: &mut App,
    ) -> Option<oneshot::Receiver<usize>> {
        let handle = raw_window_handle::HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::Win32(handle) = handle.as_raw() else { return None };
        let owner = handle.hwnd.get();
        let id = window.window_handle().window_id();
        let dialog = Arc::new(Dialog {
            owner,
            has_cancel: answers.iter().any(PromptButton::is_cancel),
            cancelled: AtomicBool::new(false),
        });
        let lock = OWNERS.with_borrow_mut(|owners| owners.entry(owner).or_default().clone());
        // 所有者窗口关了框要跟着关：GPUI 销毁 HWND 前不等这里的框。
        let closed = cx.on_window_closed({
            let dialog = dialog.clone();
            move |_, closed| {
                if closed == id {
                    dialog.cancel();
                }
            }
        });
        let (message, detail) = (message.to_owned(), detail.map(str::to_owned));
        let (mut tx, rx) = oneshot::channel();
        cx.foreground_executor()
            .spawn(async move {
                let _closed = closed;
                async {
                    let guard = match select(lock.lock(), tx.cancellation()).await {
                        Either::Left((guard, _)) => guard,
                        Either::Right(_) => return,
                    };
                    if dialog.cancelled() {
                        return;
                    }
                    let (done_tx, done_rx) = oneshot::channel();
                    let spawned = std::thread::Builder::new().name("prompt".into()).spawn({
                        let dialog = dialog.clone();
                        move || {
                            let _ = done_tx.send(run(&dialog, level, &message, detail.as_deref(), &answers));
                        }
                    });
                    if let Err(err) = spawned {
                        tracing::warn!("could not start the prompt thread: {err}");
                        return;
                    }
                    let answer = match select(done_rx, tx.cancellation()).await {
                        Either::Left((answer, _)) => answer,
                        Either::Right(((), done_rx)) => {
                            dialog.cancel();
                            done_rx.await
                        }
                    };
                    // 框关掉之前不放锁，同一个窗口上的下一个框才不会和它叠在一起。
                    drop(guard);
                    if let Ok(Some(answer)) = answer {
                        let _ = tx.send(answer);
                    }
                }
                .await;
                OWNERS.with_borrow_mut(|owners| {
                    if Rc::strong_count(&lock) == 2 {
                        owners.remove(&owner);
                    }
                });
            })
            .detach();
        Some(rx)
    }

    /// 在弹框的线程上弹框，一直阻塞到框关掉。按了 `answers` 里的哪个按钮就交回它的序号；弹不出来、
    /// 不要这个框了时为 `None`。
    fn run(
        dialog: &Dialog,
        level: PromptLevel,
        message: &str,
        detail: Option<&str>,
        answers: &[PromptButton],
    ) -> Option<usize> {
        if dialog.cancelled() {
            return None;
        }
        let title = wide(TITLE);
        let message = wide(message);
        let detail = detail.map(wide);
        let labels: Vec<Vec<u16>> = answers.iter().map(|answer| wide(answer.label())).collect();
        // 小的正数是系统按钮的 ID，自己的按钮从 -1 往下排。
        let ids: Vec<i32> = answers
            .iter()
            .enumerate()
            .map(|(index, answer)| match answer {
                PromptButton::Ok(_) => IDOK,
                PromptButton::Cancel(_) => IDCANCEL,
                PromptButton::Other(_) => -1 - i32::try_from(index).unwrap_or(i32::MAX),
            })
            .collect();
        let buttons: Vec<TASKDIALOG_BUTTON> = ids
            .iter()
            .zip(&labels)
            .map(|(&id, label)| TASKDIALOG_BUTTON { nButtonID: id, pszButtonText: label.as_ptr() })
            .collect();

        let config = TASKDIALOGCONFIG {
            cbSize: u32::try_from(size_of::<TASKDIALOGCONFIG>()).unwrap_or(u32::MAX),
            hwndParent: dialog.owner as HWND,
            // 关闭按钮和 Esc 都要 `TDF_ALLOW_DIALOG_CANCELLATION`，没有取消按钮时由 `callback` 拦下。
            dwFlags: TDF_ALLOW_DIALOG_CANCELLATION | TDF_CALLBACK_TIMER,
            pszWindowTitle: title.as_ptr(),
            Anonymous1: TASKDIALOGCONFIG_0 {
                pszMainIcon: match level {
                    PromptLevel::Info => TD_INFORMATION_ICON,
                    PromptLevel::Warning => TD_WARNING_ICON,
                    PromptLevel::Critical => TD_ERROR_ICON,
                },
            },
            pszMainInstruction: message.as_ptr(),
            pszContent: detail.as_ref().map_or(ptr::null(), |detail| detail.as_ptr()),
            cButtons: u32::try_from(buttons.len()).unwrap_or(0),
            pButtons: buttons.as_ptr(),
            pfCallback: Some(callback),
            lpCallbackData: ptr::from_ref(dialog) as isize,
            ..TASKDIALOGCONFIG::default()
        };

        let mut pressed = 0;
        // SAFETY: `config` 里的字符串、按钮和 `dialog` 都活到 TaskDialogIndirect 返回；不要单选框和复选框的结果。
        let result = unsafe { TaskDialogIndirect(&config, &mut pressed, ptr::null_mut(), ptr::null_mut()) };
        if result < 0 {
            tracing::warn!("could not show the prompt: HRESULT {result:#x}");
            return None;
        }
        if dialog.cancelled() {
            return None;
        }
        ids.iter().position(|&id| id == pressed)
    }

    /// TaskDialog 的回调，`data` 是 `run` 交进去的 `Dialog`。没有取消按钮时拦下 Esc 和关闭按钮；计时器每
    /// 走一下（约 200 毫秒）看一次要不要关框，所有者窗口没了也关。
    unsafe extern "system" fn callback(hwnd: HWND, msg: u32, wparam: WPARAM, _: LPARAM, data: isize) -> HRESULT {
        // SAFETY: `data` 指着 `run` 借着的 `Dialog`，TaskDialogIndirect 返回前一直有效。
        let dialog = unsafe { &*(data as *const Dialog) };
        match msg.cast_signed() {
            TDN_TIMER => {
                // SAFETY: `IsWindow` 对任何值都只回答是不是还在的窗口。
                if !dialog.cancelled() && unsafe { IsWindow(dialog.owner as HWND) } == 0 {
                    dialog.cancel();
                }
                if dialog.cancelled() {
                    // SAFETY: `hwnd` 是这个线程上正开着的框。
                    unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
                }
                S_OK
            }
            TDN_BUTTON_CLICKED
                if i32::try_from(wparam) == Ok(IDCANCEL) && !dialog.has_cancel && !dialog.cancelled() =>
            {
                S_FALSE
            }
            _ => S_OK,
        }
    }

    /// 以 0 结尾的 UTF-16。
    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0]).collect()
    }
}
