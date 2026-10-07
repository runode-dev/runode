//! 进程级的系统接口：替换 libghostty 依赖的外部实现。
//!
//! 这些设置对整个进程生效，应在启动时、使用依赖它们的终端功能之前设置。
//! 日志回调见 [`crate::set_logger`]，PNG 解码见
//! `kitty::graphics::set_png_decoder`。

use std::sync::RwLock;

use crate::{error::Result, ffi};

/// 安全随机数源：用密码学安全的随机字节填满缓冲区，成功返回 `true`，没有
/// 可用熵时返回 `false`。
///
/// libghostty 用它生成秘密（例如 Kitty 粘贴事件的一次性密码），所以必须是
/// 真正的 CSPRNG（`getrandom`、`arc4random_buf`、`BCryptGenRandom`、
/// `crypto.getRandomValues` 等），可预测的随机源是安全漏洞。
///
/// 可能在任意使用终端的线程上被调用，所以要求 `Send + Sync`。
pub type SecureRandom = dyn Fn(&mut [u8]) -> bool + Send + Sync;

static SECURE_RANDOM: RwLock<Option<Box<SecureRandom>>> = RwLock::new(None);

/// 替换 libghostty 的安全随机数源。
///
/// 默认情况下库从平台获取安全随机字节（POSIX 上是 `getrandom` 或
/// `arc4random_buf`，Windows 上是 CNG）。没有平台随机源的目标（如
/// wasm32-freestanding）没有默认值，需要熵的操作会以
/// [`Error::IoError`](crate::Error::IoError) 失败，直到设置了它。设置后，
/// 所有目标上都用它代替平台随机源；传 `None` 恢复平台默认值。
///
/// 回调里的 panic 被捕获并视为没有可用熵（返回 `false`），不会穿过 C 边界。
pub fn set_secure_random(f: Option<Box<SecureRandom>>) -> Result<()> {
    unsafe extern "C" fn callback(_userdata: *mut std::ffi::c_void, buf: *mut u8, len: usize) -> bool {
        let Ok(random) = SECURE_RANDOM.read() else {
            return false;
        };
        let Some(random) = random.as_deref() else {
            return false;
        };
        // 零长度时不构造切片：`from_raw_parts_mut` 要求非空且对齐的指针，
        // 而 C 没有保证零长度请求时的指针。
        if len == 0 {
            return true;
        }
        // SAFETY: libghostty 给出 `len` 字节的可写缓冲区，只在这次调用期间
        // 有效。
        let buf = unsafe { std::slice::from_raw_parts_mut(buf, len) };
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| random(buf))).unwrap_or(false)
    }

    let ptr: ffi::SysRandomSecureFn = f.as_ref().map(|_| callback as _);
    {
        let Ok(mut slot) = SECURE_RANDOM.write() else {
            return Err(crate::Error::InvalidValue);
        };
        *slot = f;
    }
    crate::sys_set(ffi::SysOption::RANDOM_SECURE, ptr.map_or(std::ptr::null(), |p| p as *const std::ffi::c_void))
}

/// 串行化会替换或依赖全局随机源的测试：随机源是进程级状态，而测试并行运行。
#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::{Error, Terminal, paste::PasteRequest, terminal::ClipboardReadError};

    /// 开启粘贴事件后粘贴一次，返回事件里的一次性密码（base64）。
    fn paste_event_password() -> crate::error::Result<String> {
        let pty = RefCell::new(Vec::new());
        let mut terminal = Terminal::new(20, 3).unwrap();
        terminal
            .on_pty_write(|_, data| pty.borrow_mut().extend_from_slice(data))
            .unwrap()
            .on_clipboard_read(|_, read| read.reply(Err(ClipboardReadError::Denied)))
            .unwrap();
        terminal.vt_write(b"\x1b[?5522h");
        terminal.paste(&PasteRequest::new(&["text/plain"]), |_, _| Ok(()))?;
        let event = String::from_utf8(pty.take()).unwrap();
        Ok(event.trim_start_matches("\x1b]5522;type=read:status=OK:pw=").split('\x1b').next().unwrap().to_owned())
    }

    #[test]
    fn secure_random_source_can_be_replaced() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);

        // 固定字节只为验证替换生效，真实宿主必须使用 CSPRNG。
        set_secure_random(Some(Box::new(|buf| {
            buf.fill(0);
            true
        })))
        .unwrap();
        // 随机源固定时，每次生成的密码都相同。
        let fixed = paste_event_password().unwrap();
        assert_ne!(fixed, "");
        assert_eq!(paste_event_password().unwrap(), fixed);

        // 没有熵时，需要它的操作以 IoError 失败；panic 也按没有熵处理。
        set_secure_random(Some(Box::new(|_| false))).unwrap();
        assert!(matches!(paste_event_password(), Err(Error::IoError)));
        set_secure_random(Some(Box::new(|_| panic!("no entropy")))).unwrap();
        assert!(matches!(paste_event_password(), Err(Error::IoError)));

        // 恢复平台默认值。
        set_secure_random(None).unwrap();
        let first = paste_event_password().unwrap();
        assert_ne!(first, fixed);
        assert_ne!(paste_event_password().unwrap(), first);
    }
}
