//! 状态栏要问系统的事：各个进程的父进程和 CPU（`ps`）、内存（`proc_pid_rusage`），有哪些 TCP 端口在监听（`lsof`），
//! 以及防止电脑休眠（`caffeinate`）。都是 macOS 自带的命令，跑不了时当没有。Linux 上端口读 `/proc`，防休眠经
//! `systemd-inhibit`；Windows 上这几样由 `windows` 经系统接口问。

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub(super) use windows::{Caffeinate, listening_ports, processes};

use std::collections::{HashMap, HashSet};
#[cfg(not(windows))]
use std::{
    io::Read as _,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// `ps`、`lsof` 最多跑这么久，超时当这次没数据：卡住一次不能让状态栏的轮询（连同防休眠）停下。
#[cfg(not(windows))]
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// 一个进程的父进程、内存（字节，macOS 上取 phys_footprint，Windows 上取私有工作集）和 CPU 占用（百分比，一个核满载是 100）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Proc {
    pub ppid: u32,
    pub memory: u64,
    pub cpu: f32,
    pub helper: Option<Helper>,
}

/// runode 按需拉起的子进程，按命令行认：这个 app 的可执行文件加一个参数。采集进程和拉起它的进程脱开，
/// 不一定在 app 和宿主的子孙里，所以在整张进程表里认。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Helper {
    /// `runode --speech`
    Speech,
    /// `runode --screen`
    Screen,
}

impl Helper {
    /// 命令行最后一个参数 `flag` 对应的子进程。
    fn from_flag(flag: &str) -> Option<Self> {
        match flag {
            "--speech" => Some(Self::Speech),
            "--screen" => Some(Self::Screen),
            _ => None,
        }
    }
}

/// 一组进程的 CPU 和内存加起来。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Usage {
    pub cpu: f32,
    pub memory: u64,
}

impl std::ops::Add for Usage {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self { cpu: self.cpu + other.cpu, memory: self.memory + other.memory }
    }
}

/// 这一刻的进程表。
#[derive(Default)]
pub(super) struct Procs {
    procs: HashMap<u32, Proc>,
    children: HashMap<u32, Vec<u32>>,
}

impl Procs {
    fn new(procs: HashMap<u32, Proc>) -> Self {
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for (&pid, proc) in &procs {
            children.entry(proc.ppid).or_default().push(pid);
        }
        Self { procs, children }
    }

    /// `pid` 和它所有的子孙进程，父进程排在它的子孙前面。父子关系成了环时每个进程也只列一次。
    fn subtree(&self, pid: u32) -> Vec<u32> {
        let mut all = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![pid];
        while let Some(pid) = stack.pop() {
            if seen.insert(pid) {
                all.push(pid);
                stack.extend(self.children.get(&pid).into_iter().flatten());
            }
        }
        all
    }

    /// 在跑的子进程，按种类和进程号排好。
    pub(super) fn helpers(&self) -> Vec<(Helper, u32)> {
        let mut helpers: Vec<_> = self.procs.iter().filter_map(|(&pid, proc)| Some((proc.helper?, pid))).collect();
        helpers.sort_unstable();
        helpers
    }

    /// 只算 `pid` 这一个进程。
    pub(super) fn one(&self, pid: u32) -> Usage {
        self.procs.get(&pid).map_or(Usage::default(), |proc| Usage { cpu: proc.cpu, memory: proc.memory })
    }

    /// `pid` 和它所有的子孙进程加起来。
    pub(super) fn tree(&self, pid: u32) -> Usage {
        self.subtree(pid).into_iter().fold(Usage::default(), |total, pid| total + self.one(pid))
    }

    /// `roots` 里各个进程连同子孙加起来；一个进程在另一个的子孙里时只算一次。
    pub(super) fn forest(&self, roots: &HashSet<u32>) -> Usage {
        roots
            .iter()
            .filter(|&&pid| {
                let parent = self.procs.get(&pid).map(|proc| proc.ppid);
                parent.and_then(|ppid| self.owner(ppid, |pid| roots.contains(&pid))).is_none()
            })
            .fold(Usage::default(), |total, &pid| total + self.tree(pid))
    }

    /// 沿父进程往上找，`pid` 自己或者最近的一个让 `is_root` 成立的祖先；找到 1 号进程还没有时为 `None`。
    pub(super) fn owner(&self, pid: u32, is_root: impl Fn(u32) -> bool) -> Option<u32> {
        let mut pid = pid;
        // 进程表是一刻的快照，父子关系理应成树；限一个深度，万一成了环也不会卡住。
        for _ in 0..64 {
            if is_root(pid) {
                return Some(pid);
            }
            pid = self.procs.get(&pid)?.ppid;
            if pid <= 1 {
                return None;
            }
        }
        None
    }
}

/// 子孙关系由 `procs` 决定，`children` 里的顺序随 `HashMap` 的遍历而变，不比。
impl PartialEq for Procs {
    fn eq(&self, other: &Self) -> bool {
        self.procs == other.procs
    }
}

/// 一个在监听的 TCP 端口，和监听它的进程。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Port {
    pub port: u16,
    pub pid: u32,
    pub command: String,
}

/// 现在的进程表；`ps` 跑不了时是空的。phys_footprint 只问 `roots` 和它们的子孙：状态栏只算这些进程的内存，
/// 别的进程只用来认父子关系，`ps` 列出的几百个进程挨个问一遍白白耗时。
#[cfg(not(windows))]
pub(super) fn processes(roots: &HashSet<u32>) -> Procs {
    let text = output(Command::new("/bin/ps").args(["-axo", "pid=,ppid=,rss=,%cpu=,args="]), PROBE_TIMEOUT);
    let exe = std::env::current_exe().map(|exe| exe.to_string_lossy().into_owned()).unwrap_or_default();
    let mut procs = Procs::new(parse_ps(&text, &exe));
    let used: HashSet<u32> = roots
        .iter()
        .flat_map(|&root| procs.subtree(root))
        .chain(procs.helpers().into_iter().map(|(_, pid)| pid))
        .collect();
    for pid in used {
        if let (Some(proc), Some(footprint)) = (procs.procs.get_mut(&pid), phys_footprint(pid)) {
            proc.memory = footprint;
        }
    }
    procs
}

/// 进程的 phys_footprint，和活动监视器「内存」一栏同一个口径。`ps` 的 rss 把映射进来的可执行文件、
/// 共享库这些干净的文件页也算上，单文件打包的大程序（比如 claude）能多出好几倍，几个进程相加时共享页还重复计数。
/// 读不了（别的用户的进程、刚退出的进程）时为 `None`，调用方退回 rss。
#[cfg(target_os = "macos")]
fn phys_footprint(pid: u32) -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
    // SAFETY: 缓冲区是一个 `rusage_info_v2`，按 `RUSAGE_INFO_V2` 内核正好写这么多字节。
    let status = unsafe { libc::proc_pid_rusage(pid.try_into().ok()?, libc::RUSAGE_INFO_V2, info.as_mut_ptr().cast()) };
    // SAFETY: 调用成功时内核写满了整个结构体；结构体本身也已清零，全是整数字段，哪个值都合法。
    (status == 0).then(|| unsafe { info.assume_init() }.ri_phys_footprint)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn phys_footprint(_pid: u32) -> Option<u64> {
    None
}

/// 现在在监听的 TCP 端口，按端口号排好、同一个端口只列一次（IPv4 和 IPv6 各监听一次的那种）。
#[cfg(target_os = "macos")]
pub(super) fn listening_ports() -> Vec<Port> {
    let text = output(Command::new("/usr/sbin/lsof").args(["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpcn"]), PROBE_TIMEOUT);
    parse_lsof(&text)
}

/// 跑命令拿标准输出；跑不了、`timeout` 内没跑完（这时结束它）时是空字符串。`lsof` 有进程读不了时
/// 退出码不是 0，输出照样能用。
#[cfg(not(windows))]
fn output(command: &mut Command, timeout: Duration) -> String {
    let spawned = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(err) => {
            tracing::debug!("cannot run {command:?}: {err}");
            return String::new();
        }
    };
    // 输出放在别的线程里读：一边等它退出一边不读的话，输出多过管道的缓冲时它会卡在写上。
    let mut stdout = child.stdout.take();
    let reader = thread::spawn(move || {
        let mut data = Vec::new();
        if let Some(stdout) = &mut stdout {
            let _ = stdout.read_to_end(&mut data);
        }
        data
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            waited => {
                if let Err(err) = waited {
                    tracing::debug!("cannot wait for {command:?}: {err}");
                } else {
                    tracing::warn!("{command:?} did not finish in {timeout:?}, killing it");
                }
                // 结束后管道关上，读的线程随之退出。
                let _ = child.kill();
                let _ = child.wait();
                return String::new();
            }
        }
    }
    reader.join().map(|data| String::from_utf8_lossy(&data).into_owned()).unwrap_or_default()
}

/// `ps -axo pid=,ppid=,rss=,%cpu=,args=` 的输出：每行进程号、父进程号、常驻内存（KB）、CPU 百分比和命令行。
/// 常驻内存先填进 `memory`，读得到 phys_footprint 时 `processes` 再换掉。`exe` 是这个 app 的可执行文件。
#[cfg(any(not(windows), test))]
fn parse_ps(text: &str, exe: &str) -> HashMap<u32, Proc> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let ppid = fields.next()?.parse().ok()?;
            let rss = fields.next()?.parse::<u64>().ok()? * 1024;
            // 有的语言环境里小数点是逗号。
            let cpu = fields.next()?.replace(',', ".").parse().ok()?;
            let args: Vec<&str> = fields.collect();
            let helper = match args.split_last() {
                Some((&flag, command)) if command.iter().copied().eq(exe.split_whitespace()) => Helper::from_flag(flag),
                _ => None,
            };
            Some((pid, Proc { ppid, memory: rss, cpu, helper }))
        })
        .collect()
}

/// 现在在监听的 TCP 端口，按端口号排好、同一个端口只列一次。只认得出本用户进程的端口：别人的进程的
/// 描述符读不了，和不带 sudo 的 `lsof` 一样。
#[cfg(all(unix, not(target_os = "macos")))]
pub(super) fn listening_ports() -> Vec<Port> {
    let mut inodes = HashMap::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        if let Ok(text) = std::fs::read_to_string(table) {
            inodes.extend(parse_proc_net_tcp(&text));
        }
    }
    let mut ports: Vec<Port> = Vec::new();
    if inodes.is_empty() {
        return ports;
    }
    let Ok(procs) = std::fs::read_dir("/proc") else { return ports };
    for entry in procs.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok()) else { continue };
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else { continue };
        let mut command = None;
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else { continue };
            let Some(inode) = target
                .to_str()
                .and_then(|target| target.strip_prefix("socket:["))
                .and_then(|rest| rest.strip_suffix(']'))
                .and_then(|inode| inode.parse::<u64>().ok())
            else {
                continue;
            };
            let Some(&port) = inodes.get(&inode) else { continue };
            if ports.iter().any(|known| known.port == port) {
                continue;
            }
            let command = command
                .get_or_insert_with(|| {
                    std::fs::read_to_string(entry.path().join("comm"))
                        .map(|comm| comm.trim().to_owned())
                        .unwrap_or_default()
                })
                .clone();
            ports.push(Port { port, pid, command });
        }
    }
    ports.sort_by_key(|port| port.port);
    ports
}

/// `/proc/net/tcp` 和 `tcp6` 里在监听（状态 `0A`）的套接字：inode 对端口号。
#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn parse_proc_net_tcp(text: &str) -> HashMap<u64, u16> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.get(3) != Some(&"0A") {
                return None;
            }
            let port = u16::from_str_radix(fields.get(1)?.rsplit_once(':')?.1, 16).ok()?;
            let inode = fields.get(9)?.parse().ok().filter(|&inode| inode != 0)?;
            Some((inode, port))
        })
        .collect()
}

/// `lsof -F pcn` 的输出：`p` 行开始一个进程，跟着 `c` 行是进程名，再是它的各个文件，`n` 行是地址，
/// 比如 `*:3000`、`127.0.0.1:5173`、`[::1]:8080`。
#[cfg(any(target_os = "macos", test))]
fn parse_lsof(text: &str) -> Vec<Port> {
    let mut ports: Vec<Port> = Vec::new();
    let (mut pid, mut command) = (0, String::new());
    for line in text.lines() {
        let Some(kind) = line.chars().next() else { continue };
        let value = &line[1..];
        match kind {
            'p' => pid = value.parse().unwrap_or(0),
            'c' => command = value.to_owned(),
            'n' => {
                let Some(port) = value.rsplit_once(':').and_then(|(_, port)| port.parse().ok()) else { continue };
                if pid != 0 && !ports.iter().any(|known| known.port == port) {
                    ports.push(Port { port, pid, command: command.clone() });
                }
            }
            _ => {}
        }
    }
    ports.sort_by_key(|port| port.port);
    ports
}

/// 防止电脑闲置时休眠：拿着期间系统不会因为没人操作而睡眠，放掉（drop）就恢复。拉起的命令盯着本进程
/// （macOS 上 `caffeinate -w`，Linux 上 `systemd-inhibit` 拿着锁跑 `tail --pid`），app 崩了它也跟着退出，
/// 不会一直挡着休眠。
#[cfg(not(windows))]
pub(super) struct Caffeinate(Child);

#[cfg(not(windows))]
impl Caffeinate {
    pub(super) fn start() -> std::io::Result<Self> {
        let pid = std::process::id().to_string();
        let mut command = if cfg!(target_os = "macos") {
            let mut command = Command::new("/usr/bin/caffeinate");
            command.args(["-i", "-w", &pid]);
            command
        } else {
            let mut command = Command::new("systemd-inhibit");
            command.args([
                "--what=idle:sleep",
                "--who=Runode",
                "--why=Runode keeps the computer awake",
                "--mode=block",
            ]);
            command.args(["tail", "-f", "/dev/null", &format!("--pid={pid}")]);
            command
        };
        command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map(Self)
    }

    /// 还在跑时为 `None`。被别人结束了（被信号结束、正常退出）或者问不出来时为 `Ok`，宁可重拉也不假装还拦着
    /// 休眠；以非零退出码退出是没拿到锁（比如 logind 不在、没有权限），为 `Err`，带着退出状态。
    pub(super) fn exit(&mut self) -> Option<Result<(), String>> {
        match self.0.try_wait() {
            Ok(None) => None,
            Ok(Some(status)) if status.code().is_some_and(|code| code != 0) => Some(Err(status.to_string())),
            Ok(Some(_)) | Err(_) => Some(Ok(())),
        }
    }
}

#[cfg(not(windows))]
impl Drop for Caffeinate {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps_lines_become_processes() {
        let exe = "/Applications/My Apps/Runode.app/Contents/MacOS/runode";
        let procs = parse_ps(
            concat!(
                "    1     0  12000   0.0 /sbin/launchd\n",
                "  200     1   2048  12,5 /Applications/My Apps/Runode.app/Contents/MacOS/runode\n",
                "bogus line\n",
                "  300   200   1024   1.5 /Applications/My Apps/Runode.app/Contents/MacOS/runode --speech\n",
                "  400     1   1024   0.0 /Applications/My Apps/Runode.app/Contents/MacOS/runode --screen\n",
                "  500   200   1024   0.0 /usr/local/bin/other --screen\n",
            ),
            exe,
        );
        assert_eq!(procs.len(), 5);
        assert_eq!(procs[&200], Proc { ppid: 1, memory: 2048 * 1024, cpu: 12.5, helper: None });
        assert_eq!(procs[&500].helper, None);
        assert_eq!(Procs::new(procs).helpers(), [(Helper::Speech, 300), (Helper::Screen, 400)]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_killed_caffeinate_counts_as_exited() {
        let mut caffeinate = Caffeinate::start().unwrap();
        assert_eq!(caffeinate.exit(), None);
        caffeinate.0.kill().unwrap();
        caffeinate.0.wait().unwrap();
        assert_eq!(caffeinate.exit(), Some(Ok(())));
    }

    #[cfg(unix)]
    #[test]
    fn a_refused_inhibitor_is_an_error() {
        let mut refused = Caffeinate(Command::new("/bin/sh").args(["-c", "exit 1"]).spawn().unwrap());
        while refused.exit().is_none() {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(matches!(refused.exit(), Some(Err(_))));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn own_footprint_is_readable() {
        assert!(phys_footprint(std::process::id()).is_some_and(|bytes| bytes > 0));
    }

    #[test]
    fn trees_sum_descendants_and_owners_walk_up() {
        let procs = Procs::new(parse_ps("1 0 1 0\n10 1 100 1.0\n11 10 10 2.0\n12 11 1 3.0\n20 1 7 0\n", ""));
        assert_eq!(procs.tree(10), Usage { cpu: 6.0, memory: 111 * 1024 });
        assert_eq!(procs.one(10), Usage { cpu: 1.0, memory: 100 * 1024 });
        assert_eq!(procs.owner(12, |pid| pid == 10), Some(10));
        assert_eq!(procs.owner(10, |pid| pid == 10), Some(10));
        assert_eq!(procs.owner(20, |pid| pid == 10), None);
        // 11 在 10 下面不重复算；20 的父进程是 1 号进程，单独算上，1 号进程本身不算进来。
        assert_eq!(procs.forest(&HashSet::from([10, 11, 20])), Usage { cpu: 6.0, memory: 118 * 1024 });
    }

    #[cfg(unix)]
    #[test]
    fn a_command_that_hangs_is_killed_and_gives_nothing() {
        let started = Instant::now();
        assert_eq!(output(Command::new("/bin/sleep").arg("30"), Duration::from_millis(200)), "");
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        assert_eq!(output(Command::new("/bin/echo").arg("hi"), PROBE_TIMEOUT), "hi\n");
    }

    #[test]
    fn proc_net_tcp_keeps_listening_sockets() {
        let text = concat!(
            "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n",
            "   0: 00000000:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4242 1\n",
            "   1: 0100007F:1435 0100007F:A2B4 01 00000000:00000000 00:00000000 00000000  1000        0 4243 1\n",
            "   2: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 0 1\n",
        );
        assert_eq!(parse_proc_net_tcp(text), HashMap::from([(4242, 3000)]));
    }

    #[test]
    fn lsof_ports_are_deduplicated_and_sorted() {
        let text =
            "p713\ncrapportd\nf11\nn*:49152\nf12\nn*:49152\np731\ncnode\nf9\nn127.0.0.1:5173\nf10\nn[::1]:3000\n";
        let ports = parse_lsof(text);
        let pairs: Vec<_> = ports.iter().map(|port| (port.port, port.pid, port.command.as_str())).collect();
        assert_eq!(pairs, [(3000, 731, "node"), (5173, 731, "node"), (49152, 713, "rapportd")]);
    }
}
