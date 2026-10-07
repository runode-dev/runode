//! 在界面上给手机配对：和 `runode remote pair` 一样生成一个短命的口令（`PairingTicket`），拼成配对 URI
//! 画成二维码，隔一会儿看一眼口令文件，配好了、过期了或者作废了就停下说结果。设置窗口的远程访问页和
//! 窗口里的手机端引导页都用它，各自按自己的样子画。口令跟着 `Pairing::Waiting` 活：取消、重新生成、
//! 关掉所在的页面时丢掉，口令文件随之删掉，手机再拿它来配对就对不上了。
//!
//! 刚打开远程访问、或者刚改了给手机看的名字（`remote-access-name`）时，监听方要过一会儿才开好、换好
//! 名字，这时先等它（`Pairing::Starting`），好了再生成口令，二维码里不会是旧名字。
//!
//! 配好以后和命令行一样提醒 `terminal-host`（`BACKGROUND_KEY`）：宿主跑在 app 里时，退出 app 远程访问
//! 跟着停，手机就连不上了。

use std::{net::IpAddr, rc::Rc, time::Duration};

use gpui::{Bounds, Context, Hsla, SharedString, Task, Window, canvas, div, fill, point, prelude::*, px, size};
use qrcode::{Color, EcLevel, QrCode};
use runode_paths::Dirs;
use runode_protocol::remote::PAIRING_TTL;
use runode_remote_access::{ListenerStatus, PairingProgress, PairingTicket, host_name, listener_status, now_unix};

use crate::config::AppConfig;

/// 等配对时隔多久看一眼口令文件。
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// 等配对时隔这么多次看一眼监听方还在不在。
const STATUS_EVERY: u32 = 8;
/// 等监听方开好最多看这么多次（十秒）：还没开好就说没在监听；开着但名字一直不对（比如监听开在
/// 没读到新配置的别的进程里）就照它现在的名字生成口令。
const START_POLLS: u32 = 40;
/// 二维码四周留白的模块数。规范要 4 个，2 个手机照样扫得出；白底卡片本身也算留白。
const QUIET_ZONE: usize = 2;
/// 配好以后提醒的配置项，见模块说明。
pub const BACKGROUND_KEY: &str = "terminal-host";

/// 从视图里取出它的那份配对；取不到（页面关了）时等配对的任务停下。
pub type Slot<V> = fn(&mut V) -> Option<&mut Pairing>;

/// 配对进行到哪了。
#[derive(Default)]
pub enum Pairing {
    #[default]
    Idle,
    /// 等监听方开好、换上 `name`，见模块说明。
    Starting {
        dirs: Dirs,
        name: String,
        extra: Vec<IpAddr>,
        polls: u32,
        _poll: Task<()>,
    },
    Waiting(Box<Waiting>),
    Paired {
        name: String,
    },
    /// 没开成或者没配成，原因已经翻译好。
    Failed(String),
}

pub struct Waiting {
    dirs: Dirs,
    ticket: PairingTicket,
    uri: SharedString,
    qr: Rc<Qr>,
    polls: u32,
    /// 上次画的时候还剩几秒，变了才重画。
    shown_secs: u64,
    _poll: Task<()>,
}

impl Waiting {
    pub fn uri(&self) -> SharedString {
        self.uri.clone()
    }

    pub fn qr(&self) -> Rc<Qr> {
        self.qr.clone()
    }

    /// 口令还剩多久过期，写成「分:秒」。
    pub fn remaining(&self) -> String {
        remaining(self.shown_secs)
    }
}

/// 画好的二维码：每个模块深还是浅，含四周的留白。
pub struct Qr {
    width: usize,
    dark: Vec<bool>,
}

impl Qr {
    pub fn new(text: &str) -> Option<Self> {
        let code = QrCode::with_error_correction_level(text, EcLevel::L).ok()?;
        let size = code.width();
        let width = size + 2 * QUIET_ZONE;
        let mut dark = vec![false; width * width];
        for y in 0..size {
            for x in 0..size {
                dark[(y + QUIET_ZONE) * width + x + QUIET_ZONE] = code[(x, y)] == Color::Dark;
            }
        }
        Some(Self { width, dark })
    }

    /// 第 `y` 行里连着的深色模块，(起点, 长度)；连成一段画，相邻模块之间不会露出细缝。
    fn runs(&self, y: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
        let line = &self.dark[y * self.width..(y + 1) * self.width];
        line.chunk_by(|a, b| a == b)
            .scan(0, |x, run| {
                let start = *x;
                *x += run.len();
                Some((start, run))
            })
            .filter(|(_, run)| run[0])
            .map(|(start, run)| (start, run.len()))
    }
}

/// 剩下的时间，写成「分:秒」。
fn remaining(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn io_failed(err: std::io::Error) -> String {
    rust_i18n::t!("settings.pairing.io_failed", err = err.to_string()).into_owned()
}

impl Pairing {
    /// 生成口令，开始等手机；监听方还没开好、名字还不是配置里的时先等它。配对 URI 里的地址先列
    /// `extra`，再列本机别的地址。`slot` 是这份配对在视图里的位置。
    pub fn start<V: 'static>(&mut self, slot: Slot<V>, extra: Vec<IpAddr>, cx: &mut Context<V>) {
        // 先丢掉上一个口令，免得新口令写好后旧的那份在丢掉时把文件删了。
        *self = Pairing::Idle;
        let name = cx.global::<AppConfig>().0.remote_access_name.clone().unwrap_or_else(host_name);
        // 当场那次就好了的话，`Starting` 连同看它的任务一起换掉。
        *self = Pairing::Starting { dirs: Dirs::from_env(), name, extra, polls: 0, _poll: watch(slot, cx) };
        self.poll(slot, cx);
        cx.notify();
    }

    fn begin<V: 'static>(
        dirs: Dirs,
        status: &ListenerStatus,
        extra: &[IpAddr],
        slot: Slot<V>,
        cx: &mut Context<V>,
    ) -> Self {
        let waiting = (|| {
            let ticket = PairingTicket::begin(&dirs, PAIRING_TTL).map_err(io_failed)?;
            let uri = ticket.uri(status, extra).map_err(io_failed)?.to_string();
            let qr = Qr::new(&uri).ok_or_else(|| rust_i18n::t!("settings.pairing.too_long").into_owned())?;
            Ok::<_, String>(Waiting {
                shown_secs: ticket.expires_at().saturating_sub(now_unix()),
                dirs,
                ticket,
                uri: uri.into(),
                qr: Rc::new(qr),
                polls: 0,
                _poll: watch(slot, cx),
            })
        })();
        match waiting {
            Ok(waiting) => Pairing::Waiting(Box::new(waiting)),
            Err(err) => Pairing::Failed(err),
        }
    }

    /// 看一眼监听方开好没有、配对进行到哪了；不等了就换掉这一份，看的任务随之停下。
    fn poll<V: 'static>(&mut self, slot: Slot<V>, cx: &mut Context<V>) {
        let next = match self {
            Pairing::Starting { dirs, name, extra, polls, .. } => {
                *polls += 1;
                match listener_status(dirs) {
                    Ok(Some(status)) if status.host_name == *name || *polls >= START_POLLS => {
                        Self::begin(dirs.clone(), &status, extra, slot, cx)
                    }
                    Ok(_) if *polls < START_POLLS => return,
                    Ok(_) => Pairing::Failed(rust_i18n::t!("settings.pairing.not_listening").into_owned()),
                    Err(err) => Pairing::Failed(io_failed(err)),
                }
            }
            Pairing::Waiting(waiting) => {
                waiting.polls += 1;
                match waiting.ticket.poll() {
                    Ok(PairingProgress::Waiting) => {
                        if waiting.polls.is_multiple_of(STATUS_EVERY)
                            && listener_status(&waiting.dirs).ok().flatten().is_none()
                        {
                            Pairing::Failed(rust_i18n::t!("settings.pairing.stopped").into_owned())
                        } else {
                            let secs = waiting.ticket.expires_at().saturating_sub(now_unix());
                            if secs != waiting.shown_secs {
                                waiting.shown_secs = secs;
                                cx.notify();
                            }
                            return;
                        }
                    }
                    Ok(PairingProgress::Paired { name, .. }) => Pairing::Paired { name },
                    Ok(PairingProgress::Invalidated) => {
                        Pairing::Failed(rust_i18n::t!("settings.pairing.invalidated").into_owned())
                    }
                    Ok(PairingProgress::Expired) => {
                        Pairing::Failed(rust_i18n::t!("settings.pairing.expired").into_owned())
                    }
                    Ok(PairingProgress::Replaced) => {
                        Pairing::Failed(rust_i18n::t!("settings.pairing.replaced").into_owned())
                    }
                    Err(err) => Pairing::Failed(io_failed(err)),
                }
            }
            Pairing::Idle | Pairing::Paired { .. } | Pairing::Failed(_) => return,
        };
        *self = next;
        cx.notify();
    }
}

/// 隔一会儿看一眼 `slot` 里的配对，视图没了或者页面关了就停下。
fn watch<V: 'static>(slot: Slot<V>, cx: &mut Context<V>) -> Task<()> {
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor().timer(POLL_INTERVAL).await;
            let alive = this.update(cx, |view, cx| {
                let Some(pairing) = slot(view) else { return false };
                pairing.poll(slot, cx);
                true
            });
            if !matches!(alive, Ok(true)) {
                break;
            }
        }
    })
}

/// 白底黑块的二维码，一个模块 `module` 像素见方。不跟终端配色走，深色主题下手机照样扫得出。
pub fn qr_code(qr: Rc<Qr>, module: f32) -> impl IntoElement {
    let side = qr.width as f32 * module;
    div().flex_none().p(px(module * 1.5)).rounded(px(module * 2.)).bg(gpui::white()).child(
        canvas(
            |_, _, _| {},
            move |bounds: Bounds<gpui::Pixels>, (), window: &mut Window, _| {
                let black = Hsla::black();
                for y in 0..qr.width {
                    for (x, len) in qr.runs(y) {
                        let origin = bounds.origin + point(px(x as f32 * module), px(y as f32 * module));
                        let run = Bounds::new(origin, size(px(len as f32 * module), px(module)));
                        window.paint_quad(fill(run, black));
                    }
                }
            },
        )
        .size(px(side)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_code_has_a_quiet_zone_and_runs_cover_the_dark_modules() {
        let qr = Qr::new("runode://pair?v=1").unwrap();
        let size = QrCode::with_error_correction_level("runode://pair?v=1", EcLevel::L).unwrap().width();
        assert_eq!(qr.width, size + 2 * QUIET_ZONE);
        assert_eq!(qr.runs(0).count(), 0);
        for y in 0..qr.width {
            let mut line = vec![false; qr.width];
            for (x, len) in qr.runs(y) {
                assert!(len > 0);
                line[x..x + len].fill(true);
            }
            assert_eq!(line, qr.dark[y * qr.width..(y + 1) * qr.width]);
        }
    }

    #[test]
    fn remaining_time_reads_as_minutes_and_seconds() {
        assert_eq!(remaining(600), "10:00");
        assert_eq!(remaining(65), "1:05");
        assert_eq!(remaining(0), "0:00");
    }
}
