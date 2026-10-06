//! 把一个真 shell 的 PTY 交出去、经 socket 传过去、在另一端接上：shell 不中断，输入输出、
//! 改尺寸和前台进程照常，丢掉接手的一端会结束 shell，shell 退出时接手的一端能发现。

use std::{
    os::{fd::AsFd as _, unix::net::UnixStream},
    sync::mpsc,
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

/// 经 socketpair 把交出来的会话传到「另一端」：master 走 `SCM_RIGHTS`，其余字段编成文字。
/// 交出方自己那份描述符随后就关掉。
fn send_across(handoff: PtyHandoff) -> PtyHandoff {
    let (a, b) = UnixStream::pair().unwrap();
    let fields = format!(
        "{} {} {} {} {}",
        handoff.pid, handoff.size.cols, handoff.size.rows, handoff.size.cell_width_px, handoff.size.cell_height_px
    );
    send_with_fds(&a, fields.as_bytes(), &[handoff.master.as_fd()]).unwrap();
    drop(handoff);
    let (data, mut fds) = recv_with_fds(&b).unwrap();
    let fields: Vec<u32> = String::from_utf8(data).unwrap().split(' ').map(|f| f.parse().unwrap()).collect();
    let [pid, cols, rows, cell_width_px, cell_height_px] = fields[..] else { panic!("bad fields {fields:?}") };
    let size = GridSize {
        cols: u16::try_from(cols).unwrap(),
        rows: u16::try_from(rows).unwrap(),
        cell_width_px: u16::try_from(cell_width_px).unwrap(),
        cell_height_px: u16::try_from(cell_height_px).unwrap(),
    };
    assert_eq!(fds.len(), 1);
    PtyHandoff { master: fds.remove(0), pid, size, report_token: None }
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
    // 交出后旧的一端不再管这个会话：写进去的丢掉，丢掉它也不结束 shell。
    assert!(!old.foreground_is_shell());
    old.writer.write(b"echo lost-$((4+4))\n");
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
    // SAFETY: 只是给测试自己起的 shell 发信号。
    unsafe { libc::kill(libc::pid_t::try_from(handoff.pid).unwrap(), libc::SIGKILL) };
    eventually("the shell to be gone", || !alive(handoff.pid));
    let (sink, mut new_output) = output();
    let _new = Pty::adopt(send_across(handoff), sink).unwrap();
    new_output.wait_exit();
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

#[test]
fn an_unstarted_pty_cannot_be_released() {
    let (sink, _output) = output();
    let mut pty = Pty::open(SIZE, sink).unwrap();
    assert!(pty.reader_finished());
    assert!(pty.release().is_err());
    // 出错时什么都没动，照常可用。
    assert!(!pty.started());
}
