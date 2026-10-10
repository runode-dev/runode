//! 模型那一页：管用户自己装的 runode-infer（大模型和决策模型的本机服务，不随 Runode 打包）。服务卡片
//! 下面用分段控件切换大模型和决策模型，各管各的默认模型（配置里的 `chat-model`、`decision-model`）
//! 和本机模型；托管 API 只有决策模型那边有。启停服务、填托管 API 的密钥、下载和删除本机的模型，都是
//! 去跑 `runode-infer … --json`，读它 stdout 上的 `{"status":"ok","data":…}`。密钥经它的 stdin
//! 交过去，Runode 自己不存。
//!
//! 从访达打开时 app 自己的 PATH 里没有 brew 装的目录，所以和模拟器页一样，用终端里 shell 报告的
//! PATH 找它，找不到再看安装脚本默认装的 `~/.runode-infer/bin`。下载要好几分钟，放进
//! `DecisionPulls`：关掉设置页也接着下，再打开时看得到进度。

use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    io::{BufRead as _, BufReader, Write as _},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use futures::StreamExt as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Div, ElementId, Entity, Global, Hsla, Role, SharedString, Stateful,
    Task, Window, div, prelude::*, px,
};
use serde_json::Value;

use super::{
    SettingsView,
    controls::{Cards, Colors, Press, button, dropdown, input_box, row, segmented},
    pages::{key_hint, key_title},
    picker::{PickItem, PickTarget},
};
use crate::ui::{
    a11y::Disable,
    text_field::{TextField, TextFieldEvent},
};

const PROGRAM: &str = "runode-infer";
const DOWNLOAD_PAGE: &str = "https://github.com/runode-dev/runode-infer/releases";

/// 这一页的状态，挂在 `SettingsView` 上。
#[derive(Default)]
pub(super) struct State {
    /// 上次读到的；`None` 是还没读完。
    snapshot: Option<Snapshot>,
    loading: Option<Task<()>>,
    /// 正在办的操作，办完前那个按钮灰着。
    busy: Option<Busy>,
    /// 上次操作失败的原因，按区块（`service`、`typesafe`、`cloudflare`、模型名）。
    errors: HashMap<String, String>,
    inputs: Option<Inputs>,
    /// 上次看到 `DecisionPulls` 里正在下的模型，有一个不在了就是下完了，重新读状态。
    pulling: Vec<String>,
    /// 分段控件选着哪一类；只记在界面里，不写配置。
    kind: Kind,
}

/// 模型的类型，runode-infer 的 `kind`。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Kind {
    #[default]
    Chat,
    Decision,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Decision => "decision",
        }
    }

    /// 配置里这一类的默认模型的键。
    fn key(self) -> &'static str {
        match self {
            Self::Chat => "chat-model",
            Self::Decision => "decision-model",
        }
    }

    /// runode-infer 给的 `kind`；旧版不给，那时只有决策模型。不认识的类型是 `None`，不列出来。
    fn parse(value: &Value) -> Option<Self> {
        match value.as_str() {
            None => Some(Self::Decision),
            Some("chat") => Some(Self::Chat),
            Some("decision") => Some(Self::Decision),
            Some(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Snapshot {
    /// PATH 里和 `~/.runode-infer/bin` 都没有。
    Missing,
    Failed(String),
    Ready(Status),
}

#[derive(Clone, Debug, PartialEq)]
struct Status {
    version: String,
    running: bool,
    address: String,
    typesafe: bool,
    cloudflare: bool,
    /// 内置目录里能下的，加上下好了、不在目录里的（`hf.co/…`）。
    models: Vec<Model>,
    /// 服务加载着的模型。
    loaded: Vec<String>,
    /// 托管 API 上的模型；旧版 runode-infer 不给，为空。
    hosted: Vec<Hosted>,
    /// 这个版本的 runode-infer 跑得了大模型：目录里有一项带 `kind` 就算。
    chat_supported: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Hosted {
    id: String,
    kind: Kind,
    /// 要哪家的密钥，`Provider::name` 的写法。
    provider: String,
}

#[derive(Clone, Debug, PartialEq)]
struct Model {
    id: String,
    description: String,
    size: u64,
    installed: bool,
    /// runode-infer 说这台机器跑得了它。
    supported: bool,
    kind: Kind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Busy {
    Service,
    Key(Provider),
    Remove,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Provider {
    TypeSafe,
    Cloudflare,
}

impl Provider {
    fn name(self) -> &'static str {
        match self {
            Self::TypeSafe => "typesafe",
            Self::Cloudflare => "cloudflare",
        }
    }
}

struct Inputs {
    typesafe: Entity<TextField>,
    account: Entity<TextField>,
    token: Entity<TextField>,
    _subscriptions: Vec<gpui::Subscription>,
}

/// 正在下的模型，按模型名。下完从这里去掉；失败的留着原因，直到再点一次下载。
#[derive(Default)]
pub(crate) struct DecisionPulls(HashMap<String, Pull>);

impl Global for DecisionPulls {}

#[derive(Clone, Debug, Default)]
struct Pull {
    completed: u64,
    total: u64,
    error: Option<String>,
}

/// 下载线程交给界面的消息。
enum PullEvent {
    Progress(u64, u64),
    Done(Result<(), String>),
}

impl SettingsView {
    /// 重新问一遍 runode-infer 的状态、能下的和下好的模型。
    pub(super) fn refresh_decision(&mut self, cx: &mut Context<Self>) {
        let path = self.shell_path.clone();
        let job = cx.background_spawn(async move { load(path.as_deref()) });
        self.decision.loading = Some(cx.spawn(async move |this, cx| {
            let snapshot = job.await;
            this.update(cx, |this, cx| {
                this.decision.snapshot = Some(snapshot);
                this.decision.loading = None;
                cx.notify();
            })
            .ok();
        }));
    }

    pub(super) fn render_decision(&mut self, colors: Colors, window: &mut Window, cx: &mut Context<Self>) -> Div {
        if self.decision.snapshot.is_none() && self.decision.loading.is_none() {
            self.refresh_decision(cx);
        }
        let mut cards = Cards::new(div().flex().flex_col(), colors);
        cards.section(tr("settings.section.decision_service"));
        let snapshot = self.decision.snapshot.clone();
        match &snapshot {
            None => cards.push(self.service_row(tr("settings.decision.loading"), None, colors)),
            Some(Snapshot::Missing) => {
                let open = button("decision-download", tr("settings.decision.download"), colors)
                    .on_press(cx, |_, _, cx| cx.open_url(DOWNLOAD_PAGE));
                cards.push(self.service_row(tr("settings.decision.missing"), Some(open.into_any_element()), colors));
            }
            Some(Snapshot::Failed(err)) => {
                let retry = button("decision-retry", tr("settings.decision.retry"), colors)
                    .on_press(cx, |this, _, cx| this.refresh_decision(cx));
                let message = rust_i18n::t!("settings.decision.failed", err = err).into_owned();
                cards.push(self.service_row(message, Some(retry.into_any_element()), colors));
            }
            Some(Snapshot::Ready(status)) => {
                let message = if status.running {
                    rust_i18n::t!("settings.decision.running", address = status.address, version = status.version)
                } else {
                    rust_i18n::t!("settings.decision.stopped", version = status.version)
                };
                let (id, label, command) = if status.running {
                    ("decision-stop", tr("settings.decision.stop"), "stop")
                } else {
                    ("decision-start", tr("settings.decision.start"), "start")
                };
                let control = self
                    .action_button(id, label, Busy::Service, colors)
                    .on_press(cx, move |this, _, cx| this.run_decision(Busy::Service, "service", &[command], None, cx));
                cards.push(self.service_row(message.into_owned(), Some(control.into_any_element()), colors));

                let kind = self.decision.kind;
                let options = [Kind::Chat, Kind::Decision]
                    .into_iter()
                    .map(|kind| {
                        (
                            kind.name().to_owned(),
                            SharedString::from(tr(&format!("settings.decision.kind_{}", kind.name()))),
                        )
                    })
                    .collect();
                let pick = segmented(
                    "models-kind",
                    tr("settings.decision.kind"),
                    options,
                    Some(kind.name()),
                    colors,
                    cx,
                    |this, value, _, cx| {
                        this.decision.kind = if value == Kind::Chat.name() { Kind::Chat } else { Kind::Decision };
                        cx.notify();
                    },
                );
                cards.raw(div().pt(px(16.)).pb(px(8.)).flex().child(pick).into_any_element());

                if kind == Kind::Chat && !status.chat_supported {
                    let open = button("models-download", tr("settings.decision.download"), colors)
                        .on_press(cx, |_, _, cx| cx.open_url(DOWNLOAD_PAGE));
                    cards.push(
                        div()
                            .pt(px(12.))
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(16.))
                            .child(notice(
                                "models-chat-unsupported",
                                tr("settings.decision.chat_unsupported"),
                                colors.fg,
                            ))
                            .child(div().pb(px(10.)).child(open))
                            .into_any_element(),
                    );
                    return cards.finish();
                }

                cards.push(self.default_model_row(kind, status, colors, cx).into_any_element());

                if kind == Kind::Decision {
                    cards.section(tr("settings.section.decision_keys"));
                    cards
                        .push(self.key_row(Provider::TypeSafe, status.typesafe, colors, window, cx).into_any_element());
                    cards.push(
                        self.key_row(Provider::Cloudflare, status.cloudflare, colors, window, cx).into_any_element(),
                    );
                }

                cards.section(tr("settings.section.decision_models"));
                for model in status.models.iter().filter(|model| model.kind == kind) {
                    let loaded = status.loaded.contains(&model.id);
                    cards.push(self.model_row(model, loaded, colors, cx).into_any_element());
                }
                let more = match kind {
                    Kind::Chat => "settings.decision.more_chat_models",
                    Kind::Decision => "settings.decision.more_models",
                };
                cards.push(notice("decision-more-models", tr(more), colors.fg.opacity(0.55)));
            }
        }
        cards.finish()
    }

    /// 这一类的默认模型：下拉框显示配置里写的，点开从下好的本机模型（决策模型还有填了密钥的托管模型）
    /// 里挑。
    fn default_model_row(&mut self, kind: Kind, status: &Status, colors: Colors, cx: &mut Context<Self>) -> Div {
        let key = kind.key();
        let current = self.config.values(key).into_iter().next();
        let label = current.clone().unwrap_or_else(|| tr("settings.decision.default_none"));
        let items = model_choices(kind, status, current.as_deref());
        let control = dropdown(("models-default", kind as usize), key_title(key), None, label, 220., colors).on_press(
            cx,
            move |this, window, cx| {
                let items = items.iter().map(|(value, detail)| {
                    let label = if value.is_empty() { tr("settings.decision.default_none") } else { value.clone() };
                    let item = PickItem::new(value.clone(), label);
                    if let Some(detail) = detail { item.with_detail(tr(detail)) } else { item }
                });
                let current = this.config.values(key).into_iter().next().unwrap_or_default();
                this.open_picker(key_title(key), items.collect(), Some(current), PickTarget::Value(key), window, cx);
            },
        );
        let reset = self.reset(key, colors, cx);
        let error = self.errors.get(key).cloned();
        row(tr("settings.decision.default_model"), Some(key_hint(key)), control, reset, error, colors)
    }

    /// 服务那一行：说明下面是状态（报给辅助工具的提示文字），右边是按钮。
    fn service_row(&self, message: String, control: Option<AnyElement>, colors: Colors) -> AnyElement {
        let hint: SharedString = tr("settings.decision.service_hint").into();
        let error = self.decision.errors.get("service").cloned();
        div()
            .flex()
            .flex_col()
            .child(row(tr("settings.decision.service"), Some(hint), div().children(control), None, None, colors))
            .child(notice("decision-status", message, colors.fg.opacity(0.7)))
            .children(error.map(|err| notice("decision-service-error", err, colors.error)))
            .into_any_element()
    }

    /// 办着别的操作时灰着、按了不办的按钮。
    fn action_button(&self, id: impl Into<ElementId>, label: String, busy: Busy, colors: Colors) -> Stateful<Div> {
        let disabled = self.decision.busy.is_some();
        let label = if self.decision.busy == Some(busy) { tr("settings.decision.working") } else { label };
        button(id, label, colors).aria_disabled(disabled).when(disabled, |button| button.opacity(0.5))
    }

    fn key_row(
        &mut self,
        provider: Provider,
        set: bool,
        colors: Colors,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let title = tr(&format!("settings.decision.{}", provider.name()));
        let hint: SharedString = tr(&format!("settings.decision.{}_hint", provider.name())).into();
        let busy = Busy::Key(provider);
        let control = if set {
            let clear = self
                .action_button(provider_id("decision-clear", provider), tr("settings.decision.clear"), busy, colors)
                .aria_label(rust_i18n::t!("settings.decision.clear_label", what = title).into_owned())
                .on_press(cx, move |this, _, cx| {
                    this.run_decision(busy, provider.name(), &["key", "rm", provider.name()], None, cx)
                });
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .id(provider_id("decision-saved", provider))
                        .role(Role::Label)
                        .aria_label(tr("settings.decision.key_set"))
                        .text_size(px(12.))
                        .text_color(colors.fg.opacity(0.6))
                        .child(tr("settings.decision.key_set")),
                )
                .child(clear)
        } else {
            let inputs = self.decision_inputs(window, cx);
            let save = self
                .action_button(provider_id("decision-save", provider), tr("settings.decision.save"), busy, colors)
                .aria_label(rust_i18n::t!("settings.decision.save_label", what = title).into_owned())
                .on_press(cx, move |this, _, cx| this.save_key(provider, cx));
            let fields = match provider {
                Provider::TypeSafe => vec![(inputs.0, 220.)],
                Provider::Cloudflare => vec![(inputs.1, 150.), (inputs.2, 170.)],
            };
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .children(
                    fields.into_iter().map(|(input, width)| input_box(input, Some(width), false, colors, window, cx)),
                )
                .child(save)
        };
        let error = self.decision.errors.get(provider.name()).cloned();
        row(title, Some(hint), control, None, error, colors)
    }

    /// 填密钥的三个输入框，第一次用到时建；按回车和点保存一样。
    fn decision_inputs(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<TextField>, Entity<TextField>, Entity<TextField>) {
        if self.decision.inputs.is_none() {
            let field = |label: &str, placeholder: &str, masked: bool, cx: &mut Context<Self>| {
                cx.new(|cx| {
                    let input = TextField::editing(String::new(), 0, cx)
                        .with_label(tr(label))
                        .with_placeholder(tr(placeholder));
                    if masked { input.masked() } else { input }
                })
            };
            let typesafe = field("settings.decision.typesafe", "settings.decision.key_placeholder", true, cx);
            let account = field("settings.decision.account", "settings.decision.account_placeholder", false, cx);
            let token = field("settings.decision.token", "settings.decision.token_placeholder", true, cx);
            let submit = |provider: Provider| {
                move |this: &mut Self,
                      _: &Entity<TextField>,
                      event: &TextFieldEvent,
                      _: &mut Window,
                      cx: &mut Context<Self>| {
                    if matches!(event, TextFieldEvent::Next) {
                        this.save_key(provider, cx);
                    }
                }
            };
            let subscriptions = vec![
                cx.subscribe_in(&typesafe, window, submit(Provider::TypeSafe)),
                cx.subscribe_in(&account, window, submit(Provider::Cloudflare)),
                cx.subscribe_in(&token, window, submit(Provider::Cloudflare)),
            ];
            self.decision.inputs = Some(Inputs { typesafe, account, token, _subscriptions: subscriptions });
        }
        let inputs = self.decision.inputs.as_ref().expect("just created");
        (inputs.typesafe.clone(), inputs.account.clone(), inputs.token.clone())
    }

    /// 把输入框里的密钥交给 `runode-infer key set`，成功后清空输入框。
    fn save_key(&mut self, provider: Provider, cx: &mut Context<Self>) {
        let Some(inputs) = &self.decision.inputs else { return };
        let text = |input: &Entity<TextField>| input.read(cx).query().trim().to_owned();
        let stdin = match provider {
            Provider::TypeSafe => text(&inputs.typesafe),
            Provider::Cloudflare => format!("{}\n{}", text(&inputs.account), text(&inputs.token)),
        };
        if stdin.lines().any(str::is_empty) || stdin.is_empty() {
            self.decision.errors.insert(provider.name().to_owned(), tr("settings.decision.key_empty"));
            cx.notify();
            return;
        }
        self.run_decision(Busy::Key(provider), provider.name(), &["key", "set", provider.name()], Some(stdin), cx);
    }

    fn model_row(&mut self, model: &Model, loaded: bool, colors: Colors, cx: &mut Context<Self>) -> Div {
        let pull = cx.try_global::<DecisionPulls>().and_then(|pulls| pulls.0.get(&model.id)).cloned();
        let mut hint = format!("{} · {}", model.description, size(model.size));
        if loaded {
            hint.push_str(" · ");
            hint.push_str(&tr("settings.decision.loaded"));
        }
        if !model.supported {
            hint.push_str(" · ");
            hint.push_str(&tr("settings.decision.cannot_run"));
        }
        let id = model.id.clone();
        let element_id = |prefix: &str| SharedString::from(format!("{prefix}-{}", model.id));
        let control = match &pull {
            Some(pull) if pull.error.is_none() => {
                let percent = (pull.completed * 100).checked_div(pull.total).unwrap_or(0);
                let text = rust_i18n::t!("settings.decision.pulling", percent = percent).into_owned();
                div()
                    .id(element_id("decision-progress"))
                    .role(Role::ProgressIndicator)
                    .aria_label(rust_i18n::t!("settings.decision.pull_label", model = model.id).into_owned())
                    .aria_value(text.clone())
                    .text_size(px(12.))
                    .text_color(colors.fg.opacity(0.7))
                    .child(text)
                    .into_any_element()
            }
            _ if model.installed => {
                let label = rust_i18n::t!("settings.decision.remove_label", model = model.id).into_owned();
                self.action_button(element_id("decision-remove"), tr("settings.decision.remove"), Busy::Remove, colors)
                    .aria_label(label)
                    .on_press(cx, move |this, _, cx| this.run_decision(Busy::Remove, &id, &["rm", &id], None, cx))
                    .into_any_element()
            }
            _ => {
                let label = rust_i18n::t!("settings.decision.pull_label", model = model.id).into_owned();
                button(element_id("decision-pull"), tr("settings.decision.pull"), colors)
                    .aria_label(label)
                    .on_press(cx, move |this, _, cx| this.pull_model(id.clone(), cx))
                    .into_any_element()
            }
        };
        let error = pull.and_then(|pull| pull.error).or_else(|| self.decision.errors.get(&model.id).cloned());
        row(model.id.clone(), Some(hint.into()), control, None, error, colors)
    }

    /// 在后台跑 `runode-infer <args> --json`，`stdin` 有的话经 stdin 交过去；办完重新读状态，失败的原因
    /// 记在 `area` 下面。
    fn run_decision(&mut self, busy: Busy, area: &str, args: &[&str], stdin: Option<String>, cx: &mut Context<Self>) {
        if self.decision.busy.is_some() {
            return;
        }
        let Some(program) = find_program(self.shell_path.as_deref()) else {
            self.decision.snapshot = Some(Snapshot::Missing);
            cx.notify();
            return;
        };
        self.decision.busy = Some(busy);
        self.decision.errors.remove(area);
        let (area, path) = (area.to_owned(), self.shell_path.clone());
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let job = cx.background_spawn(async move {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            run(&program, path.as_deref(), &args, stdin.as_deref())
        });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                this.decision.busy = None;
                if let Err(err) = result {
                    this.decision.errors.insert(area, err);
                } else if let Some(inputs) = &this.decision.inputs {
                    // 存好了密钥：输入框里的不再留着。
                    if matches!(busy, Busy::Key(_)) {
                        for input in [&inputs.typesafe, &inputs.account, &inputs.token] {
                            input.update(cx, |input, cx| input.set_query(String::new(), cx));
                        }
                    }
                }
                this.refresh_decision(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// 开始下载 `model`；进度和结果在 `DecisionPulls` 里，下完重新读状态。
    fn pull_model(&mut self, model: String, cx: &mut Context<Self>) {
        let Some(program) = find_program(self.shell_path.as_deref()) else {
            self.decision.snapshot = Some(Snapshot::Missing);
            cx.notify();
            return;
        };
        self.decision.errors.remove(&model);
        start_pull(program, self.shell_path.clone(), model, tr("settings.decision.pull_stopped"), cx);
    }

    /// 下载有了进展；有下完的（不在 `DecisionPulls` 里了）就重新读一遍状态。
    pub(super) fn pulls_changed(&mut self, cx: &mut Context<Self>) {
        let now: Vec<String> =
            cx.try_global::<DecisionPulls>().map(|pulls| pulls.0.keys().cloned().collect()).unwrap_or_default();
        let finished = self.decision.pulling.iter().any(|model| !now.contains(model));
        self.decision.pulling = now;
        if finished {
            self.refresh_decision(cx);
        }
        cx.notify();
    }
}

/// `stopped` 是下载进程没说原因就退出时报的错。
fn start_pull(program: PathBuf, path: Option<OsString>, model: String, stopped: String, cx: &mut App) {
    let pulls = &mut cx.default_global::<DecisionPulls>().0;
    if pulls.get(&model).is_some_and(|pull| pull.error.is_none()) {
        return;
    }
    pulls.insert(model.clone(), Pull::default());
    let (tx, mut rx) = futures::channel::mpsc::unbounded();
    let name = model.clone();
    std::thread::spawn(move || {
        let result = pull(&program, path.as_deref(), &name, stopped, |completed, total| {
            let _ = tx.unbounded_send(PullEvent::Progress(completed, total));
        });
        let _ = tx.unbounded_send(PullEvent::Done(result));
    });
    cx.spawn(async move |cx| {
        while let Some(event) = rx.next().await {
            cx.update(|cx| {
                let pulls = &mut cx.default_global::<DecisionPulls>().0;
                match event {
                    PullEvent::Progress(completed, total) => {
                        if let Some(pull) = pulls.get_mut(&model) {
                            (pull.completed, pull.total) = (completed, total);
                        }
                    }
                    PullEvent::Done(Ok(())) => drop(pulls.remove(&model)),
                    PullEvent::Done(Err(err)) => {
                        if let Some(pull) = pulls.get_mut(&model) {
                            pull.error = Some(err);
                        }
                    }
                }
            });
        }
    })
    .detach();
}

/// 默认模型下拉框里的选项：(值, 补充说明的翻译键)。先是「不设」（空值，删掉这个键），再是这一类下好的
/// 本机模型，决策模型再加上填了密钥的托管模型；配置里写着、列表里又没有的值也放进去，标上不在
/// runode-infer 里，不悄悄吞掉用户手写的值。
fn model_choices(kind: Kind, status: &Status, current: Option<&str>) -> Vec<(String, Option<&'static str>)> {
    let keyed = |provider: &str| match provider {
        "typesafe" => status.typesafe,
        "cloudflare" => status.cloudflare,
        _ => false,
    };
    let local = status.models.iter().filter(|m| m.kind == kind && m.installed).map(|m| &m.id);
    let hosted = status.hosted.iter().filter(|h| h.kind == kind && keyed(&h.provider)).map(|h| &h.id);
    let mut items: Vec<(String, Option<&'static str>)> =
        std::iter::once(String::new()).chain(local.chain(hosted).cloned()).map(|id| (id, None)).collect();
    if let Some(current) = current
        && !items.iter().any(|(id, _)| id == current)
    {
        items.push((current.to_owned(), Some("settings.decision.not_in_infer")));
    }
    items
}

/// 只起提示作用的一行字（状态、出错），报给辅助工具。
fn notice(id: &'static str, text: impl Into<SharedString>, color: Hsla) -> AnyElement {
    let text = text.into();
    div()
        .id(id)
        .role(Role::Label)
        .aria_label(text.clone())
        .pb(px(10.))
        .text_size(px(12.))
        .text_color(color)
        .child(text)
        .into_any_element()
}

fn tr(key: &str) -> String {
    crate::i18n::tr(key)
}

fn provider_id(prefix: &str, provider: Provider) -> SharedString {
    format!("{prefix}-{}", provider.name()).into()
}

/// 给人看的大小，比如 `6.5 GB`。
fn size(bytes: u64) -> String {
    let bytes = bytes as f64;
    if bytes >= 1e9 { format!("{:.1} GB", bytes / 1e9) } else { format!("{:.0} MB", bytes / 1e6) }
}

/// runode-infer 装在哪：`path` 里的各个目录，再是安装脚本默认装的 `~/.runode-infer/bin`。
fn find_program(path: Option<&OsStr>) -> Option<PathBuf> {
    let dirs: Vec<PathBuf> = path.map(|path| std::env::split_paths(path).collect()).unwrap_or_default();
    let installed = runode_paths::Dirs::from_env().home.map(|home| home.join(".runode-infer").join("bin"));
    dirs.into_iter().chain(installed).map(|dir| dir.join(PROGRAM)).find(|program| program.is_file())
}

fn command(program: &Path, path: Option<&OsStr>) -> Command {
    let mut command = Command::new(program);
    // 它拉起的引擎也按终端里的 PATH 找东西。
    if let Some(path) = path {
        command.env("PATH", path);
    }
    command
}

/// 跑 `runode-infer <args> --json`，交回 `data`；出错时交回它说的原因。
fn run(program: &Path, path: Option<&OsStr>, args: &[&str], stdin: Option<&str>) -> Result<Value, String> {
    let mut child = command(program, path)
        .args(args)
        .arg("--json")
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("{PROGRAM}: {err}"))?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // 写完就关上，它读到结尾才往下走。
        let _ = pipe.write_all(text.as_bytes()).and_then(|()| pipe.write_all(b"\n"));
    }
    let output = child.wait_with_output().map_err(|err| format!("{PROGRAM}: {err}"))?;
    reply(&output.stdout, &output.stderr)
}

/// stdout 最后一行是 `{"status":"ok","data":…}` 或 `{"status":"error","error":"…"}`；读不出时用 stderr。
fn reply(stdout: &[u8], stderr: &[u8]) -> Result<Value, String> {
    let text = String::from_utf8_lossy(stdout);
    let last: Option<Value> =
        text.lines().rev().find(|line| !line.trim().is_empty()).and_then(|line| serde_json::from_str(line).ok());
    match last {
        Some(mut json) if json["status"] == "ok" => Ok(json["data"].take()),
        Some(json) if json["error"].is_string() => Err(json["error"].as_str().unwrap_or_default().to_owned()),
        _ => {
            let text = String::from_utf8_lossy(stderr);
            let line = text.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or("no output");
            Err(format!("{PROGRAM}: {line}"))
        }
    }
}

/// 读状态、能下的模型、下好的模型和加载着的模型。
fn load(path: Option<&OsStr>) -> Snapshot {
    let Some(program) = find_program(path) else { return Snapshot::Missing };
    match load_status(&program, path) {
        Ok(status) => Snapshot::Ready(status),
        Err(err) => Snapshot::Failed(err),
    }
}

fn load_status(program: &Path, path: Option<&OsStr>) -> Result<Status, String> {
    let status = run(program, path, &["status"], None)?;
    let catalog = run(program, path, &["catalog"], None)?;
    let installed = run(program, path, &["list"], None)?;
    let running = status["running"].as_bool().unwrap_or(false);
    let loaded = if running { run(program, path, &["ps"], None)? } else { Value::Null };
    Ok(parse_status(&status, &catalog, &installed, &loaded))
}

fn parse_status(status: &Value, catalog: &Value, installed: &Value, loaded: &Value) -> Status {
    let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
    let models_of = |value: &Value| value["models"].as_array().cloned().unwrap_or_default();
    // runode-infer 说这台机器跑不了的（比如 MLX 模型在 Intel Mac 上）不列出来，免得白下几个 GB；
    // 已经下好了的照样列出来，能删掉。
    let catalog_models = models_of(catalog);
    let mut models: Vec<Model> = catalog_models
        .iter()
        .filter(|m| m["supported"].as_bool().unwrap_or(true) || m["installed"].as_bool().unwrap_or(false))
        .filter_map(|m| {
            Some(Model {
                id: text(&m["model"]),
                description: text(&m["description"]),
                size: m["size"].as_u64().unwrap_or(0),
                installed: m["installed"].as_bool().unwrap_or(false),
                supported: m["supported"].as_bool().unwrap_or(true),
                kind: Kind::parse(&m["kind"])?,
            })
        })
        .collect();
    for m in models_of(installed) {
        let id = text(&m["model"]);
        let Some(kind) = Kind::parse(&m["kind"]) else { continue };
        if !models.iter().any(|model| model.id == id) {
            let description = text(&m["repo"]);
            models.push(Model {
                id,
                description,
                size: m["size"].as_u64().unwrap_or(0),
                installed: true,
                supported: true,
                kind,
            });
        }
    }
    let hosted = catalog["hosted"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|h| {
            Some(Hosted { id: text(&h["model"]), kind: Kind::parse(&h["kind"])?, provider: text(&h["provider"]) })
        })
        .collect();
    Status {
        version: text(&status["version"]),
        running: status["running"].as_bool().unwrap_or(false),
        address: text(&status["address"]),
        typesafe: status["keys"]["typesafe"].as_bool().unwrap_or(false),
        cloudflare: status["keys"]["cloudflare"].as_bool().unwrap_or(false),
        models,
        loaded: models_of(loaded).iter().map(|m| text(&m["model"])).collect(),
        hosted,
        chat_supported: catalog_models.iter().any(|m| m.get("kind").is_some()),
    }
}

/// 跑 `runode-infer pull <model> --json`，每读到一行进度调一次 `progress`。
fn pull(
    program: &Path,
    path: Option<&OsStr>,
    model: &str,
    stopped: String,
    mut progress: impl FnMut(u64, u64),
) -> Result<(), String> {
    let mut child = command(program, path)
        .args(["pull", model, "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| format!("{PROGRAM}: {err}"))?;
    let mut last = Err(stopped);
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Ok(json) = serde_json::from_str::<Value>(&line) else { continue };
            match json["status"].as_str() {
                Some("progress") => {
                    progress(json["completed"].as_u64().unwrap_or(0), json["total"].as_u64().unwrap_or(0))
                }
                Some("ok") => last = Ok(()),
                Some("error") => last = Err(json["error"].as_str().unwrap_or_default().to_owned()),
                _ => {}
            }
        }
    }
    let _ = child.wait();
    last
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reads_the_last_line_of_the_reply() {
        let stdout = b"{\"status\":\"progress\",\"completed\":1,\"total\":2}\n{\"status\":\"ok\",\"data\":{\"a\":1}}\n";
        assert_eq!(reply(stdout, b""), Ok(json!({"a": 1})));
        assert_eq!(reply(b"{\"status\":\"error\",\"error\":\"nope\"}\n", b""), Err("nope".to_owned()));
        assert_eq!(reply(b"", b"boom\n\n"), Err("runode-infer: boom".to_owned()));
    }

    #[test]
    fn merges_the_catalog_with_downloaded_models() {
        let status = json!({"version": "0.1.0", "running": true, "address": "127.0.0.1:11436",
                            "keys": {"typesafe": true, "cloudflare": false}});
        let catalog = json!({"models": [
            {"model": "clef-flash:gguf-q4_k_m", "description": "Clef-Flash 9B, 4-bit", "size": 6486448288u64, "installed": true},
            {"model": "clef:gguf-q4_k_m", "description": "Clef 27B, 4-bit", "size": 19232219200u64, "installed": false},
            {"model": "clef-flash:mlx-4bit", "description": "Clef-Flash 9B, MLX", "size": 1, "installed": false, "supported": false},
            {"model": "clef:mlx-4bit", "description": "Clef 27B, MLX", "size": 1, "installed": true, "supported": false}]});
        let installed = json!({"models": [
            {"model": "clef-flash:gguf-q4_k_m", "repo": "ggml-org/Clef-Flash-GGUF", "size": 6486448288u64},
            {"model": "hf.co/org/repo:Q8_0", "repo": "org/repo", "size": 1000}]});
        let loaded = json!({"models": [{"model": "clef-flash:gguf-q4_k_m", "state": "ready"}]});
        let status = parse_status(&status, &catalog, &installed, &loaded);
        assert!(status.running && status.typesafe && !status.cloudflare);
        let ids: Vec<_> = status.models.iter().map(|m| (m.id.as_str(), m.installed)).collect();
        assert_eq!(
            ids,
            [
                ("clef-flash:gguf-q4_k_m", true),
                ("clef:gguf-q4_k_m", false),
                ("clef:mlx-4bit", true),
                ("hf.co/org/repo:Q8_0", true)
            ]
        );
        assert!(!status.models[2].supported && status.models[0].supported);
        assert_eq!(status.loaded, ["clef-flash:gguf-q4_k_m"]);
        // 旧版没有 `kind` 也没有 `hosted`：一律当决策模型，托管的为空，跑不了大模型。
        assert!(status.models.iter().all(|m| m.kind == Kind::Decision));
        assert!(status.hosted.is_empty() && !status.chat_supported);
    }

    #[test]
    fn reads_kinds_and_hosted_models() {
        let status = json!({"version": "0.2.0", "running": false, "keys": {"typesafe": false, "cloudflare": true}});
        let catalog = json!({
            "models": [
                {"model": "gemma-4-e4b:gguf-q4_0", "size": 1, "installed": true, "kind": "chat"},
                {"model": "clef-flash:gguf-q4_k_m", "size": 1, "installed": true, "kind": "decision"},
                {"model": "embed:gguf", "size": 1, "installed": true, "kind": "embedding"}],
            "hosted": [
                {"model": "jev-latest", "kind": "decision", "provider": "typesafe"},
                {"model": "cf/clef-flash", "kind": "decision", "provider": "cloudflare"}]});
        let installed = json!({"models": [
            {"model": "hf.co/org/chat:Q4_K_M", "repo": "org/chat", "size": 1, "kind": "chat"},
            {"model": "hf.co/org/old:Q8_0", "repo": "org/old", "size": 1}]});
        let status = parse_status(&status, &catalog, &installed, &Value::Null);
        assert!(status.chat_supported);
        let kinds: Vec<_> = status.models.iter().map(|m| (m.id.as_str(), m.kind)).collect();
        // 不认识的类型不列出来；`list` 里没写类型的按决策模型算。
        assert_eq!(
            kinds,
            [
                ("gemma-4-e4b:gguf-q4_0", Kind::Chat),
                ("clef-flash:gguf-q4_k_m", Kind::Decision),
                ("hf.co/org/chat:Q4_K_M", Kind::Chat),
                ("hf.co/org/old:Q8_0", Kind::Decision)
            ]
        );
        assert_eq!(
            status.hosted,
            [
                Hosted { id: "jev-latest".into(), kind: Kind::Decision, provider: "typesafe".into() },
                Hosted { id: "cf/clef-flash".into(), kind: Kind::Decision, provider: "cloudflare".into() }
            ]
        );

        // 默认模型的选项：不设、这一类下好的本机模型、填了密钥的托管模型，再是列表里没有的当前值。
        let values = |kind, current| {
            model_choices(kind, &status, current)
                .into_iter()
                .map(|(id, detail)| (id, detail.is_some()))
                .collect::<Vec<_>>()
        };
        let item = |id: &str, missing| (id.to_owned(), missing);
        assert_eq!(
            values(Kind::Chat, None),
            [item("", false), item("gemma-4-e4b:gguf-q4_0", false), item("hf.co/org/chat:Q4_K_M", false)]
        );
        assert_eq!(
            values(Kind::Decision, Some("cf/clef-flash")),
            [
                item("", false),
                item("clef-flash:gguf-q4_k_m", false),
                item("hf.co/org/old:Q8_0", false),
                item("cf/clef-flash", false)
            ]
        );
        // jev-latest 没填 TypeSafe 的密钥，不在列表里：手写了的照样列出来，标上不在 runode-infer 里。
        assert_eq!(values(Kind::Decision, Some("jev-latest")).last(), Some(&item("jev-latest", true)));
        assert_eq!(values(Kind::Chat, Some("")).len(), 3);
    }

    #[test]
    fn sizes() {
        assert_eq!(size(6_486_448_288), "6.5 GB");
        assert_eq!(size(624_229_728), "624 MB");
    }
}
