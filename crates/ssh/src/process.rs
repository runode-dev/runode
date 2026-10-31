//! 起连主机的 ssh、给它接上管道。Windows 上的 OpenSSH 只在 overlapped 的句柄上读写才不卡：std 交给子进程的
//! 是同步的管道，ssh 读着这样的 stdin 时收不到远端的输出，stdout 是这样的管道时远端命令结束了它也不退出。所以
//! 连主机的 ssh 要接管道的 stdio 一律经 `spawn` 或 `output`。

use std::{
    io::{self, PipeReader, PipeWriter, Read as _},
    process::{Child, Command, Output, Stdio},
    thread,
};

/// 要接管道的 stdio；不接的照 `Command` 上设的。
#[derive(Clone, Copy, Debug, Default)]
pub struct Pipes {
    pub stdin: bool,
    pub stdout: bool,
    pub stderr: bool,
}

/// 起好的 ssh 和接上的管道在这一头的那端。
#[derive(Debug)]
pub struct Piped {
    pub child: Child,
    pub stdin: Option<PipeWriter>,
    pub stdout: Option<PipeReader>,
    pub stderr: Option<PipeReader>,
}

/// 起 `command`，按 `pipes` 接上管道。
pub fn spawn(command: &mut Command, pipes: Pipes) -> io::Result<Piped> {
    let stdin = pipes.stdin.then(to_child).transpose()?;
    let stdout = pipes.stdout.then(from_child).transpose()?;
    let stderr = pipes.stderr.then(from_child).transpose()?;
    let (stdin, theirs) = stdin.unzip();
    if let Some(theirs) = theirs {
        command.stdin(theirs);
    }
    let (stdout, theirs) = stdout.unzip();
    if let Some(theirs) = theirs {
        command.stdout(theirs);
    }
    let (stderr, theirs) = stderr.unzip();
    if let Some(theirs) = theirs {
        command.stderr(theirs);
    }
    let child = command.spawn();
    // 子进程那端留在 `command` 里时，它退出了这边也读不到结尾。
    if pipes.stdin {
        command.stdin(Stdio::null());
    }
    if pipes.stdout {
        command.stdout(Stdio::null());
    }
    if pipes.stderr {
        command.stderr(Stdio::null());
    }
    Ok(Piped { child: child?, stdin, stdout, stderr })
}

/// 跑完 `command`，收齐 stdout 和 stderr，同 `Command::output`；stdin 照 `command` 上设的。
pub fn output(command: &mut Command) -> io::Result<Output> {
    let piped = spawn(command, Pipes { stdin: false, stdout: true, stderr: true })?;
    let Piped { mut child, stdout, stderr, .. } = piped;
    // 两条一起读，免得 ssh 写满一条时卡住。
    let stderr = stderr.map(|mut stderr| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes);
            bytes
        })
    });
    let mut out = Vec::new();
    if let Some(mut stdout) = stdout {
        stdout.read_to_end(&mut out)?;
    }
    let err = stderr.map(|reader| reader.join().unwrap_or_default()).unwrap_or_default();
    Ok(Output { status: child.wait()?, stdout: out, stderr: err })
}

/// 子进程读的管道：这一头写，另一头交给子进程。
fn to_child() -> io::Result<(PipeWriter, Stdio)> {
    #[cfg(unix)]
    {
        let (theirs, ours) = io::pipe()?;
        Ok((ours, theirs.into()))
    }
    #[cfg(windows)]
    {
        let (ours, theirs) = windows::pipe(true)?;
        Ok((ours.into(), theirs.into()))
    }
}

/// 子进程写的管道：这一头读，另一头交给子进程。
fn from_child() -> io::Result<(PipeReader, Stdio)> {
    #[cfg(unix)]
    {
        let (ours, theirs) = io::pipe()?;
        Ok((ours, theirs.into()))
    }
    #[cfg(windows)]
    {
        let (ours, theirs) = windows::pipe(false)?;
        Ok((ours.into(), theirs.into()))
    }
}

#[cfg(windows)]
mod windows {
    use std::{
        io,
        os::windows::io::{FromRawHandle as _, OwnedHandle},
        ptr,
        sync::atomic::{AtomicU32, Ordering},
    };

    use windows_sys::Win32::{
        Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{
            CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_READ_ATTRIBUTES, OPEN_EXISTING,
            PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND,
        },
        System::Pipes::{CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT},
    };

    /// 管道一个方向上的缓冲。
    const BUFFER: u32 = 256 << 10;

    /// 一对命名管道的两头（这一头、子进程那头）；`child_reads` 时子进程那头是读的。子进程那头是 overlapped 的，
    /// 这一头是同步的；两头都不能继承，交给子进程时 std 另复制一份能继承的。
    pub(super) fn pipe(child_reads: bool) -> io::Result<(OwnedHandle, OwnedHandle)> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let name = format!(
            r"\\.\pipe\runode-ssh-{}-{}-{:x}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            std::hash::BuildHasher::hash_one(&std::hash::RandomState::new(), 0u8),
        );
        let name: Vec<u16> = name.encode_utf16().chain([0]).collect();
        let access = if child_reads { PIPE_ACCESS_OUTBOUND } else { PIPE_ACCESS_INBOUND };
        // SAFETY: `name` 是以 0 结尾的 UTF-16，活到调用结束；安全属性为空，用默认的、不能继承。
        let ours = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                access | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                BUFFER,
                BUFFER,
                0,
                ptr::null(),
            )
        };
        if ours == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: 句柄刚建好，归这里。
        let ours = unsafe { OwnedHandle::from_raw_handle(ours) };
        let access = if child_reads { GENERIC_READ } else { GENERIC_WRITE | FILE_READ_ATTRIBUTES };
        // SAFETY: 同上；打开的是刚建的那个管道，只有一个实例，别人抢先连上时这里失败。
        let theirs = unsafe {
            CreateFileW(name.as_ptr(), access, 0, ptr::null(), OPEN_EXISTING, FILE_FLAG_OVERLAPPED, ptr::null_mut())
        };
        if theirs == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: 句柄刚打开，归这里。
        Ok((ours, unsafe { OwnedHandle::from_raw_handle(theirs) }))
    }
}
