//! `runode remote …`：给手机配对远程访问（`pair`）、列出（`devices`）和撤销（`revoke`）配对过的设备。
//! 不经宿主，读写 `runode_remote` 管的文件：监听方（宿主所在的进程）开着时才能配对，列和
//! 撤销不用它开着。配置里还没开 `remote-access` 时，`pair` 替用户写上再等监听方起来。配对成了以后，`terminal-host` 还没开时问一句要不要开，免得退出 app 后远程访问
//! 跟着停掉、手机连不上。

use std::{
    io::{BufRead as _, IsTerminal as _, Write},
    net::IpAddr,
    sync::atomic::{AtomicI32, Ordering},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, anyhow, bail};
use qrcode::{Color, EcLevel, QrCode};
use runode_config::ConfigFile;
use runode_protocol::remote::PAIRING_TTL;
use runode_remote::{
    Device, ListenerStatus, PairingProgress, PairingTicket, list_devices, listener_status, now_unix, revoke_device,
};

use crate::{
    Env,
    commands::{plain, table},
};

/// 等配对时隔多久看一眼口令文件。
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// 等配对时隔这么多次看一眼监听方还在不在。
const STATUS_EVERY: u32 = 8;
/// 二维码四周留白的模块数。规范要 4 个，2 个手机照样扫得出，画出来窄一些。
const QUIET_ZONE: usize = 2;
/// 退出 app 后宿主留在后台的配置项，见 `offer_background`。
const BACKGROUND_KEY: &str = "terminal-host";
/// 开远程访问的配置项，见 `turn_on`。
const REMOTE_KEY: &str = "remote-access";
/// `turn_on` 写完配置后等监听方起来的最长时间：app 和单独跑的宿主每秒看一眼配置文件。
pub(crate) const LISTENER_WAIT: Duration = Duration::from_secs(10);

/// `answer` 读用户对「要不要在后台跑」的回答，见 `offer_background`；`listener_wait` 是 `turn_on` 等监听方的时间。
pub(crate) fn pair(
    env: &Env,
    extra: &[IpAddr],
    out: &mut dyn Write,
    answer: &mut dyn FnMut() -> Option<String>,
    listener_wait: Duration,
) -> anyhow::Result<()> {
    let status = match listener_status(&env.dirs).context("cannot tell whether remote access is on")? {
        Some(status) => status,
        None => turn_on(env, out, listener_wait)?,
    };
    // 在 `ticket` 之前建，丢的时候 `ticket` 先删掉口令文件、再放开信号。
    let signals = Signals::catch();
    let ticket = PairingTicket::begin(&env.dirs, PAIRING_TTL).context("cannot write the pairing code")?;
    let uri = ticket.uri(&status, extra)?.to_string();
    out.write_all(qr_code(&uri)?.as_bytes())?;
    writeln!(out, "\n{uri}\n")?;
    writeln!(
        out,
        "Scan the code with runode on your phone, or open the link there, within {} minutes. \
         Waiting for the phone; Ctrl-C stops waiting.",
        PAIRING_TTL.as_secs() / 60
    )?;
    out.flush()?;
    let mut polls = 0u32;
    loop {
        match ticket.poll().context("cannot read the pairing code")? {
            PairingProgress::Waiting => {}
            PairingProgress::Paired { device_id, name } => {
                // 后面还要问用户，Ctrl-C 得照常打断。
                drop(signals);
                writeln!(out, "Paired with {name} ({device_id}).")?;
                return offer_background(env, out, answer);
            }
            PairingProgress::Expired => bail!("the pairing code expired before a phone used it"),
            PairingProgress::Replaced => bail!("another `runode remote pair` replaced this pairing code"),
        }
        if let Some(signal) = signals.caught() {
            return Err(Interrupted(signal).into());
        }
        polls += 1;
        if polls.is_multiple_of(STATUS_EVERY) && listener_status(&env.dirs).ok().flatten().is_none() {
            bail!("remote access stopped while waiting for the phone");
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// `pair` 等手机时收到了信号 `.0`，口令文件已经删掉；退出码照 shell 的惯例是 128 加信号。
#[derive(Debug)]
pub(crate) struct Interrupted(pub(crate) i32);

impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "stopped by signal {}", self.0)
    }
}

impl std::error::Error for Interrupted {}

/// `Signals` 接住的信号，没接到时为 0。信号处理函数里只能做这种无锁的写。
static CAUGHT: AtomicI32 = AtomicI32::new(0);

extern "C" fn note_signal(signal: libc::c_int) {
    CAUGHT.store(signal, Ordering::Relaxed);
}

/// `pair` 等手机期间接住 Ctrl-C、关掉终端和 `kill`：按默认处理进程当场死掉，`PairingTicket` 来不及
/// 删口令文件，口令在过期前一直配得上。丢掉时换回原来的处理。
struct Signals([(libc::c_int, libc::sighandler_t); 3]);

impl Signals {
    fn catch() -> Self {
        CAUGHT.store(0, Ordering::Relaxed);
        Self([libc::SIGINT, libc::SIGHUP, libc::SIGTERM].map(|signal| {
            let handler: extern "C" fn(libc::c_int) = note_signal;
            // SAFETY: `note_signal` 只写一个原子变量，在信号处理函数里可以调。
            let previous = unsafe { libc::signal(signal, handler as libc::sighandler_t) };
            // 原来就忽略的（`nohup` 下的 SIGHUP）照旧忽略。
            if previous == libc::SIG_IGN {
                // SAFETY: 换回忽略，不涉及别的状态。
                unsafe { libc::signal(signal, libc::SIG_IGN) };
            }
            (signal, previous)
        }))
    }

    fn caught(&self) -> Option<i32> {
        Some(CAUGHT.load(Ordering::Relaxed)).filter(|&signal| signal != 0)
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        for (signal, previous) in self.0 {
            // SAFETY: `previous` 是 `catch` 时 `signal` 返回的原处理。
            unsafe { libc::signal(signal, previous) };
        }
    }
}

pub(crate) fn devices(env: &Env, json: bool, out: &mut dyn Write) -> anyhow::Result<()> {
    let devices = list_devices(&env.dirs).context("cannot read the paired devices")?;
    if json {
        serde_json::to_writer_pretty(&mut *out, &devices)?;
        writeln!(out)?;
        return Ok(());
    }
    if devices.is_empty() {
        writeln!(out, "No paired devices. Pair one with `runode remote pair`.")?;
        return Ok(());
    }
    let now = now_unix();
    // 设备名是手机报上来的，`table` 替它换掉控制字符。
    let rows = devices.iter().map(|Device { device_id, name, paired_at, last_seen, .. }| {
        vec![device_id.to_string(), name.clone(), ago(now, *paired_at), ago(now, *last_seen)]
    });
    // `PAIRED` 补到 10 格：短的「just now」也占这么宽，`LAST SEEN` 不随内容左右挪。
    table(&["ID", "NAME", "PAIRED    ", "LAST SEEN"], rows, out)
}

pub(crate) fn revoke(env: &Env, device: &str, out: &mut dyn Write) -> anyhow::Result<()> {
    let devices = list_devices(&env.dirs).context("cannot read the paired devices")?;
    let prefix = device.to_ascii_lowercase();
    let matching: Vec<&Device> =
        devices.iter().filter(|known| known.device_id.to_string().starts_with(&prefix)).collect();
    let found = match matching.as_slice() {
        [] => bail!("no paired device {device}; `runode remote devices` lists them"),
        [found] => *found,
        several => {
            let names: Vec<String> =
                several.iter().map(|known| format!("{} ({})", known.device_id, plain(&known.name))).collect();
            bail!("{device} matches {} devices: {}", several.len(), names.join(", "))
        }
    };
    if !revoke_device(&env.dirs, found.device_id).context("cannot update the paired devices")? {
        bail!("{} was revoked meanwhile", found.device_id);
    }
    writeln!(
        out,
        "Revoked {} ({}); if it is connected, it is cut off within seconds.",
        plain(&found.name),
        found.device_id
    )?;
    Ok(())
}

/// 从终端上读用户的一行回答；标准输入不是终端（脚本里、接着管道）或读到头时为 `None`。
pub(crate) fn read_answer() -> Option<String> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return None;
    }
    let mut line = String::new();
    match stdin.lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line),
    }
}

/// 配对成了以后：宿主跑在 app 里时，退出 app 远程访问跟着停，手机就连不上了。配置里还没开
/// `BACKGROUND_KEY` 时问用户要不要开，直接回车算要；开了以后退出 app 时宿主连同各终端和远程访问
/// 留在后台（app 运行中改这一项马上算数）。`answer` 为 `None`（问不了）或者回答不要时不改配置，
/// 只说怎么自己开。写不了配置文件时也照样算配对成了，说明原因。问的时候用户可能在 app 或编辑器里
/// 改了配置，所以回答以后重新读一遍再改，不拿问之前读的那份盖掉。
fn offer_background(env: &Env, out: &mut dyn Write, answer: &mut dyn FnMut() -> Option<String>) -> anyhow::Result<()> {
    let Some(path) = env.dirs.config_file() else {
        return Ok(());
    };
    let shown = path.display();
    let on = |file: &ConfigFile| file.values(BACKGROUND_KEY).last().is_some_and(|value| value == "true");
    match ConfigFile::read(&path) {
        Ok(file) if on(&file) => return Ok(()),
        Ok(_) => {}
        Err(err) => {
            writeln!(out, "Cannot read {shown} ({err}), so remote access may stop when you quit runode.")?;
            return Ok(());
        }
    }
    write!(out, "Keep runode running in the background after you quit it, so the phone can still connect? [Y/n] ")?;
    out.flush()?;
    let yes = match answer() {
        Some(line) => matches!(line.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes"),
        // 用户没按回车，换个行再往下说。
        None => {
            writeln!(out)?;
            false
        }
    };
    if !yes {
        writeln!(
            out,
            "Remote access stops when you quit runode. To keep it running, set `{BACKGROUND_KEY} = true` in {shown}."
        )?;
        return Ok(());
    }
    let mut file = match ConfigFile::read(&path) {
        Ok(file) if on(&file) => return Ok(()),
        Ok(file) => file,
        Err(err) => {
            writeln!(
                out,
                "Cannot read {shown} ({err}). To keep remote access running after you quit runode, set \
                 `{BACKGROUND_KEY} = true` there yourself."
            )?;
            return Ok(());
        }
    };
    file.set(BACKGROUND_KEY, &["true".to_owned()]);
    match file.write(&path) {
        Ok(()) => writeln!(
            out,
            "Set `{BACKGROUND_KEY} = true` in {shown}: quitting runode now leaves its terminals and remote access \
             running in the background."
        )?,
        Err(err) => writeln!(
            out,
            "Cannot write {shown} ({err}). To keep remote access running after you quit runode, set \
             `{BACKGROUND_KEY} = true` there yourself."
        )?,
    }
    Ok(())
}

/// 没有监听方时：配置里还没开 `REMOTE_KEY` 就替用户写上，说明改了什么，再等监听方（运行中的 app
/// 或 `runode --host`）读到配置起来，免得配对前还得自己改配置。已经开着（只是 runode 没在跑）、
/// 写不了配置或者等到 `wait` 还没起来时报错。
fn turn_on(env: &Env, out: &mut dyn Write, wait: Duration) -> anyhow::Result<ListenerStatus> {
    let Some(path) = env.dirs.config_file() else {
        return Err(not_listening(env));
    };
    let shown = path.display();
    let mut file = ConfigFile::read(&path).with_context(|| format!("cannot read {shown}"))?;
    if file.values(REMOTE_KEY).last().is_some_and(|value| value == "true") {
        return Err(not_listening(env));
    }
    file.set(REMOTE_KEY, &["true".to_owned()]);
    file.write(&path).with_context(|| format!("cannot write {shown}; set `{REMOTE_KEY} = true` there yourself"))?;
    writeln!(out, "Remote access was off: set `{REMOTE_KEY} = true` in {shown}.")?;
    out.flush()?;
    let deadline = Instant::now() + wait;
    loop {
        if let Some(status) = listener_status(&env.dirs).context("cannot tell whether remote access is on")? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            bail!(
                "remote access is on in {shown} now, but runode is not running to pick it up. Open runode (or start \
                 `runode --host`), then run `runode remote pair` again"
            )
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// 远程访问没开时的说明。
fn not_listening(env: &Env) -> anyhow::Error {
    let config =
        env.dirs.config_file().map_or_else(|| "runode's config file".into(), |path| path.display().to_string());
    anyhow!(
        "remote access is not running. Turn it on with `remote-access = true` in {config}; runode picks the change \
         up within seconds while it runs. If it is on already, open runode: remote access runs alongside its \
         terminal host, and the host's log says why it cannot listen"
    )
}

/// 把 `text` 画成二维码：每个字符是上下两个模块（`▀`，上面的用前景色、下面的用背景色），颜色直接
/// 写成黑白的 24 位色，不受终端配色（深色背景、改过的调色板）影响，四周留白。
fn qr_code(text: &str) -> anyhow::Result<String> {
    let code = QrCode::with_error_correction_level(text, EcLevel::L).context("the pairing link is too long")?;
    let size = code.width();
    let dark = |x: usize, y: usize| -> bool {
        x >= QUIET_ZONE
            && y >= QUIET_ZONE
            && x < size + QUIET_ZONE
            && y < size + QUIET_ZONE
            && code[(x - QUIET_ZONE, y - QUIET_ZONE)] == Color::Dark
    };
    let full = size + 2 * QUIET_ZONE;
    let color = |dark: bool| if dark { "0;0;0" } else { "255;255;255" };
    let mut out = String::new();
    for y in (0..full).step_by(2) {
        for x in 0..full {
            let (top, bottom) = (dark(x, y), y + 1 < full && dark(x, y + 1));
            // 最后一行只有上半时下半也是留白。
            out.push_str(&format!("\x1b[38;2;{}m\x1b[48;2;{}m▀", color(top), color(bottom)));
        }
        out.push_str("\x1b[0m\n");
    }
    Ok(out)
}

/// 从 `then` 到 `now` 过了多久，粗略地说。
fn ago(now: u64, then: u64) -> String {
    let secs = now.saturating_sub(then);
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86_400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_qr_code_is_drawn_in_half_blocks() {
        let drawing = qr_code("runode://pair?v=1").unwrap();
        let lines: Vec<&str> = drawing.lines().collect();
        let size = QrCode::with_error_correction_level("runode://pair?v=1", EcLevel::L).unwrap().width();
        let full = size + 2 * QUIET_ZONE;
        assert_eq!(lines.len(), full.div_ceil(2));
        for line in &lines {
            assert_eq!(line.matches('▀').count(), full);
            assert!(line.ends_with("\x1b[0m"));
        }
        // 第一行整行是留白：上下都是白的。
        assert!(!lines[0].contains("38;2;0;0;0m"));
    }

    /// 配置文件在临时目录里的 `Env`。
    fn env_in(name: &str) -> (Env, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("rnb-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = runode_paths::Dirs::from_vars(|_| Some(root.clone().into()));
        (Env { dirs, ..Env::default() }, root)
    }

    fn offer(env: &Env, answer: Option<&str>) -> String {
        let mut out = Vec::new();
        offer_background(env, &mut out, &mut || answer.map(str::to_owned)).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn background_values(env: &Env) -> Vec<String> {
        ConfigFile::read(&env.dirs.config_file().unwrap()).unwrap().values(BACKGROUND_KEY)
    }

    #[test]
    fn pressing_enter_keeps_runode_in_the_background() {
        let (env, root) = env_in("enter");
        let path = env.dirs.config_file().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "font-size = 14\nterminal-host = false\n").unwrap();
        let out = offer(&env, Some("\n"));
        assert!(out.contains("[Y/n]") && out.contains("running in the background"), "{out}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "font-size = 14\nterminal-host = true\n");
        // 已经开着就不再问。
        assert_eq!(offer(&env, None), "");
        let _ = std::fs::remove_dir_all(root);
    }

    /// 问的时候配置被别处改了：回答以后照改过的那份加上这一项，不拿问之前读的盖掉。
    #[test]
    fn changes_made_while_asking_are_kept() {
        let (env, root) = env_in("edited");
        let path = env.dirs.config_file().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "font-size = 14\n").unwrap();
        let mut out = Vec::new();
        offer_background(&env, &mut out, &mut || {
            std::fs::write(&path, "font-size = 16\n").unwrap();
            Some("y\n".into())
        })
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "font-size = 16\nterminal-host = true\n");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn saying_no_or_not_answering_leaves_the_config_alone() {
        let (env, root) = env_in("no");
        for answer in [Some(" N \n"), Some("whatever\n"), None] {
            let out = offer(&env, answer);
            assert!(out.contains("set `terminal-host = true`"), "{answer:?}: {out}");
            assert!(background_values(&env).is_empty(), "{answer:?}");
        }
        assert!(!env.dirs.config_file().unwrap().exists());
        let out = offer(&env, Some("yes\n"));
        assert!(out.contains("running in the background"), "{out}");
        assert_eq!(background_values(&env), ["true"]);
        let _ = std::fs::remove_dir_all(root);
    }

    fn pair_off(env: &Env, wait: Duration) -> (anyhow::Result<()>, String) {
        let mut out = Vec::new();
        let result = pair(env, &[], &mut out, &mut || None, wait);
        (result, String::from_utf8(out).unwrap())
    }

    #[test]
    fn pairing_writes_remote_access_into_the_config_and_gives_up_without_a_listener() {
        let (env, root) = env_in("turn-on");
        let path = env.dirs.config_file().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "font-size = 14\nremote-access = false\n").unwrap();
        let (result, out) = pair_off(&env, Duration::from_millis(100));
        assert!(result.unwrap_err().to_string().contains("not running to pick it up"));
        assert!(out.starts_with("Remote access was off: set `remote-access = true` in "), "{out}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "font-size = 14\nremote-access = true\n");
        // 已经开着只是 runode 没在跑：不再改配置、不输出。
        let (result, out) = pair_off(&env, Duration::from_millis(100));
        assert!(result.unwrap_err().to_string().contains("remote access is not running"));
        assert_eq!(out, "");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn times_read_roughly() {
        assert_eq!(ago(100, 90), "just now");
        assert_eq!(ago(4000, 100), "1h ago");
        assert_eq!(ago(200_000, 0), "2d ago");
        assert_eq!(ago(0, 100), "just now");
    }
}
