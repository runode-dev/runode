//! `runode remote …`：给手机配对远程访问（`pair`）、列出（`devices`）和撤销（`revoke`）配对过的设备。
//! 不经宿主，读写 `runode_remote_access` 管的文件：监听方（宿主所在的进程）开着时才能配对，列和
//! 撤销不用它开着。

use std::{io::Write, net::IpAddr, thread, time::Duration};

use anyhow::{Context as _, anyhow, bail};
use qrcode::{Color, EcLevel, QrCode};
use runode_protocol::remote::{PAIRING_TTL, PairingUri};
use runode_remote_access::{
    Device, PairingProgress, PairingTicket, list_devices, listener_status, local_addresses, revoke_device,
};

use crate::Env;

/// 等配对时隔多久看一眼口令文件。
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// 等配对时隔这么多次看一眼监听方还在不在。
const STATUS_EVERY: u32 = 8;
/// 二维码四周留白的模块数。规范要 4 个，2 个手机照样扫得出，画出来窄一些。
const QUIET_ZONE: usize = 2;

pub(crate) fn pair(env: &Env, extra: &[IpAddr], out: &mut dyn Write) -> anyhow::Result<()> {
    let Some(status) = listener_status(&env.dirs).context("cannot tell whether remote access is on")? else {
        return Err(not_listening(env));
    };
    let fingerprint = status.fingerprint.0.as_slice().try_into().context("the listener wrote a bad fingerprint")?;
    let ticket = PairingTicket::begin(&env.dirs, PAIRING_TTL).context("cannot write the pairing code")?;
    let mut addrs = extra.to_vec();
    for addr in local_addresses() {
        if !addrs.contains(&addr) {
            addrs.push(addr);
        }
    }
    let uri = PairingUri {
        host_name: status.host_name.clone(),
        fingerprint,
        secret: ticket.secret(),
        port: status.port,
        addrs,
        expires_at: ticket.expires_at(),
    }
    .to_string();
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
                writeln!(out, "Paired with {name} ({device_id}).")?;
                return Ok(());
            }
            PairingProgress::Invalidated => {
                bail!(
                    "the pairing code was entered wrongly too many times and no longer works; run `runode remote pair` again"
                )
            }
            PairingProgress::Expired => bail!("the pairing code expired before a phone used it"),
            PairingProgress::Replaced => bail!("another `runode remote pair` replaced this pairing code"),
        }
        polls += 1;
        if polls.is_multiple_of(STATUS_EVERY) && listener_status(&env.dirs).ok().flatten().is_none() {
            bail!("remote access stopped while waiting for the phone");
        }
        thread::sleep(POLL_INTERVAL);
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
    let width = devices.iter().map(|device| device.name.chars().count()).max().unwrap_or(0).max("NAME".len());
    writeln!(out, "{:<32}  {:<width$}  {:<10}  LAST SEEN", "ID", "NAME", "PAIRED")?;
    for Device { device_id, name, paired_at, last_seen, .. } in &devices {
        // `{:<width$}` 按字符数补齐，宽字符（中文）照样对不齐，名字放在 ID 后面影响小一些。
        let pad = width.saturating_sub(name.chars().count());
        writeln!(out, "{device_id}  {name}{:pad$}  {:<10}  {}", "", ago(now, *paired_at), ago(now, *last_seen))?;
    }
    Ok(())
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
                several.iter().map(|known| format!("{} ({})", known.device_id, known.name)).collect();
            bail!("{device} matches {} devices: {}", several.len(), names.join(", "))
        }
    };
    if !revoke_device(&env.dirs, found.device_id).context("cannot update the paired devices")? {
        bail!("{} was revoked meanwhile", found.device_id);
    }
    writeln!(out, "Revoked {} ({}); if it is connected, it is cut off within seconds.", found.name, found.device_id)?;
    Ok(())
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

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs())
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

    #[test]
    fn times_read_roughly() {
        assert_eq!(ago(100, 90), "just now");
        assert_eq!(ago(4000, 100), "1h ago");
        assert_eq!(ago(200_000, 0), "2d ago");
        assert_eq!(ago(0, 100), "just now");
    }
}
