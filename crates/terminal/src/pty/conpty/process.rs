//! 读别的进程：谁是谁的子进程（Toolhelp 快照）、可执行文件、命令行和当前目录。父进程号只是
//! 进程起来时记下的数字，父进程退出后可能被别的进程重用，所以子进程还要比父进程起得晚才算。

use std::{
    ffi::{OsString, c_void},
    mem::{offset_of, size_of},
    os::windows::ffi::OsStringExt as _,
    path::PathBuf,
    ptr,
};

use windows_sys::{
    Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation, ProcessCommandLineInformation},
    Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE, LocalFree, UNICODE_STRING},
        System::{
            Diagnostics::{
                Debug::ReadProcessMemory,
                ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS},
            },
            Threading::{
                GetProcessTimes, OpenProcess, PEB, PROCESS_BASIC_INFORMATION, PROCESS_NAME_WIN32,
                PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
                QueryFullProcessImageNameW,
            },
        },
        UI::Shell::CommandLineToArgvW,
    },
};

/// `RTL_USER_PROCESS_PARAMETERS` 里 `CurrentDirectory.DosPath` 的偏移（64 位进程）。windows-sys 的
/// 定义把这一段算在保留字段里，没有名字。
const CURRENT_DIRECTORY_OFFSET: usize = 0x38;

/// 打开的进程句柄，丢掉时关上。
struct Process(HANDLE);

impl Process {
    fn open(pid: u32, access: u32) -> Option<Self> {
        // SAFETY: 只是按进程号打开句柄，失败时返回空。
        let handle = unsafe { OpenProcess(access, 0, pid) };
        (!handle.is_null()).then_some(Self(handle))
    }

    /// 进程起来的时刻（FILETIME 的 100 纳秒数）。
    fn created(&self) -> Option<u64> {
        let mut times = [FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 }; 4];
        let [created, exited, kernel, user] = &mut times;
        // SAFETY: 句柄开着，四个输出都指向本地变量。
        let ok = unsafe { GetProcessTimes(self.0, created, exited, kernel, user) } != 0;
        ok.then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }

    /// 读目标进程里 `address` 处的 `T`。
    fn read<T: Copy>(&self, address: usize) -> Option<T> {
        let mut value = std::mem::MaybeUninit::<T>::uninit();
        let mut read = 0;
        // SAFETY: 输出缓冲是本地的 `T`，长度如实传入；读不到时不用它。
        let ok = unsafe {
            ReadProcessMemory(self.0, address as *const c_void, value.as_mut_ptr().cast(), size_of::<T>(), &mut read)
        } != 0;
        // SAFETY: 读满了 `size_of::<T>()` 字节，`T` 是纯数据。
        (ok && read == size_of::<T>()).then(|| unsafe { value.assume_init() })
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: 句柄是 `OpenProcess` 给的，只关这一次。
        unsafe { CloseHandle(self.0) };
    }
}

/// 现在所有进程的（进程号，父进程号）。
fn snapshot() -> Vec<(u32, u32)> {
    // SAFETY: 只是取快照，失败时返回 `INVALID_HANDLE_VALUE`。
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap == INVALID_HANDLE_VALUE {
        return Vec::new();
    }
    let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
    let mut pairs = Vec::new();
    // SAFETY: `snap` 开着，`entry` 是本地变量、`dwSize` 已经填好。
    let mut more = unsafe { Process32FirstW(snap, &mut entry) } != 0;
    while more {
        pairs.push((entry.th32ProcessID, entry.th32ParentProcessID));
        // SAFETY: 同上。
        more = unsafe { Process32NextW(snap, &mut entry) } != 0;
    }
    // SAFETY: 快照句柄只关这一次。
    unsafe { CloseHandle(snap) };
    pairs
}

fn created(pid: u32) -> Option<u64> {
    Process::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?.created()
}

/// `parent` 的子进程，带着起来的时刻；比 `parent` 起得早的不算（父进程号被重用了）。
fn children(pairs: &[(u32, u32)], parent: u32) -> Vec<(u32, u64)> {
    let Some(since) = created(parent) else {
        return Vec::new();
    };
    pairs
        .iter()
        .filter(|&&(pid, ppid)| ppid == parent && pid != parent)
        .filter_map(|&(pid, _)| Some((pid, created(pid).filter(|&at| at >= since)?)))
        .collect()
}

/// `shell` 最新起的那个子进程。
pub(super) fn newest_child(shell: u32) -> Option<u32> {
    children(&snapshot(), shell).into_iter().max_by_key(|&(_, at)| at).map(|(pid, _)| pid)
}

/// `leader` 和它所有的子孙进程；组长总在最前面。
pub(crate) fn group_members(leader: u32) -> Vec<u32> {
    let pairs = snapshot();
    let mut members = vec![leader];
    let mut next = 0;
    while let Some(&parent) = members.get(next) {
        for (pid, _) in children(&pairs, parent) {
            if !members.contains(&pid) {
                members.push(pid);
            }
        }
        next += 1;
    }
    members
}

/// 可执行文件的完整路径。
fn image_path(process: &Process) -> Option<String> {
    let mut buf = vec![0u16; 32 * 1024];
    let mut len = buf.len() as u32;
    // SAFETY: 缓冲和长度如实传入，成功时 `len` 是写进去的字数（不含结尾的 NUL）。
    let ok = unsafe { QueryFullProcessImageNameW(process.0, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) } != 0;
    ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
}

/// 进程名：可执行文件的文件名，带 `.exe`，和任务管理器里的一样。
pub(crate) fn process_name(pid: u32) -> Option<String> {
    let path = image_path(&Process::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?)?;
    let name = path.rsplit('\\').next().unwrap_or(&path);
    (!name.is_empty()).then(|| name.to_owned())
}

/// 进程的可执行文件路径和全部参数（命令行按 `CommandLineToArgvW` 的规矩拆开）。
pub fn process_args(pid: u32) -> Option<(String, Vec<String>)> {
    let process = Process::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let exe = image_path(&process).unwrap_or_default();
    let line = command_line(&process)?;
    if line.is_empty() {
        return None;
    }
    let mut line: Vec<u16> = line.into_iter().chain([0]).collect();
    let mut count = 0;
    // SAFETY: `line` 以 NUL 结尾；返回的数组用完后用 `LocalFree` 释放。
    let argv = unsafe { CommandLineToArgvW(line.as_mut_ptr(), &mut count) };
    if argv.is_null() {
        return None;
    }
    let args = (0..usize::try_from(count).unwrap_or(0))
        .map(|i| {
            // SAFETY: `argv` 里有 `count` 个以 NUL 结尾的字符串。
            let arg = unsafe { *argv.add(i) };
            let len = (0..).take_while(|&j| unsafe { *arg.add(j) } != 0).count();
            // SAFETY: 上面数出了长度。
            String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(arg, len) })
        })
        .collect();
    // SAFETY: `argv` 是 `CommandLineToArgvW` 分配的，只释放这一次。
    unsafe { LocalFree(argv.cast()) };
    Some((exe, args))
}

/// 进程启动时的命令行，UTF-16。
fn command_line(process: &Process) -> Option<Vec<u16>> {
    let mut len = 0u32;
    // SAFETY: 先问要多大的缓冲，不写输出。
    unsafe { NtQueryInformationProcess(process.0, ProcessCommandLineInformation, ptr::null_mut(), 0, &mut len) };
    if (len as usize) < size_of::<UNICODE_STRING>() {
        return None;
    }
    // 按 `UNICODE_STRING` 对齐，后面紧跟着它指向的字符。
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: 缓冲至少 `len` 字节；成功时开头是一个 `UNICODE_STRING`，`Buffer` 指向缓冲里面。
    let status = unsafe {
        NtQueryInformationProcess(process.0, ProcessCommandLineInformation, buf.as_mut_ptr().cast(), len, &mut len)
    };
    if status < 0 {
        return None;
    }
    // SAFETY: 成功时缓冲开头是 `UNICODE_STRING`，`Buffer` 指向缓冲里 `Length` 字节的字符。
    unsafe {
        let text = &*buf.as_ptr().cast::<UNICODE_STRING>();
        if text.Buffer.is_null() {
            return Some(Vec::new());
        }
        Some(std::slice::from_raw_parts(text.Buffer, usize::from(text.Length) / 2).to_vec())
    }
}

/// 进程的当前目录：从它的 PEB 里读 `RTL_USER_PROCESS_PARAMETERS.CurrentDirectory`。只认 64 位进程。
pub(super) fn process_cwd(pid: u32) -> Option<PathBuf> {
    let process = Process::open(pid, PROCESS_QUERY_INFORMATION | PROCESS_VM_READ)?;
    // SAFETY: 全零的 `PROCESS_BASIC_INFORMATION` 是合法的值。
    let mut basic: PROCESS_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let mut len = 0;
    // SAFETY: 输出指向本地变量，长度如实传入。
    let status = unsafe {
        NtQueryInformationProcess(
            process.0,
            ProcessBasicInformation,
            (&raw mut basic).cast(),
            size_of::<PROCESS_BASIC_INFORMATION>() as u32,
            &mut len,
        )
    };
    if status < 0 || basic.PebBaseAddress.is_null() {
        return None;
    }
    let params: usize = process.read(basic.PebBaseAddress as usize + offset_of!(PEB, ProcessParameters))?;
    let dir: UNICODE_STRING = process.read(params + CURRENT_DIRECTORY_OFFSET)?;
    let len = usize::from(dir.Length) / 2;
    if dir.Buffer.is_null() || len == 0 {
        return None;
    }
    let mut chars = vec![0u16; len];
    let mut read = 0;
    // SAFETY: 输出缓冲是本地的 `chars`，长度如实传入。
    let ok = unsafe {
        ReadProcessMemory(process.0, dir.Buffer.cast(), chars.as_mut_ptr().cast(), len * 2, &mut read)
    } != 0;
    if !ok || read != len * 2 {
        return None;
    }
    // 结尾带着 `\`，根目录（`C:\`）以外的去掉。
    if chars.len() > 3 && chars.last() == Some(&u16::from(b'\\')) {
        chars.pop();
    }
    Some(OsString::from_wide(&chars).into())
}
