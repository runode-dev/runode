//! 状态栏要问系统的事：各个进程的父进程和 CPU（`ps`）、内存（`proc_pid_rusage`），有哪些 TCP 端口在监听（`lsof`），
//! 以及防止电脑休眠（`caffeinate`）。都是 macOS 自带的命令，跑不了时当没有。

use std::{
    collections::HashMap,
    process::{Child, Command, Stdio},
};

/// 一个进程的父进程、内存（字节，取 phys_footprint）和 CPU 占用（百分比，一个核满载是 100）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Proc {
    pub ppid: u32,
    pub memory: u64,
    pub cpu: f32,
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

    pub(super) fn get(&self, pid: u32) -> Option<&Proc> {
        self.procs.get(&pid)
    }

    /// 只算 `pid` 这一个进程。
    pub(super) fn one(&self, pid: u32) -> Usage {
        self.procs.get(&pid).map_or(Usage::default(), |proc| Usage { cpu: proc.cpu, memory: proc.memory })
    }

    /// `pid` 和它所有的子孙进程加起来。
    pub(super) fn tree(&self, pid: u32) -> Usage {
        let mut total = Usage::default();
        let mut stack = vec![pid];
        while let Some(pid) = stack.pop() {
            total = total + self.one(pid);
            stack.extend(self.children.get(&pid).into_iter().flatten().filter(|&&child| child != pid));
        }
        total
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

/// 一个在监听的 TCP 端口，和监听它的进程。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Port {
    pub port: u16,
    pub pid: u32,
    pub command: String,
}

/// 现在的进程表；`ps` 跑不了时是空的。
pub(super) fn processes() -> Procs {
    let text = output(Command::new("/bin/ps").args(["-axo", "pid=,ppid=,rss=,%cpu="]));
    let mut procs = parse_ps(&text);
    for (&pid, proc) in &mut procs {
        if let Some(footprint) = phys_footprint(pid) {
            proc.memory = footprint;
        }
    }
    Procs::new(procs)
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

#[cfg(not(target_os = "macos"))]
fn phys_footprint(_pid: u32) -> Option<u64> {
    None
}

/// 现在在监听的 TCP 端口，按端口号排好、同一个端口只列一次（IPv4 和 IPv6 各监听一次的那种）。
pub(super) fn listening_ports() -> Vec<Port> {
    let text = output(Command::new("/usr/sbin/lsof").args(["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpcn"]));
    parse_lsof(&text)
}

/// 跑命令拿标准输出；跑不了时是空字符串。`lsof` 有进程读不了时退出码不是 0，输出照样能用。
fn output(command: &mut Command) -> String {
    match command.stdin(Stdio::null()).stderr(Stdio::null()).output() {
        Ok(output) => String::from_utf8_lossy(&output.stdout).into_owned(),
        Err(err) => {
            tracing::debug!("cannot run {command:?}: {err}");
            String::new()
        }
    }
}

/// `ps -axo pid=,ppid=,rss=,%cpu=` 的输出：每行进程号、父进程号、常驻内存（KB）和 CPU 百分比。
/// 常驻内存先填进 `memory`，读得到 phys_footprint 时 `processes` 再换掉。
fn parse_ps(text: &str) -> HashMap<u32, Proc> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let ppid = fields.next()?.parse().ok()?;
            let rss = fields.next()?.parse::<u64>().ok()? * 1024;
            // 有的语言环境里小数点是逗号。
            let cpu = fields.next()?.replace(',', ".").parse().ok()?;
            Some((pid, Proc { ppid, memory: rss, cpu }))
        })
        .collect()
}

/// `lsof -F pcn` 的输出：`p` 行开始一个进程，跟着 `c` 行是进程名，再是它的各个文件，`n` 行是地址，
/// 比如 `*:3000`、`127.0.0.1:5173`、`[::1]:8080`。
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

/// 防止电脑闲置时休眠：拿着期间系统不会因为没人操作而睡眠，放掉（drop）就恢复。`caffeinate -w`
/// 盯着本进程，app 崩了它也跟着退出，不会一直挡着休眠。
pub(super) struct Caffeinate(Child);

impl Caffeinate {
    pub(super) fn start() -> std::io::Result<Self> {
        Command::new("/usr/bin/caffeinate")
            .args(["-i", "-w", &std::process::id().to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(Self)
    }
}

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
        let procs =
            parse_ps("    1     0  12000   0.0\n  200     1   2048  12,5\nbogus line\n  300   200   1024   1.5\n");
        assert_eq!(procs.len(), 3);
        assert_eq!(procs[&200], Proc { ppid: 1, memory: 2048 * 1024, cpu: 12.5 });
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn own_footprint_is_readable() {
        assert!(phys_footprint(std::process::id()).is_some_and(|bytes| bytes > 0));
    }

    #[test]
    fn trees_sum_descendants_and_owners_walk_up() {
        let procs = Procs::new(parse_ps("1 0 1 0\n10 1 100 1.0\n11 10 10 2.0\n12 11 1 3.0\n20 1 7 0\n"));
        assert_eq!(procs.tree(10), Usage { cpu: 6.0, memory: 111 * 1024 });
        assert_eq!(procs.one(10), Usage { cpu: 1.0, memory: 100 * 1024 });
        assert_eq!(procs.owner(12, |pid| pid == 10), Some(10));
        assert_eq!(procs.owner(10, |pid| pid == 10), Some(10));
        assert_eq!(procs.owner(20, |pid| pid == 10), None);
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
