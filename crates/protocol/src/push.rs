//! agent 停下来等回答时，会话所在电脑的宿主经 APNs 给手机推一条普通通知，标题是「<agent> 在等你回答」，
//! 副标题是会话标题和电脑名，正文是屏幕底部的问题。任务板上的任务做完了（agent 干完活停下来，不是在等
//! 回答）时推另一种（`NotificationKind::TodoDone`），标题是「任务做完了」，副标题是任务标题和电脑名，
//! 没有正文。这里是推送的线上格式：发给 APNs 的 payload 和请求头里要用的值，以及经中转服务发送时的
//! 请求体。
//!
//! # 流程
//!
//! 1. 手机在前台连上某台电脑时发 `ClientMsg::PushRegister`，交出 APNs 的 device token；电脑记在配对
//!    设备表里，手机断开后照样能推。注销是 `token` 为空的同一条消息。这条消息只在手机经远程访问连上来
//!    时由监听方办，不到宿主那里，见 `remote` 的模块文档。
//! 2. 那台电脑上有会话的 agent 停下来等回答时，电脑给每台登记过的手机发一条通知（`notify_payload`）。
//!    一次等回答只推一次，等的时候屏幕上的问题变了不再推。通知的 `apns-collapse-id` 是会话标识
//!    （`Notification::collapse_id`），同一个会话再等时新通知替掉通知中心里的旧的；`thread-id` 是电脑，
//!    同一台电脑的通知归在一组。电脑撤不回发出去的通知，答完的由手机打开 App 时自己清掉。
//! 3. 任务板上的任务从进行中变成需要你、又不是在等回答（agent 干完活停下来）时，电脑同样推一条
//!    （`Notification::todo_done`），一次做完只推一次；`apns-collapse-id` 也是任务的会话标识。
//!
//! 官方 App 的推送经中转服务发（它持有签 APNs 请求的密钥），请求体是 `RelayNotify`；自己签名打包的
//! App 由电脑拿用户配置的密钥直连 APNs。
//!
//! # 线上格式
//!
//! APNs payload 里 `aps` 的键是 APNs 规定的 kebab-case；`aps` 外面的 `machine`、`session` 由手机端读，
//! 点开通知时据此打开那个会话。时间是 Unix 秒，由调用方传入 `now`。payload 超过 `MAX_APNS_PAYLOAD`
//! 字节时先从最上面一行起删 `lines`，删完还超就缩短 `title`，见 `notify_payload`。
//!
//! 中转请求体的键是 snake_case，都带 `v` 和 `env`。中转服务按请求体拼出和 `notify_payload` 一样的
//! payload、带上同样的请求头发给 APNs，所以请求体里的内容已经照 payload 的上限删减过。出错时中转服务
//! 原样带回 APNs 的 HTTP 状态和 `PushError`，限流时回 429 带 `Retry-After`。
//!
//! 等回答的通知发到 `RELAY_NOTIFY`、`v` 是 `RELAY_VERSION`，请求体里没有 `kind`；别的种类发到
//! `RELAY_NOTIFY_V3`、`v` 是 `RELAY_VERSION_V3`，带 `kind`。分开是因为还没升级的中转服务不认得 `kind`，
//! 发到 v2 会被拼成「在等你回答」；它对 v3 回 404，这边算推不了、不重试。
//!
//! 和 `remote` 一样，`tests/fixtures/push` 里存着照规格另外写出来的样例（不由这里的类型生成），手机端
//! 的测试读同一批文件；改格式要两边一起改。

use runode_shared_types::session::SessionMeta;
use serde::{Deserialize, Serialize};

use crate::message::SessionId;

/// 一条推送的 payload 最多这么多字节（APNs 的上限）。
pub const MAX_APNS_PAYLOAD: usize = 4096;
/// 电脑名、标题最多留这么多个字。
pub const TEXT_LIMIT: usize = 48;
/// 屏幕上的一行最多留这么多个字。
pub const LINE_LIMIT: usize = 100;
/// 等回答时带几行屏幕底部的文字（问题），和手机端 `SessionListModel.waitingPreviewLines` 一样。
pub const BLOCKED_LINES: usize = 3;
/// 通知在 APNs 那里最多留这么多秒（`apns-expiration`）：手机这段时间一直连不上就不送了，等回答的事
/// 过时了。
pub const EXPIRES_AFTER: u64 = 30 * 60;
/// 等回答的通知的中转请求体版本（`v`），发到 `RELAY_NOTIFY`。
pub const RELAY_VERSION: u32 = 2;
/// 别的种类的通知（带 `kind`）的中转请求体版本，发到 `RELAY_NOTIFY_V3`。
pub const RELAY_VERSION_V3: u32 = 3;
/// 官方中转服务的基址，下面的接口路径接在它后面。只有发布流水线编译时设 `RUNODE_PUSH_RELAY`，
/// 开发版和自己编译的版本里是 `None`，不经中转推官方 App；本地要测中转时编译前自己设上。
pub const RELAY_URL: Option<&str> = option_env!("RUNODE_PUSH_RELAY");
/// 发等回答的通知：`POST`，体是 `v` 为 `RELAY_VERSION` 的 `RelayNotify`，回 200。
pub const RELAY_NOTIFY: &str = "/v2/notify";
/// 发别的种类的通知：`POST`，体是 `v` 为 `RELAY_VERSION_V3` 的 `RelayNotify`，回 200。
pub const RELAY_NOTIFY_V3: &str = "/v3/notify";
/// 官方 App 的 bundle id：登记的是它时经中转服务推送。
pub const OFFICIAL_BUNDLE: &str = "dev.runode.mobile";
/// 没有标题时 `Notification::title` 用的名字。
const UNTITLED: &str = "终端";
/// 通知的标题和副标题在手机端本地化表里的键。不知道是哪个 agent（`agent` 为空）时标题用另一个键。
const TITLE_KEY: &str = "agent.blocked.title";
const TITLE_KEY_UNNAMED: &str = "agent.blocked.title.unnamed";
const SUBTITLE_KEY: &str = "agent.blocked.body";
/// 任务做完的通知的标题，不带参数；副标题和等回答的一样用 `SUBTITLE_KEY`。
const TODO_DONE_TITLE_KEY: &str = "todo.done.title";

/// 推给哪个 APNs 环境：App Store、TestFlight 装的是 `Production`，Xcode 直接装的是 `Development`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApnsEnv {
    Production,
    Development,
    /// 比自己新的一方才有的环境，推不了。
    #[serde(other)]
    Unknown,
}

/// 通知是哪一种。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    /// agent 在等回答。
    #[default]
    Blocked,
    /// 任务板上的任务做完了：agent 干完活停下来，不是在等回答。`Notification` 的 `title` 是任务标题，
    /// `agent`、`lines` 为空。
    TodoDone,
}

impl NotificationKind {
    fn is_blocked(&self) -> bool {
        *self == Self::Blocked
    }

    /// 经中转发时请求体的 `v` 和接口路径，见模块文档。
    pub fn relay(self) -> (u32, &'static str) {
        match self {
            Self::Blocked => (RELAY_VERSION, RELAY_NOTIFY),
            Self::TodoDone => (RELAY_VERSION_V3, RELAY_NOTIFY_V3),
        }
    }
}

/// 一条通知里的东西。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    /// 手机注册时给这台电脑编的 UUID（`ClientMsg::PushRegister::machine`），原样带回，手机据此认出
    /// 是哪台电脑。
    pub machine: String,
    /// 电脑的名字，最多 `TEXT_LIMIT` 个字。
    pub machine_name: String,
    pub session: SessionId,
    /// 会话的标题，最多 `TEXT_LIMIT` 个字。
    pub title: String,
    /// agent 给人看的名字（「Claude Code」）；不知道时为空。
    pub agent: String,
    /// 屏幕底部的问题，最多 `BLOCKED_LINES` 行、每行 `LINE_LIMIT` 个字；不带屏幕文字时为空。
    pub lines: Vec<String>,
    /// 等回答时不写出来，等回答的线上格式和加它之前一样。
    #[serde(default, skip_serializing_if = "NotificationKind::is_blocked")]
    pub kind: NotificationKind,
}

impl Notification {
    /// 按会话的状态和屏幕底部的几行（`preview_lines` 取好的）拼出来：标题是程序设置的标题，没有时
    /// 是前台程序名或目录名，都没有时是「终端」；标题、电脑名截到 `TEXT_LIMIT` 个字；`lines` 只留最后
    /// `BLOCKED_LINES` 行，每行截到 `LINE_LIMIT` 个字。不是 agent 在前台时 `agent` 为空。
    pub fn new(machine: String, machine_name: &str, session: SessionId, meta: &SessionMeta, lines: &[String]) -> Self {
        let title = [&meta.title, &meta.fallback_title]
            .into_iter()
            .flatten()
            .find(|title| !title.is_empty())
            .map_or(UNTITLED, String::as_str);
        let agent = meta.agent.map_or("", |agent| agent.kind.display_name());
        let lines = &lines[lines.len().saturating_sub(BLOCKED_LINES)..];
        Self {
            machine,
            machine_name: clipped(machine_name, TEXT_LIMIT),
            session,
            title: clipped(title, TEXT_LIMIT),
            agent: agent.to_owned(),
            lines: lines.iter().map(|line| clipped(line, LINE_LIMIT)).collect(),
            kind: NotificationKind::Blocked,
        }
    }

    /// 任务 `title` 做完了：它的会话是 `session`；标题、电脑名截到 `TEXT_LIMIT` 个字。
    pub fn todo_done(machine: String, machine_name: &str, session: SessionId, title: &str) -> Self {
        Self {
            machine,
            machine_name: clipped(machine_name, TEXT_LIMIT),
            session,
            title: clipped(title, TEXT_LIMIT),
            agent: String::new(),
            lines: Vec::new(),
            kind: NotificationKind::TodoDone,
        }
    }

    /// 请求头 `apns-collapse-id`：会话标识，同一个会话的新通知替掉旧的。
    pub fn collapse_id(&self) -> String {
        self.session.to_string()
    }
}

/// 等回答时卡片上的几行：agent 问的问题和它上面的内容（比如要执行的命令），不要选项和按键提示。
/// 屏幕上有编号的选项（「❯ 1. Yes」「2. No」这类，连同缩进到选项文字下面的续行）时去掉它们，最后一个
/// 选项下面的（「Esc to cancel · Tab to amend」这类）也不要；没有选项时就是屏幕最底下的几行。去掉
/// 行尾空白，跳过空行和只有空白、制表符（U+2500–U+257F，分隔线和边框）的行，取最后 `limit` 行。
pub fn preview_lines(text: &str, limit: usize) -> Vec<String> {
    let mut kept: Vec<&str> = Vec::new();
    // 正在看的选项的文字从第几列起：缩进到这一列或更深的行是它的续行。
    let mut option: Option<usize> = None;
    // 最后一个选项出现时 `kept` 有多长：之后的都是选项下面的提示。
    let mut before_footer = None;
    let meaningful = text
        .split('\n')
        .map(str::trim_end)
        .filter(|line| line.chars().any(|c| !c.is_whitespace() && !('\u{2500}'..='\u{257f}').contains(&c)));
    for line in meaningful {
        if let Some(column) = option_column(line) {
            option = Some(column);
            before_footer = Some(kept.len());
        } else if option.is_some_and(|column| indent(line) >= column) {
        } else {
            option = None;
            kept.push(line);
        }
    }
    kept.truncate(before_footer.unwrap_or(kept.len()));
    kept[kept.len().saturating_sub(limit)..].iter().map(|&line| line.to_owned()).collect()
}

/// `line` 是编号选项（可以带「❯」这类选中标记，编号一两位，后面跟「.」或「)」和空格）时，选项文字
/// 从第几列（按字符数）起。
fn option_column(line: &str) -> Option<usize> {
    let rest = line.trim_start();
    let rest = rest.strip_prefix(['❯', '›', '>', '▶', '→']).map_or(rest, str::trim_start);
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if !(1..=2).contains(&digits) {
        return None;
    }
    let after = rest[digits..].strip_prefix(['.', ')'])?;
    if !after.starts_with(' ') {
        return None;
    }
    Some(line.chars().count() - after.trim_start().chars().count())
}

/// 行首空白有几个字符。
fn indent(line: &str) -> usize {
    line.chars().take_while(|c| c.is_whitespace()).count()
}

/// 通知的 APNs payload：标题是「<agent> 在等你回答」（没有 agent 时是另一句，任务做完时是「任务做完了」），
/// 副标题是标题和电脑名，
/// 文字在手机端的本地化表里；正文是屏幕上的几行，没有时不写；同一台电脑的归一组（`thread-id`）。
/// 超过 `MAX_APNS_PAYLOAD` 字节时先从最上面一行起删 `lines`，删完还超就把标题一次次缩短一半；标题
/// 删光了还超（电脑名没按 `Notification::new` 截过才会）时为 `None`。
pub fn notify_payload(notification: &Notification) -> Option<Vec<u8>> {
    fit(notification).map(|(bytes, _)| bytes)
}

/// 经中转服务发通知：中转服务照 `notify_payload` 拼 payload，请求头照直连时的写。发到哪个接口、`v` 是几
/// 看通知的种类（`NotificationKind::relay`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayNotify {
    /// `RELAY_VERSION` 或 `RELAY_VERSION_V3`，见 `NotificationKind::relay`。
    pub v: u32,
    pub env: ApnsEnv,
    /// device token，十六进制。
    pub token: String,
    /// `apns-expiration`，Unix 秒。
    pub expiration: u64,
    pub notification: Notification,
}

impl RelayNotify {
    /// 过期时刻是 `now` 加 `EXPIRES_AFTER`，内容照 `notify_payload` 删减到装得下；装不下时为 `None`。
    pub fn new(env: ApnsEnv, token: String, notification: &Notification, now: u64) -> Option<Self> {
        let (_, notification) = fit(notification)?;
        let (v, _) = notification.kind.relay();
        Some(Self { v, env, token, expiration: now + EXPIRES_AFTER, notification })
    }
}

/// APNs 回错误时的响应体（`{"reason":"BadDeviceToken"}` 这类），中转服务出错时也回这个样子。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushError {
    pub reason: String,
}

/// APNs payload 外面那一层。
#[derive(Serialize)]
struct Payload<'a> {
    aps: Aps<'a>,
    machine: &'a str,
    session: SessionId,
}

/// payload 的 `aps`，键名按 APNs 的规定。
#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
struct Aps<'a> {
    alert: Alert<'a>,
    sound: &'static str,
    thread_id: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
struct Alert<'a> {
    title_loc_key: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    title_loc_args: Vec<&'a str>,
    subtitle_loc_key: &'static str,
    subtitle_loc_args: Vec<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
}

fn encode(notification: &Notification) -> Option<Vec<u8>> {
    let (title_loc_key, title_loc_args) = match notification.kind {
        NotificationKind::TodoDone => (TODO_DONE_TITLE_KEY, Vec::new()),
        NotificationKind::Blocked if notification.agent.is_empty() => (TITLE_KEY_UNNAMED, Vec::new()),
        NotificationKind::Blocked => (TITLE_KEY, vec![notification.agent.as_str()]),
    };
    let alert = Alert {
        title_loc_key,
        title_loc_args,
        subtitle_loc_key: SUBTITLE_KEY,
        subtitle_loc_args: vec![&notification.title, &notification.machine_name],
        body: (!notification.lines.is_empty()).then(|| notification.lines.join("\n")),
    };
    let aps = Aps { alert, sound: "default", thread_id: &notification.machine };
    // 只有结构体、字符串和数，编不出来的情况实际不会有。
    serde_json::to_vec(&Payload { aps, machine: &notification.machine, session: notification.session }).ok()
}

/// 编出来超过 `MAX_APNS_PAYLOAD` 时照 `notify_payload` 说的删行、缩短标题。返回编好的 payload 和删减后
/// 的内容。
fn fit(notification: &Notification) -> Option<(Vec<u8>, Notification)> {
    let mut notification = notification.clone();
    loop {
        let bytes = encode(&notification)?;
        if bytes.len() <= MAX_APNS_PAYLOAD {
            return Some((bytes, notification));
        }
        if !notification.lines.is_empty() {
            notification.lines.remove(0);
            continue;
        }
        let chars = notification.title.chars().count();
        if chars == 0 {
            return None;
        }
        notification.title = clipped(&notification.title, chars / 2);
    }
}

/// 超过 `limit` 个字时留前 `limit - 1` 个加「…」，和手机端截字的规矩一样
/// （那边按字形簇数，这里按 Unicode 标量数）。
fn clipped(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(limit.saturating_sub(1)).collect();
    if limit > 0 {
        out.push('…');
    }
    out
}
