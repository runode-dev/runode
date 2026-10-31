//! Windows 上经 ssh 跑的进程在服务的会话（会话 0）里，那里没有屏幕：从那里起的宿主，它开的终端里起的
//! 窗口谁也看不见；从那里起的采集进程采不到屏幕。显示在屏幕上的会话里登录着同一个用户时，经任务计划把
//! 进程起在那里（`schedule`），和他自己在那台电脑上开 runode 时一样。

use std::{io, os::windows::process::CommandExt as _, path::Path, process::Command};

/// 本进程和显示在屏幕上的那个会话的关系。
pub(crate) enum Desktop {
    /// 本进程不在服务的会话里，直接起就在用户能看见的地方。
    Here,
    /// 本进程在服务的会话里，屏幕上登录着的是同一个用户。
    Mine,
    /// 本进程在服务的会话里，屏幕上没人登录。
    Nobody,
    /// 本进程在服务的会话里，屏幕上登录着的是别的用户。
    Other(String),
}

pub(crate) fn desktop() -> Desktop {
    if !runode_screen::in_service_session() {
        return Desktop::Here;
    }
    let me = std::env::var("USERNAME").unwrap_or_default();
    match runode_screen::console_user() {
        None => Desktop::Nobody,
        Some(user) if user.eq_ignore_ascii_case(&me) => Desktop::Mine,
        Some(user) => Desktop::Other(user),
    }
}

/// 经任务计划以当前用户在他登录着的屏幕上起 `exe arg`，不等它：注册一个只在交互会话里跑的任务再叫它跑。
/// `schtasks /run` 不等程序起来就返回，交回的 `Scheduled` 放掉时才删任务，调用方要拿着它，等连上起来的
/// 程序或者放弃了再放；删任务不结束已经起来的程序。任务按 `task_xml` 注册：命令行建的任务默认跑满 72 小时
/// 就被结束、用电池时不起，优先级也低于普通程序，宿主和它的终端都受不了。
pub(crate) fn schedule(exe: &Path, arg: &str) -> io::Result<Scheduled> {
    let name = format!("runode{arg}-{}", std::process::id());
    // schtasks 只认 UTF-16 的任务文件。
    let xml = std::env::temp_dir().join(format!("{name}.xml"));
    let bytes: Vec<u8> =
        [0xfeff_u16].into_iter().chain(task_xml(exe, arg).encode_utf16()).flat_map(u16::to_le_bytes).collect();
    std::fs::write(&xml, bytes)?;
    let created =
        schtasks(&["/create".as_ref(), "/f".as_ref(), "/tn".as_ref(), name.as_ref(), "/xml".as_ref(), xml.as_os_str()]);
    let _ = std::fs::remove_file(&xml);
    created?;
    let task = Scheduled { name };
    schtasks(&["/run".as_ref(), "/tn".as_ref(), task.name.as_ref()])?;
    Ok(task)
}

/// `schedule` 注册的任务，放掉时删掉。
pub(crate) struct Scheduled {
    name: String,
}

impl Drop for Scheduled {
    fn drop(&mut self) {
        if let Err(err) = schtasks(&["/delete".as_ref(), "/f".as_ref(), "/tn".as_ref(), self.name.as_ref()]) {
            tracing::warn!("cannot delete the scheduled task {}: {err}", self.name);
        }
    }
}

/// 跑 `schtasks args`，不弹控制台窗口；失败时带上它说的原因。
fn schtasks(args: &[&std::ffi::OsStr]) -> io::Result<()> {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = Command::new("schtasks")
        .args(args)
        .stdin(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    if output.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(io::Error::other(format!("schtasks {} failed: {said}", args[0].display())))
}

/// 跑 `exe arg` 的任务：以注册它的用户、在他已经登录的交互会话里跑（`InteractiveToken`），不限运行时长
/// （`PT0S`），用电池时照样起、拔掉电源也不停，优先级和普通程序一样（任务的 5 对应普通）。
fn task_xml(exe: &Path, arg: &str) -> String {
    let escape = |text: &str| text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-16"?>"#,
            r#"<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">"#,
            r#"<Principals><Principal id="Author"><LogonType>InteractiveToken</LogonType>"#,
            r#"<RunLevel>LeastPrivilege</RunLevel></Principal></Principals>"#,
            r#"<Settings><MultipleInstancesPolicy>Parallel</MultipleInstancesPolicy>"#,
            r#"<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>"#,
            r#"<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>"#,
            r#"<ExecutionTimeLimit>PT0S</ExecutionTimeLimit><Priority>5</Priority></Settings>"#,
            r#"<Actions Context="Author"><Exec><Command>{exe}</Command><Arguments>{arg}</Arguments></Exec></Actions>"#,
            r#"</Task>"#
        ),
        exe = escape(&exe.to_string_lossy()),
        arg = escape(arg),
    )
}
