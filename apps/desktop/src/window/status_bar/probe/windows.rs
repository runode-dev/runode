//! Windows 上状态栏要问系统的事，都经系统接口：进程表的父进程取自 Toolhelp 快照，内存、CPU 和命令行打开
//! 各个进程问；在监听的 TCP 端口取自 `GetExtendedTcpTable`；防止休眠用电源请求。

use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    os::windows::ffi::OsStringExt as _,
    path::Path,
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

use windows_sys::{
    Wdk::System::Threading::{NtQueryInformationProcess, ProcessCommandLineInformation},
    Win32::{
        Foundation::{
            CloseHandle, ERROR_INSUFFICIENT_BUFFER, FILETIME, HANDLE, INVALID_HANDLE_VALUE, NO_ERROR, UNICODE_STRING,
        },
        NetworkManagement::IpHelper::{
            GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
        },
        Networking::WinSock::{AF_INET, AF_INET6},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
            },
            Power::{PowerClearRequest, PowerCreateRequest, PowerRequestSystemRequired, PowerSetRequest},
            ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX2},
            SystemServices::POWER_REQUEST_CONTEXT_VERSION,
            Threading::{
                GetProcessTimes, OpenProcess, POWER_REQUEST_CONTEXT_SIMPLE_STRING, PROCESS_QUERY_LIMITED_INFORMATION,
                REASON_CONTEXT, REASON_CONTEXT_0,
            },
        },
    },
};

use super::{Helper, Port, Proc, Procs};

/// 一个进程的创建时间、累计 CPU 时间（都按 100 纳秒计）和问的时刻。
type CpuSample = (u64, u64, Instant);

/// 上一次问到的各进程，下一次据此算出这段时间的 CPU 占用。
static CPU_TIMES: LazyLock<Mutex<HashMap<u32, CpuSample>>> = LazyLock::new(Mutex::default);

/// 比这更早的采样不拿来算 CPU，这一轮当作没有上一次：隔得太久的两次采样做差，算出的是那整段时间的平均值，
/// 不是现在的占用。
const CPU_SAMPLE_MAX_AGE: Duration = super::super::POLL_INTERVAL.saturating_mul(2);

/// 现在的进程表；快照拿不到时是空的。内存、CPU 和命令行只问 `roots` 和它们的子孙，以及这个 app 的
/// 可执行文件跑的进程（认 `Helper`）。
///
/// Windows 不改孤儿进程记着的父进程号，进程号又回收得快，记着的父进程可能早退出了、号被后来的进程占了，
/// 所以子进程比父进程创建得早时当作没有父进程。
pub(in crate::window::status_bar) fn processes(roots: &HashSet<u32>) -> Procs {
    let snapshot = snapshot();
    let mut procs = Procs::new(
        snapshot.iter().map(|(&pid, &(ppid, _))| (pid, Proc { ppid, memory: 0, cpu: 0., helper: None })).collect(),
    );
    let exe = std::env::current_exe().ok();
    let exe_name = exe.as_deref().and_then(Path::file_name).map(|name| name.to_string_lossy().into_owned());
    let is_runode = |pid: &u32| exe_name.as_deref().is_some_and(|name| snapshot[pid].1.eq_ignore_ascii_case(name));
    // 打不开的进程（比如提权跑的）底下也可能有打得开的子孙，照样算在里面。父进程排在子孙前面，
    // 下面认父进程的创建时间多半已经问过。
    let used: Vec<u32> = roots
        .iter()
        .filter(|pid| snapshot.contains_key(pid))
        .flat_map(|&root| procs.subtree(root))
        .chain(snapshot.keys().copied().filter(is_runode))
        .collect();

    let now = Instant::now();
    let mut cpu_times = CPU_TIMES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut seen = HashMap::new();
    // 各进程的创建时间，打不开的为 `None`。
    let mut created: HashMap<u32, Option<u64>> = HashMap::new();
    let mut visited = HashSet::new();
    for pid in used {
        if !visited.insert(pid) {
            continue;
        }
        let Some(process) = Process::open(pid) else { continue };
        let Some((at, cpu_time)) = process.times() else { continue };
        created.insert(pid, Some(at));
        seen.insert(pid, (at, cpu_time, now));
        let proc = procs.procs.get_mut(&pid).unwrap();
        let parent_at =
            *created.entry(proc.ppid).or_insert_with(|| Process::open(proc.ppid)?.times().map(|(at, _)| at));
        // 比它晚创建的「父进程」是后来占了这个进程号的别的进程；摘下来以后 `Procs` 不再把它算进那棵树。
        if parent_at.is_some_and(|parent| parent > at) {
            proc.ppid = 0;
        }
        proc.memory = process.private_memory().unwrap_or(0);
        proc.cpu = match cpu_times.get(&pid) {
            Some(&(before_at, before, when))
                if before_at == at && cpu_time >= before && now.duration_since(when) <= CPU_SAMPLE_MAX_AGE =>
            {
                let elapsed = now.duration_since(when).as_secs_f64();
                if elapsed > 0. { ((cpu_time - before) as f64 / 1e7 / elapsed * 100.) as f32 } else { 0. }
            }
            _ => 0.,
        };
        if is_runode(&pid) {
            proc.helper =
                process.command_line().and_then(|line| line.split_whitespace().last().and_then(Helper::from_flag));
        }
    }
    *cpu_times = seen;
    Procs::new(procs.procs)
}

/// 所有进程的进程号对应父进程号和可执行文件名。
fn snapshot() -> HashMap<u32, (u32, String)> {
    let mut all = HashMap::new();
    // SAFETY: 只取进程列表，不涉及别的参数。
    let handle = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if handle == INVALID_HANDLE_VALUE {
        tracing::debug!("cannot snapshot processes: {}", std::io::Error::last_os_error());
        return all;
    }
    let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
    // SAFETY: `handle` 是上面拿到的快照，`entry` 按要求设好了 `dwSize`。
    let mut more = unsafe { Process32FirstW(handle, &mut entry) } != 0;
    while more {
        let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
        let name = OsString::from_wide(&entry.szExeFile[..len]).to_string_lossy().into_owned();
        all.insert(entry.th32ProcessID, (entry.th32ParentProcessID, name));
        // SAFETY: 同上。
        more = unsafe { Process32NextW(handle, &mut entry) } != 0;
    }
    // SAFETY: 快照的句柄归这里，关一次。
    unsafe { CloseHandle(handle) };
    all
}

/// 开着的进程句柄，放掉时关上。
struct Process(HANDLE);

impl Process {
    /// 打不开（别的用户的进程、刚退出的进程、系统进程）时为 `None`。
    fn open(pid: u32) -> Option<Self> {
        // SAFETY: 只要查询信息的权限，打不开时返回空句柄。
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        (!handle.is_null()).then(|| Self(handle))
    }

    /// 创建时间和用户态、内核态加起来的 CPU 时间，都按 100 纳秒计。
    fn times(&self) -> Option<(u64, u64)> {
        let [mut created, mut exited, mut kernel, mut user] = [FILETIME::default(); 4];
        // SAFETY: 句柄开着，四个出参都是可写的 `FILETIME`。
        let ok = unsafe { GetProcessTimes(self.0, &mut created, &mut exited, &mut kernel, &mut user) } != 0;
        let ticks = |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
        ok.then(|| (ticks(created), ticks(kernel) + ticks(user)))
    }

    /// 私有工作集，和任务管理器「内存」一栏同一个口径：工作集把和别的进程共用的系统库页面也算上，几个进程相加时
    /// 重复计数。系统不填私有工作集（为 0）时退回私有提交量，即任务管理器的「提交大小」。
    fn private_memory(&self) -> Option<u64> {
        let mut counters =
            PROCESS_MEMORY_COUNTERS_EX2 { cb: size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32, ..Default::default() };
        // SAFETY: 句柄开着，`cb` 写明了缓冲区是一个 `PROCESS_MEMORY_COUNTERS_EX2`。
        let ok =
            unsafe { GetProcessMemoryInfo(self.0, (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(), counters.cb) }
                != 0;
        let bytes =
            if counters.PrivateWorkingSetSize > 0 { counters.PrivateWorkingSetSize } else { counters.PrivateUsage };
        ok.then_some(bytes as u64)
    }

    fn command_line(&self) -> Option<String> {
        let mut len = 0;
        // SAFETY: 缓冲区长度为 0，只问要多大。
        unsafe { NtQueryInformationProcess(self.0, ProcessCommandLineInformation, std::ptr::null_mut(), 0, &mut len) };
        if (len as usize) < size_of::<UNICODE_STRING>() {
            return None;
        }
        // 用 `u64` 存，`UNICODE_STRING` 按指针对齐。
        let mut buffer = vec![0_u64; (len as usize).div_ceil(size_of::<u64>())];
        // SAFETY: 缓冲区至少 `len` 字节，可写。
        let status = unsafe {
            NtQueryInformationProcess(self.0, ProcessCommandLineInformation, buffer.as_mut_ptr().cast(), len, &mut len)
        };
        if status < 0 {
            return None;
        }
        // SAFETY: 调用成功时缓冲区开头是一个 `UNICODE_STRING`，`Buffer` 指向同一个缓冲区里后面的 `Length` 字节。
        let text = unsafe {
            let string = &*buffer.as_ptr().cast::<UNICODE_STRING>();
            if string.Buffer.is_null() {
                return None;
            }
            std::slice::from_raw_parts(string.Buffer, usize::from(string.Length) / 2)
        };
        Some(String::from_utf16_lossy(text))
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: 句柄归这里，关一次。
        unsafe { CloseHandle(self.0) };
    }
}

/// 现在在监听的 TCP 端口，按端口号排好、同一个端口只列一次（IPv4 和 IPv6 各监听一次的那种）。
pub(in crate::window::status_bar) fn listening_ports() -> Vec<Port> {
    let names = snapshot();
    let v4 = tcp_listeners::<MIB_TCPROW_OWNER_PID>(AF_INET, |row| (row.dwLocalPort, row.dwOwningPid));
    let v6 = tcp_listeners::<MIB_TCP6ROW_OWNER_PID>(AF_INET6, |row| (row.dwLocalPort, row.dwOwningPid));
    let mut ports: Vec<Port> = Vec::new();
    for (port, pid) in v4.into_iter().chain(v6) {
        if !ports.iter().any(|known| known.port == port) {
            let command = names.get(&pid).map_or_else(String::new, |(_, name)| command_name(name).to_owned());
            ports.push(Port { port, pid, command });
        }
    }
    ports.sort_by_key(|port| port.port);
    ports
}

/// 可执行文件名去掉 `.exe`，和别的平台上的进程名一样写法。
fn command_name(exe: &str) -> &str {
    exe.len()
        .checked_sub(4)
        .filter(|&at| exe.is_char_boundary(at) && exe[at..].eq_ignore_ascii_case(".exe"))
        .map_or(exe, |at| &exe[..at])
}

/// 一个地址族里在监听的端口和监听它的进程号；问不到时是空的。`Row` 是这个地址族的
/// `TCP_TABLE_OWNER_PID_LISTENER` 表里一行的类型，`fields` 取出它的本地端口（网络字节序）和进程号。
fn tcp_listeners<Row>(family: u16, fields: impl Fn(&Row) -> (u32, u32)) -> Vec<(u16, u32)> {
    let mut buffer: Vec<u64> = Vec::new();
    let mut size = 0_u32;
    // 两次调用之间表可能变大，缓冲区不够时按新的大小再来。
    for _ in 0..4 {
        buffer.resize((size as usize).div_ceil(size_of::<u64>()), 0);
        // SAFETY: 缓冲区至少 `size` 字节，可写；`size` 为 0 时只问要多大。
        let status = unsafe {
            GetExtendedTcpTable(
                buffer.as_mut_ptr().cast(),
                &mut size,
                0,
                u32::from(family),
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        if status == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        if status != NO_ERROR || buffer.is_empty() {
            return Vec::new();
        }
        // SAFETY: 调用成功时缓冲区开头是行数，后面紧跟着这么多行 `Row`，都在 `size` 字节以内；`Row` 的对齐
        // 不超过 `u32`，从开头偏 `u32` 的位置就是第一行（表结构里 `table` 紧跟在 `dwNumEntries` 后面）。
        let rows = unsafe {
            let count = *buffer.as_ptr().cast::<u32>() as usize;
            std::slice::from_raw_parts(buffer.as_ptr().cast::<u32>().add(1).cast::<Row>(), count)
        };
        return rows.iter().map(&fields).map(|(port, pid)| (u16::from_be(port as u16), pid)).collect();
    }
    Vec::new()
}

/// 防止电脑闲置时休眠的电源请求：拿着期间系统不会因为没人操作而睡眠，放掉（drop）就恢复；app 退出时系统收回。
pub(in crate::window::status_bar) struct Caffeinate(HANDLE);

impl Caffeinate {
    pub(in crate::window::status_bar) fn start() -> std::io::Result<Self> {
        // `powercfg /requests` 里列出的原因。
        let mut reason: Vec<u16> = "Runode".encode_utf16().chain([0]).collect();
        let context = REASON_CONTEXT {
            Version: POWER_REQUEST_CONTEXT_VERSION,
            Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
            Reason: REASON_CONTEXT_0 { SimpleReasonString: reason.as_mut_ptr() },
        };
        // SAFETY: `context` 和它指着的原因字符串在调用期间都活着。
        let handle = unsafe { PowerCreateRequest(&context) };
        if handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        let request = Self(handle);
        // SAFETY: 句柄是上面建的电源请求。
        if unsafe { PowerSetRequest(request.0, PowerRequestSystemRequired) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(request)
    }

    /// 电源请求别人结束不了，一直拦着。
    pub(in crate::window::status_bar) fn exited(&mut self) -> bool {
        false
    }
}

impl Drop for Caffeinate {
    fn drop(&mut self) {
        // SAFETY: 句柄归这里，清掉请求后关一次。
        unsafe {
            PowerClearRequest(self.0, PowerRequestSystemRequired);
            CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_process_is_measured() {
        let me = std::process::id();
        let procs = processes(&HashSet::from([me]));
        assert!(procs.one(me).memory > 0);
    }

    #[test]
    fn a_listening_port_is_found() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let ports = listening_ports();
        let found = ports.iter().find(|known| known.port == port).expect("port listed");
        assert_eq!(found.pid, std::process::id());
        assert!(!found.command.is_empty() && !found.command.ends_with(".exe"), "{}", found.command);
    }

    #[test]
    fn exe_suffix_is_dropped() {
        assert_eq!(command_name("node.exe"), "node");
        assert_eq!(command_name("Code.EXE"), "Code");
        assert_eq!(command_name("python"), "python");
        assert_eq!(command_name("éxe"), "éxe");
    }

    #[test]
    fn sleep_can_be_prevented() {
        let mut caffeinate = Caffeinate::start().unwrap();
        assert!(!caffeinate.exited());
    }
}
