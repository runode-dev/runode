//! agent 停下来等回答时，会话所在电脑的宿主经 APNs 推送弹出手机上的 Live Activity（灵动岛、锁屏），
//! 一个会话一个，答完收起。这里是推送的线上格式：Live Activity 的 attributes 和 content-state、
//! 发给 APNs 的 payload，以及经中转服务发送时的请求体。
//!
//! # 流程
//!
//! 1. 手机在前台连上某台电脑时发 `ClientMsg::PushRegister`，交出 push-to-start token；电脑记在配对
//!    设备表里，手机断开后照样能推。注销是 `token` 为空的同一条消息。这条消息只在手机经远程访问连上来
//!    时由监听方办，不到宿主那里，见 `remote` 的模块文档。
//! 2. 那台电脑上有会话的 agent 停下来等回答时，电脑先建一个广播频道（APNs 的 broadcast channel，
//!    iOS 18 起有），再用 push-to-start token 发 `ActivityEvent::Start`（`start_payload`），payload 里的
//!    `input-push-channel` 让系统起的 Live Activity 订阅这个频道。push-to-start 起的 activity 拿不到
//!    update token，所以之后内容变了往频道里广播 `Update`（`update_payload`），答完了广播 `End`
//!    （`end_payload`），再删掉频道。同一个会话推给几台手机时，按 APNs 环境和发送的路线各建一个频道。
//!
//! 官方 App 的推送经中转服务发（它持有签 APNs 请求的密钥），请求体见 `RelayStart` 等；自己签名打包的
//! App 由电脑拿用户配置的密钥直连 APNs。
//!
//! # 线上格式
//!
//! attributes（`ActivityAttributes`）和 content-state（`ActivityContent`）由手机端 Swift 的默认
//! `Codable` 解，键名是 Swift 属性名的写法（camelCase），时间一律不用 `Date`。APNs payload 里的键是
//! APNs 规定的 kebab-case，时间是 Unix 秒，由调用方传入 `now`。payload 超过 `MAX_APNS_PAYLOAD` 字节时
//! 先从最上面一行起删 `lines`，删完还超就缩短 `title`，见 `start_payload`。
//!
//! 中转请求体的键是 snake_case，都带 `v`（`RELAY_VERSION`）和 `env`。中转服务按请求体拼出和
//! `start_payload`、`update_payload`、`end_payload` 一样的 payload（提醒用同样的本地化键）发给 APNs，
//! 所以请求体里的内容已经照 payload 的上限删减过。出错时中转服务原样带回 APNs 的 HTTP 状态和
//! `PushError`，限流时回 429 带 `Retry-After`。
//!
//! 和 `remote` 一样，`tests/fixtures/push` 里存着照规格另外写出来的样例（不由这里的类型生成），手机端
//! 的测试读同一批文件；改格式要两边一起改。

use runode_shared_types::session::SessionMeta;
use serde::{Deserialize, Serialize};

use crate::message::SessionId;

/// Swift 那边 Live Activity attributes 的类型名，APNs 起 Live Activity 时按它找类型。
pub const ATTRIBUTES_TYPE: &str = "AgentActivityAttributes";
/// 一条 Live Activity 推送的 payload 最多这么多字节（APNs 的上限）。
pub const MAX_APNS_PAYLOAD: usize = 4096;
/// 电脑名、标题最多留这么多个字。
pub const TEXT_LIMIT: usize = 48;
/// 屏幕上的一行最多留这么多个字。
pub const LINE_LIMIT: usize = 100;
/// 等回答时带几行屏幕底部的文字（问题和选项），和手机端 `SessionListModel.waitingPreviewLines` 一样。
pub const BLOCKED_LINES: usize = 3;
/// 推送的内容过了这么多秒没刷新，系统把 Live Activity 标成过时（`stale-date`）。
pub const STALE_AFTER: u64 = 30 * 60;
/// 起 Live Activity 时的 `relevance-score`：几个同时在时系统按它挑先显示哪个，都一样。
pub const RELEVANCE_SCORE: u32 = 100;
/// 中转请求体的版本，各请求体的 `v`。
pub const RELAY_VERSION: u32 = 1;
/// 官方中转服务的基址，下面几个接口的路径接在它后面。配置项 `push-relay-url` 可以换成别的。
pub const RELAY_URL: &str = "https://push.runode.dev";
/// 建频道：`POST`，体是 `RelayCreateChannel`，回 `RelayChannel`。
pub const RELAY_CREATE_CHANNEL: &str = "/v1/channels";
/// 删频道：`POST`，体是 `RelayDeleteChannel`，回 204。
pub const RELAY_DELETE_CHANNEL: &str = "/v1/channels/delete";
/// 起 Live Activity：`POST`，体是 `RelayStart`。
pub const RELAY_START: &str = "/v1/start";
/// 往频道里广播：`POST`，体是 `RelayBroadcast`。
pub const RELAY_BROADCAST: &str = "/v1/broadcast";
/// 官方 App 的 bundle id：登记的是它时经中转服务推送。
pub const OFFICIAL_BUNDLE: &str = "dev.runode.mobile";
/// 没有标题时 `ActivityContent::title` 用的名字。
const UNTITLED: &str = "终端";
/// 起 Live Activity 时提醒的标题和正文在手机端本地化表里的键。
const ALERT_TITLE_KEY: &str = "agent.blocked.title";
const ALERT_BODY_KEY: &str = "agent.blocked.body";

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

/// Live Activity 开了以后不变的部分。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityAttributes {
    /// 手机注册时给这台电脑编的 UUID（`ClientMsg::PushRegister::machine`），原样带回，手机据此认出
    /// 是哪台电脑。
    pub machine: String,
    /// 电脑的名字，最多 `TEXT_LIMIT` 个字。
    pub machine_name: String,
    pub session: SessionId,
}

impl ActivityAttributes {
    /// `machine_name` 截到 `TEXT_LIMIT` 个字。
    pub fn new(machine: String, machine_name: &str, session: SessionId) -> Self {
        Self { machine, machine_name: clipped(machine_name, TEXT_LIMIT), session }
    }
}

/// Live Activity 每次推送带的内容（content-state）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityContent {
    /// 会话的标题，最多 `TEXT_LIMIT` 个字。
    pub title: String,
    /// agent 给人看的名字（「Claude Code」）。
    pub agent: String,
    /// 屏幕底部的问题和选项，最多 `BLOCKED_LINES` 行、每行 `LINE_LIMIT` 个字；不带屏幕文字时为空。
    pub lines: Vec<String>,
}

impl ActivityContent {
    /// 按会话的状态和屏幕底部的几行（`preview_lines` 取好的）拼出来：标题是程序设置的标题，没有时
    /// 是前台程序名或目录名，都没有时是「终端」，截到 `TEXT_LIMIT` 个字；`lines` 只留最后
    /// `BLOCKED_LINES` 行，每行截到 `LINE_LIMIT` 个字。不是 agent 在前台时 `agent` 为空。
    pub fn new(meta: &SessionMeta, lines: &[String]) -> Self {
        let title = [&meta.title, &meta.fallback_title]
            .into_iter()
            .flatten()
            .find(|title| !title.is_empty())
            .map_or(UNTITLED, String::as_str);
        let agent = meta.agent.map_or("", |agent| agent.kind.display_name());
        let lines = &lines[lines.len().saturating_sub(BLOCKED_LINES)..];
        Self {
            title: clipped(title, TEXT_LIMIT),
            agent: agent.to_owned(),
            lines: lines.iter().map(|line| clipped(line, LINE_LIMIT)).collect(),
        }
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

/// Live Activity 推送的种类（APNs 的 `event`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityEvent {
    /// 用 push-to-start token 起一个新的 Live Activity。
    Start,
    /// 往频道里广播新的内容。
    Update,
    /// 往频道里广播收起。
    End,
}

/// 起 Live Activity 的 APNs payload：带着 attributes、内容和提醒（标题是「<agent> 在等你回答」这类，
/// 正文带标题和电脑名，文字在手机端的本地化表里），内容 `STALE_AFTER` 秒后过时，起来的 activity 订阅
/// 广播频道 `channel`（建频道时 APNs 给的 id）。超过 `MAX_APNS_PAYLOAD` 字节时先从最上面一行起删
/// `lines`，删完还超就把标题（内容和提醒里的一起）一次次缩短一半；标题删光了还超（attributes 没按
/// `ActivityAttributes::new` 截过才会）时为 `None`。
pub fn start_payload(
    attributes: &ActivityAttributes,
    content: &ActivityContent,
    channel: &str,
    now: u64,
) -> Option<Vec<u8>> {
    fit(content, |content| encode(start_aps(attributes, content, channel, now))).map(|(bytes, _)| bytes)
}

/// 往频道里广播新内容的 APNs payload：不带提醒，过时的时刻往后挪到 `now` 加 `STALE_AFTER`。超长时的
/// 处理同 `start_payload`。
pub fn update_payload(content: &ActivityContent, now: u64) -> Option<Vec<u8>> {
    fit(content, |content| encode(update_aps(content, now))).map(|(bytes, _)| bytes)
}

/// 往频道里广播收起的 APNs payload：带着最后的内容，`dismissal-date` 是 `now`，系统马上从锁屏上拿掉。
/// 超长时的处理同 `start_payload`。
pub fn end_payload(content: &ActivityContent, now: u64) -> Option<Vec<u8>> {
    fit(content, |content| encode(end_aps(content, now))).map(|(bytes, _)| bytes)
}

/// 建频道，回 `RelayChannel`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayCreateChannel {
    /// `RELAY_VERSION`。
    pub v: u32,
    pub env: ApnsEnv,
}

/// 建好的频道：APNs 在响应头 `apns-channel-id` 里给的 id（base64），原样转回来。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayChannel {
    pub channel: String,
}

/// 删频道，回 204。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayDeleteChannel {
    /// `RELAY_VERSION`。
    pub v: u32,
    pub env: ApnsEnv,
    pub channel: String,
}

/// 用 push-to-start token 起 Live Activity：中转服务照 `start_payload` 拼 payload。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayStart {
    /// `RELAY_VERSION`。
    pub v: u32,
    pub env: ApnsEnv,
    /// push-to-start token，十六进制。
    pub token: String,
    /// 起来的 activity 订阅的频道。
    pub channel: String,
    /// APNs 的 `timestamp`，Unix 秒。
    pub timestamp: u64,
    pub stale_date: u64,
    pub attributes: ActivityAttributes,
    pub content: ActivityContent,
}

impl RelayStart {
    /// 时刻是 `now`，内容照 `start_payload` 删减到装得下；装不下时为 `None`。
    pub fn new(
        env: ApnsEnv,
        token: String,
        channel: String,
        attributes: ActivityAttributes,
        content: &ActivityContent,
        now: u64,
    ) -> Option<Self> {
        let (_, content) = fit(content, |content| encode(start_aps(&attributes, content, &channel, now)))?;
        Some(Self {
            v: RELAY_VERSION,
            env,
            token,
            channel,
            timestamp: now,
            stale_date: now + STALE_AFTER,
            attributes,
            content,
        })
    }
}

/// 往频道里广播：`event` 是 `Update` 或 `End`，中转服务照 `update_payload`、`end_payload` 拼 payload；
/// `End` 不带 `stale_date`，`timestamp` 同时是 `dismissal-date`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayBroadcast {
    /// `RELAY_VERSION`。
    pub v: u32,
    pub env: ApnsEnv,
    pub channel: String,
    pub event: ActivityEvent,
    /// APNs 的 `timestamp`，Unix 秒。
    pub timestamp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_date: Option<u64>,
    pub content: ActivityContent,
}

impl RelayBroadcast {
    /// 广播新内容，时刻是 `now`，内容照 `update_payload` 删减到装得下。
    pub fn update(env: ApnsEnv, channel: String, content: &ActivityContent, now: u64) -> Option<Self> {
        let (_, content) = fit(content, |content| encode(update_aps(content, now)))?;
        let stale_date = Some(now + STALE_AFTER);
        Some(Self { v: RELAY_VERSION, env, channel, event: ActivityEvent::Update, timestamp: now, stale_date, content })
    }

    /// 广播收起，时刻是 `now`，内容照 `end_payload` 删减到装得下。
    pub fn end(env: ApnsEnv, channel: String, content: &ActivityContent, now: u64) -> Option<Self> {
        let (_, content) = fit(content, |content| encode(end_aps(content, now)))?;
        Some(Self {
            v: RELAY_VERSION,
            env,
            channel,
            event: ActivityEvent::End,
            timestamp: now,
            stale_date: None,
            content,
        })
    }
}

/// APNs 回错误时的响应体（`{"reason":"BadDeviceToken"}` 这类），中转服务出错时也回这个样子。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushError {
    pub reason: String,
}

fn start_aps<'a>(
    attributes: &'a ActivityAttributes,
    content: &'a ActivityContent,
    channel: &'a str,
    now: u64,
) -> Aps<'a> {
    Aps {
        timestamp: now,
        event: ActivityEvent::Start,
        content_state: content,
        attributes_type: Some(ATTRIBUTES_TYPE),
        attributes: Some(attributes),
        alert: Some(Alert {
            title: Localized { loc_key: ALERT_TITLE_KEY, loc_args: vec![&content.agent] },
            body: Localized { loc_key: ALERT_BODY_KEY, loc_args: vec![&content.title, &attributes.machine_name] },
            sound: "default",
        }),
        stale_date: Some(now + STALE_AFTER),
        dismissal_date: None,
        relevance_score: Some(RELEVANCE_SCORE),
        input_push_channel: Some(channel),
    }
}

fn update_aps(content: &ActivityContent, now: u64) -> Aps<'_> {
    Aps { stale_date: Some(now + STALE_AFTER), ..Aps::plain(ActivityEvent::Update, content, now) }
}

fn end_aps(content: &ActivityContent, now: u64) -> Aps<'_> {
    Aps { dismissal_date: Some(now), ..Aps::plain(ActivityEvent::End, content, now) }
}

/// APNs payload 外面那一层。
#[derive(Serialize)]
struct Payload<'a> {
    aps: Aps<'a>,
}

/// payload 的 `aps`，键名按 APNs 的规定。没有的项不写。
#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
struct Aps<'a> {
    timestamp: u64,
    event: ActivityEvent,
    content_state: &'a ActivityContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    attributes_type: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attributes: Option<&'a ActivityAttributes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    alert: Option<Alert<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stale_date: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dismissal_date: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    relevance_score: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_push_channel: Option<&'a str>,
}

impl<'a> Aps<'a> {
    /// 只有时刻、种类和内容的。
    fn plain(event: ActivityEvent, content_state: &'a ActivityContent, timestamp: u64) -> Self {
        Self {
            timestamp,
            event,
            content_state,
            attributes_type: None,
            attributes: None,
            alert: None,
            stale_date: None,
            dismissal_date: None,
            relevance_score: None,
            input_push_channel: None,
        }
    }
}

/// 起 Live Activity 时的提醒。
#[derive(Serialize)]
struct Alert<'a> {
    title: Localized<'a>,
    body: Localized<'a>,
    sound: &'static str,
}

/// 手机端本地化表里的一条文字和填进去的参数。
#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
struct Localized<'a> {
    loc_key: &'static str,
    loc_args: Vec<&'a str>,
}

fn encode(aps: Aps<'_>) -> Option<Vec<u8>> {
    // 只有结构体、字符串和数，编不出来的情况实际不会有。
    serde_json::to_vec(&Payload { aps }).ok()
}

/// 按 `encode` 编出来，超过 `MAX_APNS_PAYLOAD` 时照 `start_payload` 说的删行、缩短标题。返回编好的
/// payload 和删减后的内容。
fn fit(
    content: &ActivityContent,
    encode: impl Fn(&ActivityContent) -> Option<Vec<u8>>,
) -> Option<(Vec<u8>, ActivityContent)> {
    let mut content = content.clone();
    loop {
        let bytes = encode(&content)?;
        if bytes.len() <= MAX_APNS_PAYLOAD {
            return Some((bytes, content));
        }
        if !content.lines.is_empty() {
            content.lines.remove(0);
            continue;
        }
        let chars = content.title.chars().count();
        if chars == 0 {
            return None;
        }
        content.title = clipped(&content.title, chars / 2);
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
