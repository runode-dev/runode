//! 配对口令怎么从命令行交到监听方手里：`runode remote pair` 生成口令，写进
//! `remote_access_pairing_file`（0600），画成二维码，然后等；监听方每次有设备来配对时读这个文件，
//! 口令对上了就登记设备，把文件改成「配好了」（口令随即从文件里抹掉），错了就记一次，连续错
//! `PAIRING_MAX_FAILURES` 次改成「作废了」。命令行看到结果、或者口令过期后删掉文件。
//!
//! 同一时刻只有一个口令：再跑一次 `runode remote pair` 换掉前一个，前一个看到文件换了主人就不等了。
//! 用文件而不是经宿主转交，是因为监听方和宿主在同一个进程里，宿主却不依赖远程访问；命令行和
//! 监听方本来就都能读写数据目录。

use std::{io, net::IpAddr, path::PathBuf, time::Duration};

use runode_paths::Dirs;
use runode_protocol::remote::{Bytes, DeviceId, PAIRING_MAX_FAILURES, PairingUri, RejectReason, SECRET_LEN};
use serde::{Deserialize, Serialize};

use crate::{
    addrs::local_addresses,
    devices::{self, Device},
    files::{no_home, random, read_json, write_json},
    now_unix,
    status::ListenerStatus,
};

/// 配对口令文件的内容。
#[derive(Clone, Debug, Serialize, Deserialize)]
struct PairingFile {
    /// 这次配对的编号（随机，不是秘密）：命令行据此认出文件还是不是自己写的那份。
    ticket: String,
    /// 口令；配好了或者作废了以后抹掉。
    #[serde(default)]
    secret: Option<Bytes>,
    /// 过期的时刻，Unix 秒。
    expires_at: u64,
    state: State,
    /// 配好的设备。
    #[serde(default)]
    device_id: Option<DeviceId>,
    #[serde(default)]
    device_name: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum State {
    Pending,
    Paired,
    Invalidated,
}

/// `runode remote pair` 生成的一次配对。丢掉时删掉口令文件（文件已经换了主人时不动）。
#[derive(Debug)]
pub struct PairingTicket {
    path: PathBuf,
    ticket: String,
    secret: [u8; SECRET_LEN],
    expires_at: u64,
}

/// 配对进行到哪了，见 `PairingTicket::poll`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairingProgress {
    /// 还在等设备来配对。
    Waiting,
    /// 配好了。
    Paired { device_id: DeviceId, name: String },
    /// 口令连续错了 `PAIRING_MAX_FAILURES` 次，作废了。
    Invalidated,
    /// 过期了，没有设备来配对。
    Expired,
    /// 口令文件被删了，或者换成了另一次配对的（又跑了一次 `runode remote pair`）。
    Replaced,
}

impl PairingTicket {
    /// 生成一个新的口令，`ttl` 后过期，写进口令文件；之前的口令（如果有）随之作废。
    pub fn begin(dirs: &Dirs, ttl: Duration) -> io::Result<Self> {
        dirs.create_remote_access_dir()?;
        let path = dirs.remote_access_pairing_file().ok_or_else(no_home)?;
        let ticket: String = random::<8>()?.iter().map(|b| format!("{b:02x}")).collect();
        let secret = random::<SECRET_LEN>()?;
        let expires_at = now_unix() + ttl.as_secs();
        let file = PairingFile {
            ticket: ticket.clone(),
            secret: Some(Bytes(secret.to_vec())),
            expires_at,
            state: State::Pending,
            device_id: None,
            device_name: None,
        };
        write_json(&path, &file)?;
        Ok(Self { path, ticket, secret, expires_at })
    }

    pub fn secret(&self) -> [u8; SECRET_LEN] {
        self.secret
    }

    /// 过期的时刻，Unix 秒。
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }

    /// 手机扫的配对 URI：监听方 `status` 的主机名、证书指纹和端口，这次的口令，地址先列 `extra`，
    /// 再列本机的各个地址。监听方写的指纹长度不对时报 `InvalidData`。
    pub fn uri(&self, status: &ListenerStatus, extra: &[IpAddr]) -> io::Result<PairingUri> {
        let fingerprint = status
            .fingerprint
            .0
            .as_slice()
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "the listener wrote a bad fingerprint"))?;
        let mut addrs = extra.to_vec();
        for addr in local_addresses() {
            if !addrs.contains(&addr) {
                addrs.push(addr);
            }
        }
        Ok(PairingUri {
            host_name: status.host_name.clone(),
            fingerprint,
            secret: self.secret,
            port: status.port,
            addrs,
            expires_at: self.expires_at,
        })
    }

    /// 看一眼口令文件，配对进行到哪了。
    pub fn poll(&self) -> io::Result<PairingProgress> {
        let file: Option<PairingFile> = read_json(&self.path)?;
        let Some(file) = file.filter(|file| file.ticket == self.ticket) else {
            return Ok(PairingProgress::Replaced);
        };
        Ok(match file.state {
            State::Paired => match file.device_id {
                Some(device_id) => PairingProgress::Paired { device_id, name: file.device_name.unwrap_or_default() },
                None => PairingProgress::Invalidated,
            },
            State::Invalidated => PairingProgress::Invalidated,
            State::Pending if now_unix() >= self.expires_at => PairingProgress::Expired,
            State::Pending => PairingProgress::Waiting,
        })
    }
}

/// 有还在等设备、没过期的配对口令（`runode remote pair` 正在等）。读不了口令文件时当作没有。
pub fn pairing_pending(dirs: &Dirs) -> bool {
    dirs.remote_access_pairing_file()
        .and_then(|path| read_json::<PairingFile>(&path).ok().flatten())
        .is_some_and(|file| file.state == State::Pending && now_unix() < file.expires_at)
}

impl Drop for PairingTicket {
    fn drop(&mut self) {
        let mine = read_json::<PairingFile>(&self.path).ok().flatten().is_some_and(|file| file.ticket == self.ticket);
        if mine && let Err(err) = std::fs::remove_file(&self.path) {
            tracing::debug!("failed to remove {}: {err}", self.path.display());
        }
    }
}

/// 监听方收配对口令的柜台：记着现在这份口令错了几次。调用方把它放在锁里，同一时刻只办一个配对，
/// 两台设备拿着同一个口令也只有一台配得上。
#[derive(Default)]
pub(crate) struct Desk {
    /// 在数哪一次配对（`PairingFile::ticket`）的错误次数。
    ticket: Option<String>,
    failures: u32,
}

impl Desk {
    /// 设备拿 `secret` 来配对：口令对上了（没过期、没用过、没作废）才调 `verify` 验签、拿到要登记
    /// 的设备，登记好后把口令文件改成配好了。口令不对、`verify` 不过都算错一次。
    pub(crate) fn redeem(
        &mut self,
        dirs: &Dirs,
        secret: &[u8],
        verify: impl FnOnce() -> Result<Device, RejectReason>,
    ) -> Result<Device, RejectReason> {
        let Some(path) = dirs.remote_access_pairing_file() else { return Err(RejectReason::PairingInvalid) };
        let file = match read_json::<PairingFile>(&path) {
            Ok(file) => file,
            Err(err) => {
                tracing::warn!("cannot read the pairing code {}: {err}", path.display());
                None
            }
        };
        let Some(mut file) = file else { return Err(RejectReason::PairingInvalid) };
        let Some(expected) = file.secret.clone().filter(|_| file.state == State::Pending) else {
            return Err(RejectReason::PairingInvalid);
        };
        if now_unix() >= file.expires_at {
            return Err(RejectReason::PairingInvalid);
        }
        if self.ticket.as_deref() != Some(file.ticket.as_str()) {
            self.ticket = Some(file.ticket.clone());
            self.failures = 0;
        }
        if !same_secret(&expected.0, secret) {
            self.failed(&path, file);
            return Err(RejectReason::PairingInvalid);
        }
        let device = match verify() {
            Ok(device) => device,
            Err(reason) => {
                self.failed(&path, file);
                return Err(reason);
            }
        };
        if let Err(err) = devices::add(dirs, device.clone()) {
            tracing::warn!("cannot record the paired device: {err}");
            return Err(RejectReason::PairingInvalid);
        }
        file.secret = None;
        file.state = State::Paired;
        file.device_id = Some(device.device_id);
        file.device_name = Some(device.name.clone());
        if let Err(err) = write_json(&path, &file) {
            tracing::warn!("cannot mark the pairing code as used: {err}");
        }
        self.ticket = None;
        Ok(device)
    }

    /// 这份口令又错了一次，满 `PAIRING_MAX_FAILURES` 次就作废。
    fn failed(&mut self, path: &std::path::Path, mut file: PairingFile) {
        self.failures += 1;
        tracing::info!("wrong pairing attempt {} of {PAIRING_MAX_FAILURES}", self.failures);
        if self.failures < PAIRING_MAX_FAILURES {
            return;
        }
        file.secret = None;
        file.state = State::Invalidated;
        if let Err(err) = write_json(path, &file) {
            tracing::warn!("cannot invalidate the pairing code: {err}");
        }
    }
}

/// 比较两个口令，花的时间和哪里不同无关。
fn same_secret(expected: &[u8], given: &[u8]) -> bool {
    expected.len() == given.len() && expected.iter().zip(given).fold(0u8, |diff, (a, b)| diff | (a ^ b)) == 0
}
