//! 状态栏和远端的项目服务（`project_host::serve`）要问系统的事：各个进程的父进程和 CPU（`ps`）、内存（`proc_pid_rusage`），有哪些 TCP 端口在监听（`lsof`），
//! 以及防止电脑休眠（`caffeinate`）。都是 macOS 自带的命令，跑不了时当没有。Linux 上端口读 `/proc`，防休眠经
//! `systemd-inhibit`；Windows 上这几样由 `windows` 经系统接口问。

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub(crate) use windows::listening_ports;
#[cfg(windows)]
pub(crate) use windows::{Caffeinate, processes};

use std::collections::{HashMap, HashSet};
#[cfg(not(windows))]
use std::{
    process::{Child, Command, Stdio},
    time::Duration,
};

/// 状态栏隔多久重新问一次系统；Windows 和 Linux 上算 CPU 时据此认出太旧的采样。
pub(crate) const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

/// 比这更早的采样不拿来算 CPU，这一轮当作没有上一次：隔得太久的两次采样做差，算出的是那整段时间的平均值，
/// 不是现在的占用。
#[cfg(any(windows, all(unix, not(target_os = "macos")), test))]
const CPU_SAMPLE_MAX_AGE: std::time::Duration = POLL_INTERVAL.saturating_mul(2);

/// `ps`、`lsof` 最多跑这么久，超时当这次没数据：卡住一次不能让状态栏的轮询（连同防休眠）停下。
#[cfg(not(windows))]
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// 一个进程的父进程、内存（字节，macOS 上取 phys_footprint，Windows 上取私有工作集）和 CPU 占用（百分比，一个核满载是 100）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Proc {
    pub ppid: u32,
    pub memory: u64,
    pub cpu: f32,
    pub helper: Option<Helper>,
}

/// runode 按需拉起的子进程，按命令行认：这个 app 的可执行文件加一个参数。采集进程和拉起它的进程脱开，
/// 不一定在 app 和宿主的子孙里，所以在整张进程表里认。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Helper {
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
pub(crate) struct Usage {
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
pub(crate) struct Procs {
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
    pub(crate) fn helpers(&self) -> Vec<(Helper, u32)> {
        let mut helpers: Vec<_> = self.procs.iter().filter_map(|(&pid, proc)| Some((proc.helper?, pid))).collect();
        helpers.sort_unstable();
        helpers
    }

    /// 只算 `pid` 这一个进程。
    pub(crate) fn one(&self, pid: u32) -> Usage {
        self.procs.get(&pid).map_or(Usage::default(), |proc| Usage { cpu: proc.cpu, memory: proc.memory })
    }

    /// `pid` 和它所有的子孙进程加起来。
    pub(crate) fn tree(&self, pid: u32) -> Usage {
        self.subtree(pid).into_iter().fold(Usage::default(), |total, pid| total + self.one(pid))
    }

    /// `roots` 里各个进程连同子孙加起来；一个进程在另一个的子孙里时只算一次。
    pub(crate) fn forest(&self, roots: &HashSet<u32>) -> Usage {
        roots
            .iter()
            .filter(|&&pid| {
                let parent = self.procs.get(&pid).map(|proc| proc.ppid);
                parent.and_then(|ppid| self.owner(ppid, |pid| roots.contains(&pid))).is_none()
            })
            .fold(Usage::default(), |total, &pid| total + self.tree(pid))
    }

    /// 沿父进程往上找，`pid` 自己或者最近的一个让 `is_root` 成立的祖先；找到 1 号进程还没有时为 `None`。
    pub(crate) fn owner(&self, pid: u32, is_root: impl Fn(u32) -> bool) -> Option<u32> {
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

/// 一个在监听的 TCP 端口，和监听它的进程。同一个端口号可以有几个进程分别在不同的地址上监听，各是一个。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Port {
    pub port: u16,
    pub pid: u32,
    pub command: String,
}

/// 现在的进程表；`ps` 跑不了时是空的。phys_footprint 只问 `roots` 和它们的子孙：状态栏只算这些进程的内存，
/// 别的进程只用来认父子关系，`ps` 列出的几百个进程挨个问一遍白白耗时。
#[cfg(not(windows))]
pub(crate) fn processes(roots: &HashSet<u32>) -> Procs {
    let text = output(Command::new("/bin/ps").args(["-axo", "pid=,ppid=,rss=,%cpu=,args="]), PROBE_TIMEOUT);
    let exe = runode_paths::runode_exe().map(|exe| exe.to_string_lossy().into_owned()).unwrap_or_default();
    #[cfg_attr(not(target_os = "linux"), expect(unused_mut, reason = "只有 Linux 上要去掉 AppImage 的运行时"))]
    let mut parsed = parse_ps(&text, &exe);
    #[cfg(target_os = "linux")]
    drop_appimage_runtimes(&mut parsed);
    let mut procs = Procs::new(parsed);
    let used: HashSet<u32> = roots
        .iter()
        .flat_map(|&root| procs.subtree(root))
        .chain(procs.helpers().into_iter().map(|(_, pid)| pid))
        .collect();
    #[cfg(not(target_os = "macos"))]
    sample_cpu(&mut procs, &used);
    for pid in used {
        if let (Some(proc), Some(footprint)) = (procs.procs.get_mut(&pid), phys_footprint(pid)) {
            proc.memory = footprint;
        }
    }
    procs
}

/// 从 AppImage 跑时，运行时 fork 出来给镜像供文件的 FUSE 进程和它服务的 runode 进程命令行一模一样，不能当成
/// runode 的子进程：它的可执行文件是 AppImage 文件本身，runode 自己的在挂载点里。
#[cfg(target_os = "linux")]
fn drop_appimage_runtimes(procs: &mut HashMap<u32, Proc>) {
    let Some(image) = runode_paths::appimage() else { return };
    for (pid, proc) in procs.iter_mut() {
        if proc.helper.is_some() && std::fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|exe| exe == image) {
            proc.helper = None;
        }
    }
}

/// 一个进程的启动时刻、累计 CPU 时间（都按时钟滴答计）和问的时刻。
#[cfg(any(all(unix, not(target_os = "macos")), test))]
type CpuSample = (u64, u64, std::time::Instant);

/// 上一次问到的各进程，下一次据此算出这段时间的 CPU 占用。
#[cfg(all(unix, not(target_os = "macos")))]
static CPU_TIMES: std::sync::LazyLock<std::sync::Mutex<HashMap<u32, CpuSample>>> =
    std::sync::LazyLock::new(Default::default);

/// 把 `used` 里各进程的 CPU 换成上一次问到现在这段时间的占用。procps 的 `%cpu` 是进程整个寿命里的平均：跑了很久的
/// 进程忙起来还显示近乎 0，闲下来又一直像在忙。读不到 `/proc/<pid>/stat`、没有上一次的采样时为 0。
#[cfg(all(unix, not(target_os = "macos")))]
fn sample_cpu(procs: &mut Procs, used: &HashSet<u32>) {
    // SAFETY: 只问一个配置值。
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks <= 0 {
        return;
    }
    let now = std::time::Instant::now();
    let mut samples = CPU_TIMES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    // 不同的调用方问的进程不同，各自的采样都留着，只扔掉太旧的。
    samples.retain(|_, &mut (_, _, when)| now.duration_since(when) <= CPU_SAMPLE_MAX_AGE);
    for &pid in used {
        let Some(proc) = procs.procs.get_mut(&pid) else { continue };
        let Some((started, cpu_time)) =
            std::fs::read_to_string(format!("/proc/{pid}/stat")).ok().and_then(|stat| parse_proc_stat(&stat))
        else {
            proc.cpu = 0.;
            continue;
        };
        let sample = (started, cpu_time, now);
        proc.cpu = cpu_since(samples.insert(pid, sample), sample, ticks as f64);
    }
}

/// `/proc/<pid>/stat` 里的启动时刻和累计 CPU 时间（用户态加内核态），都按时钟滴答计。进程名在括号里，可以带空格和
/// 括号，从最后一个右括号往后数字段。
#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn parse_proc_stat(stat: &str) -> Option<(u64, u64)> {
    // 右括号后面从第 3 个字段（状态）数起：utime、stime 是第 14、15 个，starttime 是第 22 个。
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    let field = |n: usize| fields.get(n - 3)?.parse::<u64>().ok();
    Some((field(22)?, field(14)?.checked_add(field(15)?)?))
}

/// 从 `before` 到 `now` 这段时间的 CPU 占用（百分比，一个核满载是 100），`units` 是一秒多少个 CPU 时间单位。
/// 没有上一次、进程号换了进程（启动时刻不同）、太旧的采样时为 0。
#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn cpu_since(before: Option<CpuSample>, now: CpuSample, units: f64) -> f32 {
    let (started, cpu_time, at) = now;
    match before {
        Some((before_started, before, when))
            if before_started == started
                && cpu_time >= before
                && at.duration_since(when) <= CPU_SAMPLE_MAX_AGE
                && at > when =>
        {
            ((cpu_time - before) as f64 / units / at.duration_since(when).as_secs_f64() * 100.) as f32
        }
        _ => 0.,
    }
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

/// 现在在监听的 TCP 端口，按端口号排好、同一个进程的同一个端口只列一次（IPv4 和 IPv6 各监听一次的那种）。
#[cfg(target_os = "macos")]
pub(crate) fn listening_ports() -> Vec<Port> {
    let text = output(Command::new("/usr/sbin/lsof").args(["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpcn"]), PROBE_TIMEOUT);
    parse_lsof(&text)
}

/// 跑命令拿标准输出（`subprocess::output_within`）；跑不了、`timeout` 内没跑完时是空字符串。`lsof` 有进程读不了时
/// 退出码不是 0，输出照样能用。
#[cfg(not(windows))]
fn output(command: &mut Command, timeout: Duration) -> String {
    match crate::subprocess::output_within(command, timeout) {
        Ok(output) => String::from_utf8_lossy(&output.stdout).into_owned(),
        Err(crate::subprocess::RunError::Timeout) => {
            tracing::warn!("{command:?} did not finish in {timeout:?}, ended it");
            String::new()
        }
        Err(err) => {
            tracing::debug!("cannot run {command:?}: {err:?}");
            String::new()
        }
    }
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

/// 现在在监听的 TCP 端口，按端口号排好、同一个进程的同一个端口只列一次。只认得出本用户进程的端口：别人的进程的
/// 描述符读不了，和不带 sudo 的 `lsof` 一样。
#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn listening_ports() -> Vec<Port> {
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
            if ports.iter().any(|known| known.port == port && known.pid == pid) {
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
                if pid != 0 && !ports.iter().any(|known| known.port == port && known.pid == pid) {
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
pub(crate) struct Caffeinate(Child);

#[cfg(not(windows))]
impl Caffeinate {
    pub(crate) fn start() -> std::io::Result<Self> {
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
    pub(crate) fn exit(&mut self) -> Option<Result<(), String>> {
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
    use std::time::Duration;
    #[cfg(unix)]
    use std::time::Instant;

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

    /// 进程名里带空格和括号也从最后一个右括号往后数。
    #[test]
    fn proc_stat_gives_start_and_cpu_time() {
        let stat = "4242 (my (odd) name) S 1 4242 4242 0 -1 4194304 100 0 0 0 250 50 0 0 20 0 1 0 98765 1000 10 \
                    18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0 0 0";
        assert_eq!(parse_proc_stat(stat), Some((98765, 300)));
        assert_eq!(parse_proc_stat("4242 (cut"), None);
    }

    /// 两次采样之间用了多少 CPU；换了进程、太旧、头一次时为 0。
    #[test]
    fn cpu_is_measured_between_samples() {
        let start = std::time::Instant::now();
        let later = start + Duration::from_secs(2);
        assert_eq!(cpu_since(Some((7, 100, start)), (7, 300, later), 100.), 100.);
        assert_eq!(cpu_since(Some((7, 100, start)), (7, 150, later), 100.), 25.);
        assert_eq!(cpu_since(None, (7, 300, later), 100.), 0.);
        assert_eq!(cpu_since(Some((6, 100, start)), (7, 300, later), 100.), 0.);
        let stale = start + CPU_SAMPLE_MAX_AGE + Duration::from_secs(1);
        assert_eq!(cpu_since(Some((7, 100, start)), (7, 300, stale), 100.), 0.);
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

    /// 同一个进程的同一个端口只列一次；别的进程在另一个地址上监听同一个端口时也列出来。
    #[test]
    fn lsof_ports_are_deduplicated_and_sorted() {
        let text = concat!(
            "p713\ncrapportd\nf11\nn*:49152\nf12\nn*:49152\n",
            "p731\ncnode\nf9\nn127.0.0.1:5173\nf10\nn[::1]:3000\n",
            "p800\ncpython\nf3\nn127.0.0.1:3000\n",
        );
        let ports = parse_lsof(text);
        let pairs: Vec<_> = ports.iter().map(|port| (port.port, port.pid, port.command.as_str())).collect();
        assert_eq!(pairs, [(3000, 731, "node"), (3000, 800, "python"), (5173, 731, "node"), (49152, 713, "rapportd")]);
    }
}
