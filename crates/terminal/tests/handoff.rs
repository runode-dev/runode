//! 把一个真 shell 的 PTY 交出去、经 socket 传过去、在另一端接上：shell 不中断，输入输出、
//! 改尺寸和前台进程照常，丢掉接手的一端会结束 shell，shell 退出时接手的一端能发现。

use std::{
    os::{
        fd::{AsFd as _, AsRawFd as _, BorrowedFd},
        unix::net::UnixStream,
    },
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use runode_shared_types::{grid::GridSize, settings::TermSettings, shell::IntegrationMode};
use runode_terminal::{
    fd_passing::{recv_with_fds, send_with_fds},
    host_session::HostSession,
    pty::{Pty, PtyEvent, PtyHandoff, PtySink},
};

const SIZE: GridSize = GridSize { cols: 80, rows: 24, cell_width_px: 8, cell_height_px: 16 };
const TIMEOUT: Duration = Duration::from_secs(10);

/// 收一个 `Pty` 的输出。
struct Output {
    rx: mpsc::Receiver<PtyEvent>,
    text: String,
    exited: bool,
}

fn output() -> (PtySink, Output) {
    let (tx, rx) = mpsc::channel();
    (Box::new(move |event| tx.send(event).is_ok()), Output { rx, text: String::new(), exited: false })
}

impl Output {
    fn take(&mut self, event: PtyEvent) {
        match event {
            PtyEvent::Output(data) => self.text.push_str(&String::from_utf8_lossy(&data)),
            PtyEvent::Exited => self.exited = true,
        }
    }

    /// 已经收到的都收下，不等。
    fn drain(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            self.take(event);
        }
    }

    fn wait_until(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        let until = Instant::now() + TIMEOUT;
        while !done(self) {
            let left = until.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(event) => self.take(event),
                Err(_) => panic!("timed out waiting for {what}; got {:?}", self.text),
            }
        }
    }

    fn wait_for(&mut self, needle: &str) {
        self.wait_until(needle, |output| output.text.contains(needle));
    }

    fn wait_exit(&mut self) {
        self.wait_until("the exit", |output| output.exited);
    }
}

/// 起一个交互式的 dash。不用 /bin/sh（macOS 上是 bash 3.2）：它在两条命令之间偶尔会把终端
/// 尺寸改回旧的，刚改完尺寸就 `stty size` 时读到的不一定是新尺寸，没有交接也一样。
fn shell() -> (Pty, Output) {
    let (sink, output) = output();
    let pty = Pty::spawn(SIZE, Some("/bin/dash"), None, IntegrationMode::Off, sink).unwrap();
    (pty, output)
}

fn alive(pid: u32) -> bool {
    // SAFETY: 信号 0 不发信号，只检查进程在不在。
    unsafe { libc::kill(libc::pid_t::try_from(pid).unwrap(), 0) == 0 }
}

fn eventually(what: &str, check: impl Fn() -> bool) {
    let until = Instant::now() + TIMEOUT;
    while !check() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn winsize(master: BorrowedFd<'_>) -> (u16, u16) {
    let mut size = libc::winsize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: `master` 开着；`TIOCGWINSZ` 只往传进去的结构里写。
    assert_eq!(unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCGWINSZ, &raw mut size) }, 0);
    (size.ws_col, size.ws_row)
}

/// 经 socketpair 把交出来的会话传到「另一端」：master 走 `SCM_RIGHTS`，其余字段编成一行文字，
/// 没写出去的输入原样跟在后面。交出方自己那份描述符随后就关掉。
fn send_across(handoff: PtyHandoff) -> PtyHandoff {
    let (a, b) = UnixStream::pair().unwrap();
    let mut message = format!(
        "{} {} {} {} {}\n",
        handoff.pid, handoff.size.cols, handoff.size.rows, handoff.size.cell_width_px, handoff.size.cell_height_px
    )
    .into_bytes();
    message.extend_from_slice(&handoff.pending_input);
    // 消息可能比 socket 的缓冲大，另起线程发。
    let sender = thread::spawn(move || {
        send_with_fds(&a, &message, &[handoff.master.as_fd()]).unwrap();
        drop(handoff);
    });
    let (data, mut fds) = recv_with_fds(&b).unwrap();
    sender.join().unwrap();
    let newline = data.iter().position(|&b| b == b'\n').unwrap();
    let pending_input = data[newline + 1..].to_vec();
    let fields: Vec<u32> =
        std::str::from_utf8(&data[..newline]).unwrap().split(' ').map(|f| f.parse().unwrap()).collect();
    let [pid, cols, rows, cell_width_px, cell_height_px] = fields[..] else { panic!("bad fields {fields:?}") };
    let size = GridSize {
        cols: u16::try_from(cols).unwrap(),
        rows: u16::try_from(rows).unwrap(),
        cell_width_px: u16::try_from(cell_width_px).unwrap(),
        cell_height_px: u16::try_from(cell_height_px).unwrap(),
    };
    assert_eq!(fds.len(), 1);
    PtyHandoff { master: fds.remove(0), pid, size, report_token: None, pending_input }
}

/// 问 shell 自己的进程号。
fn shell_pid(pty: &Pty, output: &mut Output) -> u32 {
    // 回显的命令里是 `%s`，输出里才是数字。
    pty.writer.write(b"printf 'pid:%s:end\\n' $$\n");
    output.wait_until("the pid", |output| output.text.contains(":end\r"));
    let start = output.text.rfind("pid:").unwrap() + 4;
    let end = start + output.text[start..].find(":end").unwrap();
    output.text[start..end].parse().unwrap()
}

#[test]
fn a_handed_over_shell_keeps_running() {
    let (mut old, mut old_output) = shell();
    old.writer.write(b"echo before-$((1+1))\n");
    old_output.wait_for("before-2");

    // 交出前刚敲的命令：写线程写完才交出，输出由哪边读到都行，但一个字节也不能丢。
    old.writer.write(b"echo queued-$((3+4))\n");
    old.stop_reading();
    let handoff = old.release().unwrap();
    let pid = handoff.pid;
    assert_eq!(handoff.size, SIZE);
    // 交出后旧的一端不再管这个会话：改尺寸不碰 PTY，写进去的丢掉，丢掉它也不结束 shell。
    assert!(!old.foreground_is_shell());
    old.resize(GridSize { cols: 120, rows: 50, ..SIZE });
    assert_eq!(winsize(handoff.master.as_fd()), (SIZE.cols, SIZE.rows));
    old.writer.write(b"echo lost-$((4+4))\n");
    assert!(old.release().is_err(), "a pty can be handed off only once");
    drop(old);
    let (sink, mut new_output) = output();
    let new = Pty::adopt(send_across(handoff), sink).unwrap();
    assert!(new.started());

    // 交出的一方全关掉了，shell 还活着。
    thread::sleep(Duration::from_millis(200));
    assert!(alive(pid));
    old_output.drain();
    assert!(!old_output.exited, "the old side must not report an exit");
    assert!(!old_output.text.contains("lost-8"));
    new_output.wait_until("queued-7", |new| old_output.text.contains("queued-7") || new.text.contains("queued-7"));

    new.writer.write(b"echo after-$((2+3))\n");
    new_output.wait_for("after-5");

    new.resize(GridSize { cols: 100, rows: 40, ..SIZE });
    new.writer.write(b"stty size\n");
    new_output.wait_for("40 100");

    // 前台进程组照常读得到。
    eventually("the shell in the foreground", || new.foreground_is_shell());
    new.writer.write(b"sleep 30\n");
    eventually("sleep in the foreground", || !new.foreground_is_shell());
    new.writer.write(b"\x03");
    eventually("the shell back in the foreground", || new.foreground_is_shell());

    assert!(!new_output.text.contains("lost-8"));
    new.writer.write(b"exit\n");
    new_output.wait_exit();
    eventually("the shell to be gone", || !alive(pid));
}

#[test]
fn dropping_the_adopted_pty_ends_the_shell() {
    let (mut old, mut old_output) = shell();
    old.writer.write(b"echo ready\n");
    old_output.wait_for("ready");
    let handoff = old.release().unwrap();
    drop(old);
    let pid = handoff.pid;
    let (sink, mut new_output) = output();
    let new = Pty::adopt(send_across(handoff), sink).unwrap();
    // 前台跑着一个程序，它也要一起结束。
    new.writer.write(b"sleep 30\n");
    eventually("sleep in the foreground", || !new.foreground_is_shell());
    drop(new);
    new_output.wait_exit();
    eventually("the shell to be gone", || !alive(pid));
}

/// 交出之后、接手之前 shell 就退出了：接手的一端马上发现。
#[test]
fn a_shell_that_exited_before_adoption_is_noticed() {
    let (mut old, mut old_output) = shell();
    old.writer.write(b"echo ready\n");
    old_output.wait_for("ready");
    let handoff = old.release().unwrap();
    drop(old);
    let pid = handoff.pid;
    // SAFETY: 只是给测试自己起的 shell 发信号。
    unsafe { libc::kill(libc::pid_t::try_from(pid).unwrap(), libc::SIGKILL) };
    // 不等它被回收再接：PTY 里还有没读的输出时，shell 退出关终端看来要等输出被读走，没人读
    // master 时偶尔十几秒都没被回收；接手的一端读了就走得完。
    thread::sleep(Duration::from_millis(200));
    let (sink, mut new_output) = output();
    let _new = Pty::adopt(send_across(handoff), sink).unwrap();
    new_output.wait_exit();
    eventually("the shell to be gone", || !alive(pid));
}

/// 宿主那份会话按交接的顺序走一遍：叫停读线程、喂完收到的输出、编快照、交出 PTY。交出后
/// 丢掉会话不结束 shell，接手的一端照常用。
#[test]
fn a_host_session_hands_its_pty_over() {
    let (pty, mut old_output) = shell();
    let mut session = HostSession::new(SIZE, pty, None, &TermSettings::default()).unwrap();
    session.write(b"echo ready\n".to_vec());
    old_output.wait_for("ready");
    session.stop_reading();
    eventually("the reader to finish", || session.pty_reader_finished());
    old_output.drain();
    session.feed(old_output.text.as_bytes());
    let handoff = session.release_pty().unwrap();
    let pid = handoff.pid;
    assert!(session.screen_text(None).unwrap().contains("ready"));
    assert!(session.snapshot().is_ok());
    // 交出后改尺寸只改这边的 VT。
    assert!(session.resize(GridSize { cols: 120, rows: 50, ..SIZE }));
    assert_eq!(winsize(handoff.master.as_fd()), (SIZE.cols, SIZE.rows));
    drop(session);
    thread::sleep(Duration::from_millis(200));
    assert!(alive(pid));
    let (sink, mut new_output) = output();
    let new = Pty::adopt(send_across(handoff), sink).unwrap();
    new.writer.write(b"echo still-$((1+2))\n");
    new_output.wait_for("still-3");
    new.writer.write(b"exit\n");
    new_output.wait_exit();
}

/// 叫停读线程后不交了，接着读：输出还交给原来的 `PtySink`，停着的时候写的也不丢。
#[test]
fn a_host_session_resumes_reading() {
    let (pty, mut out) = shell();
    let mut session = HostSession::new(SIZE, pty, None, &TermSettings::default()).unwrap();
    session.write(b"echo ready\n".to_vec());
    out.wait_for("ready");
    session.stop_reading();
    eventually("the reader to finish", || session.pty_reader_finished());
    session.write(b"echo resumed-$((5+5))\n".to_vec());
    thread::sleep(Duration::from_millis(300));
    out.drain();
    assert!(!out.text.contains("resumed-10"), "a stopped reader must not read");
    session.resume_reading().unwrap();
    assert!(!session.pty_reader_finished());
    out.wait_for("resumed-10");
}

/// 程序不读输入、写线程写不进去时，交出不等它，没写出去的输入跟着交过去，由接手的一端先写：
/// 一个字节都不丢，也不和之后的输入交错。
#[test]
fn input_the_program_has_not_read_is_handed_over() {
    let (mut old, mut old_output) = shell();
    old.writer.write(b"stty raw -echo; sleep 2; head -c 70000 | wc -c; stty sane\n");
    eventually("sleep in the foreground", || old.foreground_title(|| None).as_deref() == Some("sleep"));
    old.writer.send(vec![b'x'; 70000]);
    // 让写线程写满终端的输入队列、卡住。
    thread::sleep(Duration::from_millis(300));
    let started = Instant::now();
    let handoff = old.release().unwrap();
    assert!(started.elapsed() < Duration::from_secs(1), "release waited {:?}", started.elapsed());
    let pending = handoff.pending_input.len();
    assert!(pending > 0 && pending < 70000, "{pending} bytes pending");
    drop(old);
    old_output.drain();
    let (sink, mut new_output) = output();
    let new = Pty::adopt(send_across(handoff), sink).unwrap();
    new_output.wait_for("70000");
    new.writer.write(b"exit\n");
    new_output.wait_exit();
}

/// 读线程按额度限流、会卡在 `PtySink` 里时，按「叫停、一边处理收到的输出一边等读线程结束、
/// 交出」走：不死锁，交出前后收到的输出接起来一行不缺。
#[test]
fn a_throttled_reader_is_stopped_without_losing_output() {
    const LINES: u32 = 30000;
    // 读线程每交一块输出要一个额度，测试线程处理完一块才还一个。
    let credits = Arc::new((Mutex::new(1usize), Condvar::new()));
    let (tx, rx) = mpsc::channel();
    let sink: PtySink = {
        let credits = credits.clone();
        Box::new(move |event| {
            let (available, freed) = &*credits;
            let mut available = available.lock().unwrap();
            while *available == 0 {
                available = freed.wait(available).unwrap();
            }
            *available -= 1;
            tx.send(event).is_ok()
        })
    };
    let give_back = || {
        let (available, freed) = &*credits;
        *available.lock().unwrap() += 1;
        freed.notify_all();
    };
    let mut before = String::new();
    let mut take = |event: PtyEvent| {
        if let PtyEvent::Output(data) = event {
            before.push_str(&String::from_utf8_lossy(&data));
        }
    };
    let mut old = Pty::spawn(SIZE, Some("/bin/dash"), None, IntegrationMode::Off, sink).unwrap();
    old.writer.write(format!("seq 1 {LINES}; echo DONE\n").as_bytes());
    // 读到一部分输出后再叫停，这时读线程多半正等着额度。
    let mut chunks = 0;
    while chunks < 20 {
        take(rx.recv_timeout(TIMEOUT).unwrap());
        give_back();
        chunks += 1;
    }
    old.stop_reading();
    while !old.reader_finished() {
        if let Ok(event) = rx.recv_timeout(Duration::from_millis(20)) {
            take(event);
            give_back();
        }
    }
    while let Ok(event) = rx.try_recv() {
        take(event);
    }
    let handoff = old.release().unwrap();
    drop(old);
    let (sink, mut new_output) = output();
    let new = Pty::adopt(send_across(handoff), sink).unwrap();
    new_output.wait_for("\nDONE");
    let all = before + &new_output.text;
    let numbers: Vec<u32> = all
        .split('\n')
        // 敲的命令回显得比 dash 印提示符早时，第一行输出跟在提示符后面。
        .map(|line| line.trim_end_matches('\r').trim_start_matches("$ "))
        .filter(|line| !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit()))
        .map(|line| line.parse().unwrap())
        .collect();
    assert_eq!(numbers, (1..=LINES).collect::<Vec<_>>());
    new.writer.write(b"exit\n");
    new_output.wait_exit();
}

/// 接手失败时交来的东西原样还回来，描述符没关，shell 还活着，改好了可以再接。
#[test]
fn a_failed_adoption_gives_the_handoff_back() {
    let (mut old, mut old_output) = shell();
    old.writer.write(b"echo ready\n");
    old_output.wait_for("ready");
    let mut handoff = old.release().unwrap();
    drop(old);
    let pid = handoff.pid;
    handoff.pid = 0;
    let (sink, _unused) = output();
    let mut handoff = match Pty::adopt(handoff, sink) {
        Ok(_) => panic!("adopting pid 0 must fail"),
        Err(err) => err.handoff,
    };
    thread::sleep(Duration::from_millis(200));
    assert!(alive(pid));
    handoff.pid = pid;
    let (sink, mut new_output) = output();
    let new = Pty::adopt(handoff, sink).unwrap();
    new.writer.write(b"echo again-$((6+6))\n");
    new_output.wait_for("again-12");
    new.writer.write(b"exit\n");
    new_output.wait_exit();
}

/// 没交出去的 `Pty` 照常丢掉：shell 被结束，也被回收了（`kill(pid, 0)` 对还没回收的僵尸也成功，
/// 回收后才失败）。
#[test]
fn dropping_a_pty_that_was_not_handed_off_ends_and_reaps_the_shell() {
    let (pty, mut out) = shell();
    let pid = shell_pid(&pty, &mut out);
    assert!(alive(pid));
    drop(pty);
    eventually("the shell to be ended and reaped", || !alive(pid));
}

#[test]
fn an_unstarted_pty_cannot_be_released() {
    let (sink, _output) = output();
    let mut pty = Pty::open(SIZE, sink).unwrap();
    assert!(pty.reader_finished());
    assert!(pty.release().is_err());
    // 出错时什么都没动，照常可用。
    assert!(!pty.started());
}
