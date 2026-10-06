//! 远程访问那一页的配对手机：和 `runode remote pair` 一样生成一个短命的口令（`PairingTicket`），
//! 拼成配对 URI 画成二维码，隔一会儿看一眼口令文件，配好了、过期了或者作废了就停下说结果。口令
//! 跟着 `Pairing::Waiting` 活：取消、再配一台、关设置窗口时丢掉，口令文件随之删掉，手机再拿它来
//! 配对就对不上了。
//!
//! 配好以后和命令行一样提醒 `terminal-host`：宿主跑在 app 里时，退出 app 远程访问跟着停，手机就
//! 连不上了。

use std::{rc::Rc, time::Duration};

use gpui::{
    Bounds, ClipboardItem, Context, Div, Hsla, SharedString, Task, Window, canvas, div, fill, point, prelude::*, px,
    size,
};
use qrcode::{Color, EcLevel, QrCode};
use runode_paths::Dirs;
use runode_protocol::remote::PAIRING_TTL;
use runode_remote_access::{PairingProgress, PairingTicket, listener_status};

use super::{
    SettingsView,
    controls::{Colors, button, on_click, row, switch},
};

/// 等配对时隔多久看一眼口令文件。
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// 等配对时隔这么多次看一眼监听方还在不在。
const STATUS_EVERY: u32 = 8;
/// 二维码一个模块的边长，像素。
const MODULE_SIZE: f32 = 4.;
/// 二维码四周留白的模块数。规范要 4 个，2 个手机照样扫得出；白底卡片本身也算留白。
const QUIET_ZONE: usize = 2;
/// 配好以后提醒的配置项，见模块说明。
const BACKGROUND_KEY: &str = "terminal-host";

/// 配对进行到哪了。
#[derive(Default)]
pub(super) enum Pairing {
    #[default]
    Idle,
    Waiting(Box<Waiting>),
    Paired {
        name: String,
    },
    /// 没开成或者没配成，原因已经翻译好。
    Failed(String),
}

pub(super) struct Waiting {
    dirs: Dirs,
    ticket: PairingTicket,
    uri: SharedString,
    qr: Rc<Qr>,
    polls: u32,
    /// 上次画的时候还剩几秒，变了才重画。
    shown_secs: u64,
    _poll: Task<()>,
}

/// 画好的二维码：每个模块深还是浅，含四周的留白。
struct Qr {
    width: usize,
    dark: Vec<bool>,
}

impl Qr {
    fn new(text: &str) -> Option<Self> {
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
        let mut x = 0;
        std::iter::from_fn(move || {
            while x < line.len() && !line[x] {
                x += 1;
            }
            let start = x;
            while x < line.len() && line[x] {
                x += 1;
            }
            (x > start).then_some((start, x - start))
        })
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs())
}

/// 剩下的时间，写成「分:秒」。
fn remaining(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

impl SettingsView {
    /// 生成口令，开始等手机。远程访问没在监听、写不了口令文件时说原因。
    fn start_pairing(&mut self, cx: &mut Context<Self>) {
        self.pairing = match self.begin_pairing(cx) {
            Ok(waiting) => Pairing::Waiting(Box::new(waiting)),
            Err(err) => Pairing::Failed(err),
        };
        cx.notify();
    }

    fn begin_pairing(&mut self, cx: &mut Context<Self>) -> Result<Waiting, String> {
        // 先丢掉上一个口令，免得新口令写好后旧的那份在丢掉时把文件删了。
        self.pairing = Pairing::Idle;
        let io_failed = |err: std::io::Error| rust_i18n::t!("settings.pairing.io_failed", err = err.to_string());
        let dirs = Dirs::from_env();
        let status = listener_status(&dirs)
            .map_err(io_failed)?
            .ok_or_else(|| rust_i18n::t!("settings.pairing.not_listening").into_owned())?;
        let ticket = PairingTicket::begin(&dirs, PAIRING_TTL).map_err(io_failed)?;
        let uri = ticket.uri(&status, &[]).map_err(io_failed)?.to_string();
        let qr = Qr::new(&uri).ok_or_else(|| rust_i18n::t!("settings.pairing.too_long").into_owned())?;
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                if this.update(cx, |this, cx| this.poll_pairing(cx)).is_err() {
                    break;
                }
            }
        });
        Ok(Waiting {
            shown_secs: ticket.expires_at().saturating_sub(now_unix()),
            dirs,
            ticket,
            uri: uri.into(),
            qr: Rc::new(qr),
            polls: 0,
            _poll: poll,
        })
    }

    /// 看一眼配对进行到哪了；不等了就换掉 `Waiting`，看口令的任务随之停下。
    fn poll_pairing(&mut self, cx: &mut Context<Self>) {
        let Pairing::Waiting(waiting) = &mut self.pairing else {
            return;
        };
        waiting.polls += 1;
        let next = match waiting.ticket.poll() {
            Ok(PairingProgress::Waiting) => {
                if waiting.polls.is_multiple_of(STATUS_EVERY) && listener_status(&waiting.dirs).ok().flatten().is_none()
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
            Ok(PairingProgress::Expired) => Pairing::Failed(rust_i18n::t!("settings.pairing.expired").into_owned()),
            Ok(PairingProgress::Replaced) => Pairing::Failed(rust_i18n::t!("settings.pairing.replaced").into_owned()),
            Err(err) => Pairing::Failed(rust_i18n::t!("settings.pairing.io_failed", err = err.to_string()).into()),
        };
        self.pairing = next;
        cx.notify();
    }

    pub(super) fn render_pairing(&mut self, colors: Colors, cx: &mut Context<Self>) -> Div {
        let title = rust_i18n::t!("settings.pairing.title").into_owned();
        let start = |label: &str, cx: &mut Context<Self>| {
            let enabled = self.config.remote_access;
            button("pair", rust_i18n::t!(label).into_owned(), colors)
                .when(!enabled, |button| button.opacity(0.5))
                .when(enabled, |button| button.on_click(on_click(cx, |this, _, cx| this.start_pairing(cx))))
        };
        let idle_hint = || {
            let key = if self.config.remote_access { "settings.pairing.hint" } else { "settings.pairing.off" };
            SharedString::from(rust_i18n::t!(key).into_owned())
        };
        match &self.pairing {
            Pairing::Idle => row(title, Some(idle_hint()), start("settings.pairing.start", cx), None, None, colors),
            Pairing::Failed(err) => {
                let err = err.clone();
                row(title, Some(idle_hint()), start("settings.pairing.start", cx), None, Some(err), colors)
            }
            Pairing::Paired { name } => {
                let hint = rust_i18n::t!("settings.pairing.paired", name = name).into_owned();
                let on = self.config.values(BACKGROUND_KEY).first().is_some_and(|value| value == "true");
                let background = switch("pairing-background", on, colors).on_click(on_click(cx, move |this, _, cx| {
                    this.write_or_report(BACKGROUND_KEY, vec![(!on).to_string()], cx)
                }));
                let error = self.errors.get(BACKGROUND_KEY).cloned();
                div()
                    .flex()
                    .flex_col()
                    .child(row(title, Some(hint.into()), start("settings.pairing.again", cx), None, None, colors))
                    .child(row(
                        rust_i18n::t!("settings.key.terminal_host").into_owned(),
                        Some(rust_i18n::t!("settings.pairing.background").into_owned().into()),
                        background,
                        None,
                        error,
                        colors,
                    ))
            }
            Pairing::Waiting(waiting) => {
                let hint = rust_i18n::t!("settings.pairing.waiting", time = remaining(waiting.shown_secs)).into_owned();
                let cancel = button("pair-cancel", rust_i18n::t!("settings.pairing.cancel").into_owned(), colors)
                    .on_click(on_click(cx, |this, _, cx| {
                        this.pairing = Pairing::Idle;
                        cx.notify();
                    }));
                let uri = waiting.uri.clone();
                let copy = button("pair-copy", rust_i18n::t!("settings.pairing.copy").into_owned(), colors).on_click(
                    on_click(cx, move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(uri.to_string()))),
                );
                div().flex().flex_col().child(row(title, Some(hint.into()), cancel, None, None, colors)).child(
                    div().py(px(14.)).flex().items_center().gap(px(16.)).child(qr_code(waiting.qr.clone())).child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .items_start()
                            .gap(px(8.))
                            .child(
                                div()
                                    .w_full()
                                    .text_size(px(11.5))
                                    .text_color(colors.fg.opacity(0.55))
                                    .truncate()
                                    .child(waiting.uri.clone()),
                            )
                            .child(copy),
                    ),
                )
            }
        }
    }
}

/// 白底黑块的二维码，不跟终端配色走，深色主题下手机照样扫得出。
fn qr_code(qr: Rc<Qr>) -> impl IntoElement {
    let side = qr.width as f32 * MODULE_SIZE;
    div().flex_none().p(px(6.)).rounded(px(8.)).bg(gpui::white()).child(
        canvas(
            |_, _, _| {},
            move |bounds: Bounds<gpui::Pixels>, (), window: &mut Window, _| {
                let black = Hsla::black();
                for y in 0..qr.width {
                    for (x, len) in qr.runs(y) {
                        let origin = bounds.origin + point(px(x as f32 * MODULE_SIZE), px(y as f32 * MODULE_SIZE));
                        let run = Bounds::new(origin, size(px(len as f32 * MODULE_SIZE), px(MODULE_SIZE)));
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
