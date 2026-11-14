//! 等回答的 agent 屏幕上认出的带编号的提问（`runode_agent::prompt`），全 app 一份，窗口和看板都读，画成同一个
//! 部件（`render_prompt`）；替用户按选项的编号键回答。
//!
//! 会话进入等回答（`AgentState::Blocked`）时读一次屏幕底部，离开等回答就忘掉（`watch`）。等回答期间调用方每报
//! 一次状态再读一次；这一次等待已经读到过屏幕时（`PromptState::settled`），这样的重读之间至少隔 `SETTLED_GAP`。
//! 读屏和按键在后台线程上等宿主回话。同一个会话同时只有一次读屏在路上，读着时又要读的等这次读完再读一次；
//! 两次读屏开始之间至少隔 `READ_GAP`。
//!
//! 认出的提问放在 `Prompts` 里，只在变了时更新它，界面 `observe_global` 它重画；读屏的记账另放在 `Watched`，
//! 变了不叫界面重画。

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use gpui::{App, Context, Div, Entity, Global, Task, Window, div, prelude::*, px};
use runode_agent::prompt::{self, Prompt};
use runode_protocol::SessionId;
use runode_types::agent::AgentState;

use crate::{
    host_client::{Link, hosts::HostId},
    i18n::action_for,
    terminal_view::TerminalView,
    ui::{
        a11y::{Press as _, StaticText as _},
        button::{Button, ButtonSize, ButtonVariant},
        font,
        font_size::{FontSize, scaled},
        theme::ActiveTheme as _,
        tooltip::tooltip,
    },
};

/// 一个会话：所在的主机和宿主里的标识。
pub type Key = (HostId, SessionId);

/// 选项按钮和打开终端的图标按钮的高度（基准字号下的点数）。
const BUTTON_HEIGHT: f32 = 26.;

/// 读屏幕底部多少行：要装得下 `prompt::MAX_QUESTION_LINES` 行问题、`prompt::MAX_OPTIONS` 个折了行的选项和
/// 下面的按键提示。
const SCREEN_LINES: u32 = 24;
/// 同一个会话两次读屏开始之间至少隔这么久。
const READ_GAP: Duration = Duration::from_millis(250);
/// 这一次等待已经读到过屏幕以后，调用方再报同一个状态引起的重读离上一次读屏开始至少隔这么久：看板每次刷新、
/// 会话的标题每变一次都报一遍，屏幕多半没变。
const SETTLED_GAP: Duration = Duration::from_secs(3);
/// 按键送到以后，从送到时算起各在这些时刻重读一次：agent 重画得慢时也认得出下一问。
const REREADS_AFTER: [Duration; 3] = [Duration::from_millis(300), Duration::from_secs(1), Duration::from_millis(2500)];
/// `REREADS_AFTER` 都读过了、屏幕上还是答的那个提问时，隔这么久再读一次，直到 `ANSWERED_REREADS` 用完。
const REREAD_GAP: Duration = Duration::from_millis(1500);
/// 经 `Link` 读屏、按键最多等这么久：远端主机上一次来回要久些。
const LINK_TIMEOUT: Duration = Duration::from_secs(10);
/// 答过以后读到的屏幕上一直是答的那个提问时，最多这么多次不认提问；之后还是它，按键多半没起作用，提问照旧
/// 认出来。
const ANSWERED_REREADS: u8 = 5;

/// 怎么读一个会话的屏幕、怎么给它按键。两样都等宿主回话，只在后台线程上调。
#[derive(Clone)]
pub struct Access {
    read: Arc<dyn Fn(SessionId, u32) -> Result<String> + Send + Sync>,
    keys: Arc<dyn Fn(SessionId, Vec<String>) -> Result<()> + Send + Sync>,
}

impl Access {
    /// `read` 读会话屏幕上从最后一个有字的行往上多少行，`keys` 给会话按键，每项是一个键的写法。
    pub fn new(
        read: impl Fn(SessionId, u32) -> Result<String> + Send + Sync + 'static,
        keys: impl Fn(SessionId, Vec<String>) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        Self { read: Arc::new(read), keys: Arc::new(keys) }
    }

    /// 经到会话所在主机的连接读屏、按键，各最多等 `LINK_TIMEOUT`。
    pub fn link(link: Link) -> Self {
        let keys = link.clone();
        Self::new(
            move |id, lines| link.read_screen(id, lines, LINK_TIMEOUT),
            move |id, list| keys.send_keys(id, list, LINK_TIMEOUT),
        )
    }
}

/// 一个等回答的会话里认出的提问，和答过之后等屏幕变走的记录。不碰界面、不读钟：读到的屏幕和会话的状态由
/// 调用方交进来。
#[derive(Default)]
pub struct PromptState {
    /// agent 进入现在这个种类和状态的时刻（`SessionMeta::agent_since_ms`），宿主没报时为空。
    since: Option<u64>,
    /// 最近一次读到屏幕时的 `since`。
    read_since: Option<u64>,
    /// 认出的提问。
    prompt: Option<Arc<Prompt>>,
    /// 答过、还没读到别的提问。
    answered: Option<Answered>,
}

/// 一个提问的问题和各项原文，不记光标停在哪一项。两幅屏幕上是不是同一个提问按它比：屏幕上别处的字（agent 的
/// 状态栏、走着的秒数）变了不算变。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asked {
    question: Vec<String>,
    options: Vec<(u8, String)>,
}

impl Asked {
    fn of(prompt: &Prompt) -> Self {
        Self {
            question: prompt.question.clone(),
            options: prompt.options.iter().map(|option| (option.number, option.text.clone())).collect(),
        }
    }

    /// 屏幕 `screen` 上认出的提问，认不出时为空。
    fn on(screen: &str) -> Option<Self> {
        prompt::parse(screen).map(|prompt| Self::of(&prompt))
    }
}

struct Answered {
    /// 所答的提问：按键刚写进去、agent 还没重画时，读到的还是它。
    asked: Asked,
    /// 答时的 `PromptState::since`；之后不同了就是新的一次等待。
    since: Option<u64>,
    /// 读到的还是 `asked` 时还能不认几次。
    rereads: u8,
}

impl PromptState {
    /// 认出的提问；还没读到、认不出、刚答过时为空。
    pub fn prompt(&self) -> Option<&Arc<Prompt>> {
        self.prompt.as_ref()
    }

    /// 会话的状态：`blocked` 是 agent 在等回答，`since` 是它进入现在这个状态的时刻。不在等时认出的提问和答过的
    /// 记录都作废。返回认出的提问变了没有。
    pub fn meta(&mut self, blocked: bool, since: Option<u64>) -> bool {
        self.since = since;
        if blocked {
            return false;
        }
        self.read_since = None;
        self.answered = None;
        self.prompt.take().is_some()
    }

    /// 从时刻 `since` 起的这一次等待已经读到过屏幕。宿主没报时刻时分不出是不是同一次等待，总是假。
    pub fn settled(&self, since: Option<u64>) -> bool {
        since.is_some() && self.read_since == since
    }

    /// 读到的屏幕，返回认出的提问变了没有。答过以后，时刻没变（两边都为空也算没变）、屏幕上还是答的那个
    /// 提问、并且还有剩余次数时不认提问，次数减一；不然丢掉答过的记录，照常认。
    pub fn note_screen(&mut self, screen: &str) -> bool {
        self.read_since = self.since;
        let prompt = prompt::parse(screen);
        if let Some(found) = &prompt
            && let Some(answered) = self.answered.as_mut()
            && answered.since == self.since
            && answered.rereads > 0
            && answered.asked == Asked::of(found)
        {
            answered.rereads -= 1;
            return self.prompt.take().is_some();
        }
        self.answered = None;
        let old = self.prompt.take();
        self.prompt = prompt.map(|prompt| match &old {
            Some(same) if **same == prompt => same.clone(),
            _ => Arc::new(prompt),
        });
        old != self.prompt
    }

    /// 答过，还在等读到的屏幕上换成别的提问或者没有提问。
    pub fn awaiting_change(&self) -> bool {
        self.answered.is_some()
    }

    /// 要按 `number` 回答时，回答前重读到的屏幕上还是这个提问才按。提问里没有这一项时为空。
    pub fn expected(&self, number: u8) -> Option<Asked> {
        let prompt = self.prompt.as_ref()?;
        prompt.options.iter().any(|option| option.number == number).then(|| Asked::of(prompt))
    }

    /// 按键送到了：认出的提问作废，记下答的是它和现在的时刻。返回认出的提问变了没有。
    pub fn answered(&mut self) -> bool {
        let Some(prompt) = self.prompt.take() else { return false };
        self.answered = Some(Answered { asked: Asked::of(&prompt), since: self.since, rereads: ANSWERED_REREADS });
        true
    }
}

/// 认出的提问和回答在路上的会话。界面画选项时读（`prompt`、`answering`），`observe_global` 它重画。
#[derive(Default)]
pub struct Prompts {
    prompts: HashMap<Key, Arc<Prompt>>,
    /// 回答在路上（或者排着等这次读屏办完）的会话。
    answering: HashSet<Key>,
}

impl Global for Prompts {}

/// 会话 `key` 认出的提问；不在等回答、还没读到、认不出或者刚答过时为空。
pub fn prompt(key: &Key, cx: &App) -> Option<Arc<Prompt>> {
    cx.try_global::<Prompts>()?.prompts.get(key).cloned()
}

/// 会话 `key` 有一次回答在路上：这时再按选项什么都不做（`answer`），界面把按钮禁掉。
pub fn answering(key: &Key, cx: &App) -> bool {
    cx.try_global::<Prompts>().is_some_and(|prompts| prompts.answering.contains(key))
}

/// 在跟的会话和它们读屏的记账。
#[derive(Default)]
struct Watched {
    entries: HashMap<Key, Entry>,
    /// 每个会话报来过的最晚的 `since`，忘掉以后也留着，会话结束（`ended`）才去掉。
    seen: HashMap<Key, u64>,
    /// 下一个 `Entry::serial`。
    next_serial: u64,
}

impl Global for Watched {}

struct Entry {
    /// 每次开始跟时换一个：忘掉又重新跟上后，上一轮在路上的读屏回来时认得出不是这一轮的。
    serial: u64,
    access: Access,
    state: PromptState,
    /// 有一次读屏（或者回答）在路上，或者排着等 `READ_GAP`。
    reading: bool,
    /// 在路上的是回答。
    answering: bool,
    /// 读着的时候又要读：这次办完再读一次。
    again: bool,
    /// 读着的时候按下的回答，这次办完接着办：编号和那时的 `PromptState::expected`。
    answer: Option<(u8, Asked)>,
    /// 上一次读屏开始的时刻。
    last_start: Option<Instant>,
    /// 同一次等待又报来时排着的那次读屏，等到离上一次读屏开始满 `SETTLED_GAP` 才读；不占着 `reading`，
    /// 回答不用等它。丢掉就不读了。
    settled_read: Option<Task<()>>,
    /// 按键送到以后排着的重读，丢掉就不读了。
    _rereads: Option<Task<()>>,
    /// `rereads` 里还有几次没到。
    rereads_left: usize,
}

enum Job {
    Read,
    /// 先重读，屏幕上还是提问 `expected` 才按 `number`。
    Answer {
        number: u8,
        expected: Asked,
    },
}

enum Outcome {
    Screen(String),
    /// 回答的按键送到了。
    Sent,
    Failed(anyhow::Error),
}

/// 跟着会话 `key` 等不等回答：`blocked` 是 agent 在等回答，`since` 是它进入现在这个状态的时刻
/// （`SessionMeta::agent_since_ms`）。在等时读一次屏幕，不在等时忘掉这个会话；`access` 是读屏、按键的办法，
/// 每次换上最新给的。这一次等待已经读到过屏幕时（`PromptState::settled`），离上一次读屏开始不到 `SETTLED_GAP`
/// 的先等到那时再读，已经有一次在路上或排着的不再加。几处各按自己读到的状态报来：报来的时刻比这个会话报来过的
/// 早，就是晚到的旧状态，不理，不论那时在跟还是已经忘掉。
pub fn watch(key: Key, access: Access, blocked: bool, since: Option<u64>, cx: &mut App) {
    if let Some(since) = since {
        let seen = cx.default_global::<Watched>().seen.entry(key.clone()).or_insert(since);
        if since < *seen {
            return;
        }
        *seen = since;
    }
    if !blocked {
        forget(&key, cx);
        return;
    }
    let watched = cx.default_global::<Watched>();
    let serial = watched.next_serial;
    let entry = watched.entries.entry(key.clone()).or_insert_with(|| Entry {
        serial,
        access: access.clone(),
        state: PromptState::default(),
        reading: false,
        answering: false,
        again: false,
        answer: None,
        last_start: None,
        settled_read: None,
        _rereads: None,
        rereads_left: 0,
    });
    if entry.serial == serial {
        watched.next_serial += 1;
    }
    entry.access = access;
    let settled = entry.state.settled(since);
    entry.state.meta(true, since);
    if !settled {
        entry.settled_read = None;
        request_read(&key, cx);
    } else if !entry.reading && entry.settled_read.is_none() {
        let delay = entry.last_start.map_or(Duration::ZERO, |at| SETTLED_GAP.saturating_sub(at.elapsed()));
        let serial = entry.serial;
        let task_key = key.clone();
        let task = cx.spawn(async move |cx| {
            cx.background_executor().timer(delay).await;
            cx.update(|cx| {
                let Some(entry) = current(&task_key, serial, cx) else { return };
                entry.settled_read = None;
                if entry.reading {
                    entry.again = true;
                } else {
                    start(task_key, Job::Read, Duration::ZERO, cx);
                }
            });
        });
        if let Some(entry) = current(&key, serial, cx) {
            entry.settled_read = Some(task);
        }
    }
}

/// 会话 `key` 结束了：忘掉它（`forget`），报来过的时刻也去掉。
pub fn ended(key: &Key, cx: &mut App) {
    forget(key, cx);
    if cx.has_global::<Watched>() {
        cx.global_mut::<Watched>().seen.remove(key);
    }
}

/// 不再跟会话 `key`：认出的提问和回答在路上的记录都去掉，在路上的读屏、回答回来时不再理会。
pub fn forget(key: &Key, cx: &mut App) {
    if cx.has_global::<Watched>() && cx.global::<Watched>().entries.contains_key(key) {
        cx.global_mut::<Watched>().entries.remove(key);
    }
    let shown = cx
        .try_global::<Prompts>()
        .is_some_and(|prompts| prompts.prompts.contains_key(key) || prompts.answering.contains(key));
    if shown {
        let prompts = cx.global_mut::<Prompts>();
        prompts.prompts.remove(key);
        prompts.answering.remove(key);
    }
}

/// 跟着窗口里的终端 `view` 的会话等不等回答，读屏、按键经它到所在主机的连接。还没开会话的不管。
pub fn watch_terminal(view: &Entity<TerminalView>, cx: &mut App) {
    let view = view.read(cx);
    let Some(id) = view.session_id() else { return };
    let blocked = view.agent().is_some_and(|agent| agent.state == AgentState::Blocked);
    let key = (view.host().clone(), id);
    let since = view.meta().agent_since_ms;
    let link = view.host_link().clone();
    watch(key, Access::link(link), blocked, since, cx);
}

/// 替用户回答会话 `key` 认出的提问，选编号 `number` 那一项：先重读屏幕，上面还是用户看到的那个提问（`Asked`）
/// 才按这个数字键，不是了只按读到的更新提问。没认出提问、提问里没有这一项、上一次回答还没办完时什么都
/// 不做。
pub fn answer(key: &Key, number: u8, cx: &mut App) {
    let Some(entry) = watched(key, cx) else { return };
    if entry.answering || entry.answer.is_some() {
        return;
    }
    let Some(expected) = entry.state.expected(number) else { return };
    if entry.reading {
        entry.answer = Some((number, expected));
    } else {
        start(key.clone(), Job::Answer { number, expected }, READ_GAP, cx);
    }
    publish_answering(key, cx);
}

/// 改 `Prompts` 里会话 `key` 的提问，`observe_global` 它的界面跟着重画。只在提问变了时调。
fn publish(key: &Key, prompt: Option<Arc<Prompt>>, cx: &mut App) {
    let prompts = &mut cx.default_global::<Prompts>().prompts;
    match prompt {
        Some(prompt) => prompts.insert(key.clone(), prompt),
        None => prompts.remove(key),
    };
}

/// 按记账更新 `Prompts` 里会话 `key` 的回答在不在路上，变了才改，`observe_global` 它的界面才只在这时重画。
fn publish_answering(key: &Key, cx: &mut App) {
    let busy = watched(key, cx).is_some_and(|entry| entry.answering || entry.answer.is_some());
    if answering(key, cx) == busy {
        return;
    }
    let answering = &mut cx.default_global::<Prompts>().answering;
    if busy {
        answering.insert(key.clone());
    } else {
        answering.remove(key);
    }
}

/// 读一次会话 `key` 的屏幕；正读着时等这次读完再读。
fn request_read(key: &Key, cx: &mut App) {
    let Some(entry) = watched(key, cx) else { return };
    if entry.reading {
        entry.again = true;
        return;
    }
    start(key.clone(), Job::Read, READ_GAP, cx);
}

/// 办 `job`：离上一次读屏开始不到 `gap` 时先等到那时，再在后台读屏（回答的话再按键），办完回主线程收尾。
/// 调用方已经确认这个会话没有别的在路上。
fn start(key: Key, job: Job, gap: Duration, cx: &mut App) {
    let Some(entry) = cx.global_mut::<Watched>().entries.get_mut(&key) else { return };
    entry.reading = true;
    entry.answering = matches!(job, Job::Answer { .. });
    let delay = entry.last_start.map_or(Duration::ZERO, |at| gap.saturating_sub(at.elapsed()));
    let serial = entry.serial;
    let access = entry.access.clone();
    cx.spawn(async move |cx| {
        if !delay.is_zero() {
            cx.background_executor().timer(delay).await;
        }
        let began = cx.update(|cx| match current(&key, serial, cx) {
            Some(entry) => {
                entry.last_start = Some(Instant::now());
                true
            }
            None => false,
        });
        if !began {
            return;
        }
        let id = key.1;
        let outcome = cx.background_executor().spawn(async move { run(job, id, &access) }).await;
        cx.update(|cx| finish(key, serial, outcome, cx));
    })
    .detach();
}

/// 后台线程上办 `job`。
fn run(job: Job, id: SessionId, access: &Access) -> Outcome {
    let screen = match (access.read)(id, SCREEN_LINES) {
        Ok(screen) => screen,
        Err(err) => return Outcome::Failed(err),
    };
    match job {
        Job::Answer { number, expected } if Asked::on(&screen).as_ref() == Some(&expected) => {
            match (access.keys)(id, vec![number.to_string()]) {
                Ok(()) => Outcome::Sent,
                Err(err) => Outcome::Failed(err),
            }
        }
        _ => Outcome::Screen(screen),
    }
}

/// 在跟的会话 `key` 的记账。
fn watched<'a>(key: &Key, cx: &'a mut App) -> Option<&'a mut Entry> {
    if !cx.has_global::<Watched>() {
        return None;
    }
    cx.global_mut::<Watched>().entries.get_mut(key)
}

/// 还在跟、并且还是第 `serial` 轮的会话 `key` 的记账。
fn current<'a>(key: &Key, serial: u64, cx: &'a mut App) -> Option<&'a mut Entry> {
    watched(key, cx).filter(|entry| entry.serial == serial)
}

/// 一次读屏或回答办完了：更新认出的提问，按键送到了就排上重读，再办读着时排下的回答或读屏。
fn finish(key: Key, serial: u64, outcome: Outcome, cx: &mut App) {
    let Some(entry) = current(&key, serial, cx) else { return };
    entry.reading = false;
    entry.answering = false;
    let mut rereads = None;
    let changed = match outcome {
        Outcome::Screen(screen) => {
            let changed = entry.state.note_screen(&screen);
            if entry.state.awaiting_change() && entry.rereads_left == 0 {
                rereads = Some(vec![REREAD_GAP]);
            }
            changed
        }
        Outcome::Sent => {
            // 按着键的时候又按下的回答对的是这一问，作废，免得按到下一问上。
            entry.answer = None;
            rereads = Some(REREADS_AFTER.to_vec());
            entry.state.answered()
        }
        Outcome::Failed(err) => {
            tracing::debug!("failed to read the screen of session {} or answer it: {err:#}", key.1);
            false
        }
    };
    let prompt = entry.state.prompt().cloned();
    let next = match entry.answer.take() {
        Some((number, expected)) => Some(Job::Answer { number, expected }),
        None => std::mem::take(&mut entry.again).then_some(Job::Read),
    };
    if let Some(after) = rereads {
        schedule_rereads(&key, serial, after, cx);
    }
    if changed {
        publish(&key, prompt, cx);
    }
    if let Some(job) = next {
        start(key.clone(), job, READ_GAP, cx);
    }
    publish_answering(&key, cx);
}

/// 从现在算起在 `after` 的各个时刻重读会话 `key` 的屏幕，换掉之前排着的。
fn schedule_rereads(key: &Key, serial: u64, after: Vec<Duration>, cx: &mut App) {
    let count = after.len();
    let task_key = key.clone();
    let task = cx.spawn(async move |cx| {
        let mut waited = Duration::ZERO;
        for at in after {
            cx.background_executor().timer(at.saturating_sub(waited)).await;
            waited = at;
            cx.update(|cx| {
                if let Some(entry) = current(&task_key, serial, cx) {
                    entry.rereads_left = entry.rereads_left.saturating_sub(1);
                    request_read(&task_key, cx);
                }
            });
        }
    });
    if let Some(entry) = current(key, serial, cx) {
        entry._rereads = Some(task);
        entry.rereads_left = count;
    }
}

/// 会话 `key` 认出的提问 `prompt` 画成的部件：上面是问题，等宽的小字放在深色的小框里，每行放不下时截断、悬停
/// 看全文；下面是各选项的按钮，写编号和原文，第一项是琥珀色的主按钮，按下替用户回答（`answer`），回答在路上时
/// 都禁掉；最右是图标 `open_icon` 的按钮，按下调 `open`，`open_label` 是它做什么。`title` 是会话的标题，放进
/// 按钮报给辅助工具的名字里，几张卡片上的同一种按钮才分得清。
pub fn render_prompt<V: 'static>(
    key: &Key,
    prompt: &Prompt,
    title: &str,
    open_icon: &'static str,
    open_label: String,
    open: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
    cx: &mut Context<V>,
) -> Div {
    let theme = cx.theme();
    let (well, border, foreground) = (theme.background, theme.border, theme.foreground);
    let (warning, warning_foreground) = (theme.warning, theme.warning_foreground);
    let mono = font::mono(cx);
    let busy = answering(key, cx);
    let question = (!prompt.question.is_empty()).then(|| {
        let lines = prompt.question.iter().enumerate().map(|(ix, line)| {
            div().id(("prompt-line", ix)).min_w_0().truncate().tooltip(tooltip(line.clone(), None)).child(line.clone())
        });
        div()
            .id("prompt-question")
            .aria_static_text(prompt.question.join("\n"))
            .flex()
            .flex_col()
            .gap(px(2.))
            .px(px(10.))
            .py(px(8.))
            .rounded(px(6.))
            .bg(well)
            .border_1()
            .border_color(border)
            .text_size(FontSize::Small)
            .text_color(foreground)
            .when_some(mono, |question, mono| question.font_family(mono))
            .children(lines)
    });
    let options = prompt.options.iter().enumerate().map(|(ix, option)| {
        let number = option.number;
        let text = format!("{number}. {}", option.text);
        let label = rust_i18n::t!("prompts.option_for", number = number, text = option.text.as_str(), title = title)
            .into_owned();
        // 放不下一行时各占一行，长的截断；按钮里的字由自己的子元素画，才截得断。
        let button = Button::new(("prompt-option", usize::from(number)), label)
            .flex_grow(1.)
            .min_w_0()
            .max_w_full()
            .disabled(busy)
            .tooltip(tooltip(text.clone(), None))
            .child(div().min_w_0().truncate().child(text));
        let button = if ix == 0 {
            button
                .variant(ButtonVariant::Default)
                .bg(warning)
                .text_color(warning_foreground)
                .hover(move |hover| hover.bg(warning.opacity(0.9)))
        } else {
            button.variant(ButtonVariant::Secondary)
        };
        if busy {
            return button;
        }
        let key = key.clone();
        button.cursor_pointer().on_press(cx, move |_, _, cx| answer(&key, number, cx))
    });
    let options: Vec<Button> = options.collect();
    let open_button = Button::new("prompt-open", open_label.clone())
        .aria_label(action_for(&open_label, title))
        .icon(open_icon)
        .variant(ButtonVariant::Outline)
        .with_size(ButtonSize::Icon)
        .size(scaled(BUTTON_HEIGHT))
        .cursor_pointer()
        .tooltip(tooltip(open_label, None))
        .on_press(cx, open);
    div().flex().flex_col().gap(px(8.)).children(question).child(
        div()
            .flex()
            .items_start()
            .gap(px(6.))
            .child(div().flex_1().min_w_0().flex().flex_wrap().gap(px(6.)).children(options))
            .child(open_button),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    const ASKING: &str = " Do you want to proceed?\n ❯ 1. Yes\n   2. No (esc)\n";
    /// `ASKING` 里的光标挪到了第二项。
    const MOVED: &str = " Do you want to proceed?\n   1. Yes\n ❯ 2. No (esc)\n";
    const NEXT: &str = " Allow edits to src/main.rs?\n ❯ 1. Yes\n   2. No (esc)\n";

    fn blocked(since: Option<u64>) -> PromptState {
        let mut state = PromptState::default();
        state.meta(true, since);
        state
    }

    fn texts(state: &PromptState) -> Option<Vec<String>> {
        state.prompt().map(|prompt| prompt.options.iter().map(|option| option.text.clone()).collect())
    }

    #[test]
    fn recognizes_a_prompt() {
        let mut state = blocked(Some(1));
        assert!(state.note_screen(ASKING));
        assert_eq!(texts(&state), Some(vec!["Yes".into(), "No".into()]));
        assert_eq!(state.prompt().unwrap().question, ["Do you want to proceed?"]);
        // 同一幅再读一次，提问没变。
        assert!(!state.note_screen(ASKING));
        assert!(state.note_screen("$ ls\nREADME.md\n"));
        assert!(state.prompt().is_none());
    }

    #[test]
    fn expects_the_same_prompt_for_a_listed_option() {
        let mut state = blocked(None);
        assert_eq!(state.expected(1), None);
        state.note_screen(ASKING);
        assert_eq!(state.expected(2), Asked::on(ASKING));
        // 光标挪了、提问下面的状态栏变了都还是这个提问；换了问题就不是了。
        assert_eq!(state.expected(2), Asked::on(MOVED));
        assert_eq!(state.expected(2), Asked::on(&format!("{ASKING} 12s elapsed\n")));
        assert_ne!(state.expected(2), Asked::on(NEXT));
        assert_eq!(state.expected(3), None);
    }

    #[test]
    fn the_answered_prompt_is_not_recognized_while_other_lines_change() {
        let mut state = blocked(Some(1));
        state.note_screen(&format!("{ASKING} 12s elapsed\n"));
        state.answered();
        assert!(!state.note_screen(&format!("{ASKING} 13s elapsed\n")));
        assert!(state.prompt().is_none());
        assert!(state.awaiting_change());
    }

    #[test]
    fn the_answered_screen_is_not_recognized_again() {
        let mut state = blocked(Some(1));
        state.note_screen(ASKING);
        assert!(state.answered());
        assert!(state.prompt().is_none());
        assert!(!state.note_screen(ASKING));
        assert!(state.prompt().is_none());
        assert!(state.awaiting_change());
    }

    #[test]
    fn a_screen_that_only_moved_the_marker_is_not_recognized() {
        let mut state = blocked(Some(1));
        state.note_screen(ASKING);
        state.answered();
        assert!(!state.note_screen(MOVED));
        assert!(state.prompt().is_none());
    }

    #[test]
    fn a_changed_screen_is_recognized() {
        let mut state = blocked(Some(1));
        state.note_screen(ASKING);
        state.answered();
        assert!(state.note_screen(NEXT));
        assert_eq!(state.prompt().unwrap().question, ["Allow edits to src/main.rs?"]);
        assert!(!state.awaiting_change());
        // 答过的记录随之作废，之后又读到答之前那幅也照认。
        assert!(state.note_screen(ASKING));
    }

    #[test]
    fn a_new_wait_with_the_same_screen_is_recognized() {
        let mut state = blocked(Some(1));
        state.note_screen(ASKING);
        state.answered();
        state.meta(true, Some(2));
        assert!(state.note_screen(ASKING));
        assert!(state.prompt().is_some());
        assert!(!state.awaiting_change());
    }

    #[test]
    fn without_times_the_screen_decides() {
        let mut state = blocked(None);
        state.note_screen(ASKING);
        state.answered();
        assert!(!state.note_screen(ASKING));
        assert!(state.prompt().is_none());
    }

    #[test]
    fn the_prompt_comes_back_when_the_rereads_run_out() {
        let mut state = blocked(Some(1));
        state.note_screen(ASKING);
        state.answered();
        for _ in 0..ANSWERED_REREADS {
            assert!(!state.note_screen(ASKING));
            assert!(state.prompt().is_none());
        }
        assert!(state.note_screen(ASKING));
        assert!(state.prompt().is_some());
        assert!(!state.awaiting_change());
    }

    /// 读屏总是读到 `screen`；按的键记在返回的表里。
    fn fixed(screen: &'static str) -> (Access, Arc<Mutex<Vec<Vec<String>>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let keys = sent.clone();
        let access = Access::new(
            move |_, _| Ok(screen.to_owned()),
            move |_, list| {
                keys.lock().unwrap().push(list);
                Ok(())
            },
        );
        (access, sent)
    }

    #[test]
    fn answering_is_published_until_the_keys_are_sent() {
        let cx = gpui::TestAppContext::single();
        let key = (HostId::Local, SessionId(1));
        let (access, sent) = fixed(ASKING);
        cx.update(|cx| watch(key.clone(), access, true, Some(1), cx));
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(prompt(&key, cx).is_some());
            assert!(!answering(&key, cx));
            answer(&key, 1, cx);
            assert!(answering(&key, cx));
        });
        cx.executor().advance_clock(READ_GAP);
        cx.run_until_parked();
        assert_eq!(*sent.lock().unwrap(), [vec!["1".to_owned()]]);
        cx.update(|cx| {
            assert!(!answering(&key, cx));
            assert!(prompt(&key, cx).is_none());
        });
    }

    /// 读屏总是读到 `ASKING`，读了几次记在返回的数里。
    fn counted() -> (Access, Arc<Mutex<usize>>) {
        let reads = Arc::new(Mutex::new(0));
        let count = reads.clone();
        let access = Access::new(
            move |_, _| {
                *count.lock().unwrap() += 1;
                Ok(ASKING.to_owned())
            },
            |_, _| Ok(()),
        );
        (access, reads)
    }

    #[test]
    fn reporting_the_same_wait_again_rereads_only_after_the_gap() {
        let cx = gpui::TestAppContext::single();
        let key = (HostId::Local, SessionId(1));
        let (access, reads) = counted();
        let report = |since: Option<u64>| {
            cx.update(|cx| watch(key.clone(), access.clone(), true, since, cx));
            cx.run_until_parked();
        };
        report(Some(1));
        assert_eq!(*reads.lock().unwrap(), 1);
        // 同一次等待再报几遍：只排上一次，到 `SETTLED_GAP` 才读。
        for _ in 0..5 {
            report(Some(1));
        }
        cx.executor().advance_clock(READ_GAP);
        cx.run_until_parked();
        assert_eq!(*reads.lock().unwrap(), 1);
        cx.executor().advance_clock(SETTLED_GAP);
        cx.run_until_parked();
        assert_eq!(*reads.lock().unwrap(), 2);
        // 新的一次等待照 `READ_GAP` 读。
        report(Some(2));
        cx.executor().advance_clock(READ_GAP);
        cx.run_until_parked();
        assert_eq!(*reads.lock().unwrap(), 3);
    }

    /// 同一次等待又报来、排着 `SETTLED_GAP` 的读屏时按下的回答不等它，照 `READ_GAP` 办。
    #[test]
    fn answering_does_not_wait_for_the_settled_reread() {
        let cx = gpui::TestAppContext::single();
        let key = (HostId::Local, SessionId(1));
        let (access, sent) = fixed(ASKING);
        cx.update(|cx| watch(key.clone(), access.clone(), true, Some(1), cx));
        cx.run_until_parked();
        cx.update(|cx| {
            watch(key.clone(), access, true, Some(1), cx);
            answer(&key, 1, cx);
        });
        cx.executor().advance_clock(READ_GAP);
        cx.run_until_parked();
        assert_eq!(*sent.lock().unwrap(), [vec!["1".to_owned()]]);
    }

    /// 一处按较早读到的状态报不在等（时刻比这次等待早），认出的提问和在路上的回答都留着。
    #[test]
    fn an_older_report_does_not_forget_the_wait() {
        let cx = gpui::TestAppContext::single();
        let key = (HostId::Local, SessionId(1));
        let (access, sent) = fixed(ASKING);
        cx.update(|cx| watch(key.clone(), access.clone(), true, Some(5), cx));
        cx.run_until_parked();
        cx.update(|cx| {
            watch(key.clone(), access.clone(), false, Some(4), cx);
            assert!(prompt(&key, cx).is_some());
            answer(&key, 1, cx);
        });
        cx.executor().advance_clock(READ_GAP);
        cx.run_until_parked();
        assert_eq!(*sent.lock().unwrap(), [vec!["1".to_owned()]]);
        // 时刻更晚的才算数。
        cx.update(|cx| {
            watch(key.clone(), access, false, Some(6), cx);
            assert!(prompt(&key, cx).is_none());
        });
    }

    /// 已经报过不在等、忘掉了以后，一处按较早读到的状态报在等，不重新跟上；会话结束后再来的照常跟。
    #[test]
    fn an_older_wait_does_not_come_back_after_forgetting() {
        let cx = gpui::TestAppContext::single();
        let key = (HostId::Local, SessionId(1));
        let (access, _) = fixed(ASKING);
        cx.update(|cx| watch(key.clone(), access.clone(), true, Some(1), cx));
        cx.run_until_parked();
        cx.update(|cx| {
            watch(key.clone(), access.clone(), false, Some(2), cx);
            watch(key.clone(), access.clone(), true, Some(1), cx);
        });
        cx.run_until_parked();
        cx.update(|cx| assert!(prompt(&key, cx).is_none()));
        cx.update(|cx| {
            ended(&key, cx);
            watch(key.clone(), access, true, Some(1), cx);
        });
        cx.run_until_parked();
        cx.update(|cx| assert!(prompt(&key, cx).is_some()));
    }

    #[test]
    fn without_times_every_report_rereads() {
        let cx = gpui::TestAppContext::single();
        let key = (HostId::Local, SessionId(1));
        let (access, reads) = counted();
        for _ in 0..3 {
            cx.update(|cx| watch(key.clone(), access.clone(), true, None, cx));
            cx.executor().advance_clock(READ_GAP);
            cx.run_until_parked();
        }
        assert_eq!(*reads.lock().unwrap(), 3);
    }

    #[test]
    fn forgetting_drops_the_prompt_and_the_answer_on_the_way() {
        let cx = gpui::TestAppContext::single();
        let key = (HostId::Local, SessionId(1));
        let (access, sent) = fixed(ASKING);
        cx.update(|cx| watch(key.clone(), access, true, Some(1), cx));
        cx.run_until_parked();
        cx.update(|cx| {
            answer(&key, 1, cx);
            forget(&key, cx);
            assert!(prompt(&key, cx).is_none());
            assert!(!answering(&key, cx));
        });
        cx.executor().advance_clock(READ_GAP);
        cx.run_until_parked();
        assert!(sent.lock().unwrap().is_empty());
    }

    #[test]
    fn leaving_the_wait_clears_everything() {
        let mut state = blocked(Some(1));
        state.note_screen(ASKING);
        assert!(state.meta(false, None));
        assert!(state.prompt().is_none());
        // 答过的记录也作废：再进等回答、屏幕和时刻都没变也照认。
        let mut state = blocked(Some(1));
        state.note_screen(ASKING);
        state.answered();
        assert!(!state.meta(false, Some(1)));
        assert!(!state.awaiting_change());
        state.meta(true, Some(1));
        assert!(state.note_screen(ASKING));
    }
}
