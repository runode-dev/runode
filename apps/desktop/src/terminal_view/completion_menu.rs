//! 按 Tab 弹出的命令补全菜单：什么时候由 runode 接管 Tab、菜单开着时的按键、鼠标和刷新。
//! 菜单怎么画在 `paint` 里，切词、匹配和插入的计算在 `completion` 里。

mod paint;

use std::{ffi::OsString, path::PathBuf, time::Instant};

use gpui::{
    AccessibleAction, AnyElement, Context, Keystroke, MouseDownEvent, Role, ScrollWheelEvent, Task, anchored, div,
    point, prelude::*, px,
};

use runode_completion::{
    self as completion, Candidate, GeneratorJob, GeneratorResults, Kind, Request, Shell, generators,
};
use runode_terminal::session::Session;

use super::{
    ECHO_WAIT, TerminalView,
    input::{take_whole_lines, wheel_lines},
};

/// 开着的补全菜单。
pub(super) struct CompletionMenu {
    /// 当前词之前的那部分输入；它变了说明光标离开了这个词或者输入已经提交，菜单关掉。
    before_word: String,
    /// 当前词的起点到光标占几格；词折到了上一行时，菜单往上画要让开整个词。
    cells_before_cursor: usize,
    /// shell 打开菜单时所在的目录，列文件和跑生成器都在这里；读不到或者已经不存在时不跑
    /// 生成器。
    cwd: Option<PathBuf>,
    /// shell 集成报告的 PATH 和各种名字：补命令名、跑生成器时用。
    shell: Shell,
    /// 当前词光标前的部分，候选按它过滤。
    typed: String,
    /// 不用跑命令就有的候选，输入一变就重算。
    local: Vec<Candidate>,
    generated: Vec<Generated>,
    /// `local` 加上已经跑完的生成器结果。
    candidates: Vec<Candidate>,
    /// 列出来的候选在 `candidates` 里的下标，排好了序。
    items: Vec<usize>,
    /// `candidates` 里能列出来的一共几项，见 `completion::total`；随候选重算，画的时候直接用。
    total: usize,
    /// `items` 里出现的分组，见 `paint::groups`；随候选重算，画的时候直接用。
    groups: Vec<(Kind, Option<String>)>,
    /// 选中的是 `items` 里的第几项。
    selected: usize,
    /// 显示的第一行是 `items` 里的第几项，画的时候按能放下的行数调整，让选中项总在里面。
    top: usize,
    /// 滚轮不足一行的余量。
    scroll_remainder: f32,
    /// 上次画在网格里的位置，点击和滚轮按它判断落在哪一项。
    shown: Option<paint::Shown>,
    /// 用户用方向键、Ctrl+P/N 或滚轮移动过选中项。
    moved: bool,
    /// 刚按 Tab 打开、用户还没动过：等生成器跑完再按候选的个数决定直接插入、把 Tab 交还
    /// shell 还是留着菜单。
    fresh: bool,
}

/// 一个生成器的结果。
struct Generated {
    command: String,
    from: usize,
    group: u32,
    /// 还在跑时为 `None`。
    results: Option<Vec<Candidate>>,
    /// 丢掉就取消，命令还在跑时会被杀掉。
    _job: Option<generators::Job>,
    _task: Task<()>,
}

impl Generated {
    /// 是不是 `job` 这个生成器的结果：命令、起点和组号都一样。
    fn is_for(&self, job: &GeneratorJob) -> bool {
        self.command == job.command && self.from == job.from && self.group == job.group
    }
}

/// 回显之前按下的补全键，等屏幕上的输入跟上了再处理。
pub(super) enum PendingKey {
    Tab,
    /// 接受这个候选；带着当时当前词之前的输入，变了就不接受。
    Accept(Candidate, String),
}

impl CompletionMenu {
    fn loading(&self) -> bool {
        self.generated.iter().any(|g| g.results.is_none())
    }

    /// 重新合并候选、按 `typed` 排序。用户还没移动过选中项时选中第一项；移动过时尽量还选中
    /// 插入文字相同的那一项，找不到再回到第一项。
    fn rebuild(&mut self) {
        let selected = self.items.get(self.selected).filter(|_| self.moved).map(|&i| {
            let c = &self.candidates[i];
            (c.from, c.value.clone())
        });
        self.candidates = self.local.clone();
        for generated in &self.generated {
            self.candidates.extend(generated.results.iter().flatten().cloned());
        }
        self.items = completion::rank(&self.candidates, &self.typed);
        self.total = completion::total(&self.candidates);
        self.groups = paint::groups(&self.candidates, &self.items);
        self.selected = selected
            .and_then(|(from, value)| {
                self.items.iter().position(|&i| self.candidates[i].from == from && self.candidates[i].value == value)
            })
            .unwrap_or(0);
    }

    /// 选中上一项（负数）或下一项，到头了绕回另一头。
    fn select(&mut self, delta: isize) {
        let len = self.items.len() as isize;
        if len == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
        self.fresh = false;
        self.moved = true;
    }

    fn selected_candidate(&self) -> Option<&Candidate> {
        self.items.get(self.selected).map(|&i| &self.candidates[i])
    }
}

impl TerminalView {
    /// 补全用到的按键：菜单开着时处理上下选择、接受和关闭，其他键照常交给 shell；没开时看
    /// 这次 Tab 要不要由 runode 接管。返回这个键是不是已经处理了。
    pub(super) fn completion_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) -> bool {
        let m = &keystroke.modifiers;
        let plain = !m.modified();
        let ctrl = m.control && !m.alt && !m.shift && !m.platform && !m.function;
        let key = keystroke.key.as_str();
        if let Some(menu) = &mut self.completion {
            match key {
                "up" if plain => menu.select(-1),
                "p" if ctrl => menu.select(-1),
                "down" if plain => menu.select(1),
                "n" if ctrl => menu.select(1),
                "escape" if plain => self.completion = None,
                "tab" | "enter" if plain => {
                    let Some(candidate) = menu.selected_candidate().cloned() else {
                        // 没有可选的候选：关掉菜单，这个键照常交给 shell。
                        self.completion = None;
                        return false;
                    };
                    let before = menu.before_word.clone();
                    self.completion = None;
                    self.accept_candidate(candidate, before, cx);
                }
                _ => return false,
            }
            return true;
        }
        key == "tab" && plain && self.complete_on_tab(cx)
    }

    /// 一次没有菜单时的 Tab：光标在提示符输入里、所在命令有规格、光标不在命令名上时由
    /// runode 补全，返回 true；否则返回 false，Tab 交给 shell。
    fn complete_on_tab(&mut self, cx: &mut Context<Self>) -> bool {
        // 没在看（重新连上的过程中、和宿主断开了）时也不接管，这个键随之丢掉。
        if !self.config.command_completions
            || self.marked_text.is_some()
            || !self.screen.live().is_some_and(|session| !session.has_selection() && session.viewport_at_bottom())
        {
            return false;
        }
        // 全屏程序里、光标不在提示符上时不管回显没回显都直接交给程序，不耽搁这个键。
        if self.screen.live().and_then(Session::prompt_input).is_none() {
            return false;
        }
        // 上一次 Tab 还在等回显。
        if self.completion_pending.is_some() {
            return true;
        }
        // 刚发出的输入还没回显，屏幕上的输入是旧的：等回显了再按新的输入处理。
        if self.echo_pending() {
            self.defer_completion(PendingKey::Tab, cx);
            return true;
        }
        self.open_completion(cx)
    }

    /// 按现在屏幕上的输入打开菜单；runode 补不了时返回 false。
    fn open_completion(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(request) = self.completion_request() else {
            return false;
        };
        let Some(session) = self.screen.live() else {
            return false;
        };
        self.completion = Some(CompletionMenu {
            before_word: request.before_word().to_owned(),
            cells_before_cursor: 0,
            cwd: session.cwd(),
            shell: Shell {
                path: session.shell_path(),
                names: session.shell_names(),
                usage: completion::usage::current(),
            },
            typed: String::new(),
            local: Vec::new(),
            generated: Vec::new(),
            candidates: Vec::new(),
            items: Vec::new(),
            total: 0,
            groups: Vec::new(),
            selected: 0,
            top: 0,
            scroll_remainder: 0.,
            shown: None,
            moved: false,
            fresh: true,
        });
        self.update_completion(&request, cx);
        self.settle_completion(&request);
        true
    }

    /// 从屏幕上读出提示符上的输入，看光标处能不能补全。
    fn completion_request(&self) -> Option<Request> {
        let input = self.screen.live()?.prompt_input()?;
        Request::new(&input.text, input.cursor)
    }

    /// 按新的输入重算候选：过滤用的词、文件列表；生成器的命令变了的重新跑，没变的留着。
    fn update_completion(&mut self, request: &Request, cx: &mut Context<Self>) {
        // shell 里有的这两个变量生成器也带上：`runode` 的会话补全靠它们找到这个 app 的命令行，
        // 知道是从哪个终端补的。
        let session = self.id.map(|id| (runode_protocol::ENV_SESSION.into(), id.to_string().into()));
        let bin = std::env::current_exe().ok().map(|exe| (runode_cli::ENV_BIN.into(), exe.into()));
        let vars: Vec<(OsString, OsString)> = session.into_iter().chain(bin).collect();
        let Some(menu) = &mut self.completion else {
            return;
        };
        menu.cells_before_cursor = request.cells_before_cursor();
        menu.typed = request.typed().to_owned();
        menu.local = request.local_candidates(menu.cwd.as_deref(), &menu.shell);
        // shell 的目录读不到或者已经不存在时不跑生成器，也不退回 runode 自己的目录。
        let jobs = match &menu.cwd {
            Some(cwd) if cwd.is_dir() => request.generator_jobs(),
            _ => Vec::new(),
        };
        // 不再需要的生成器丢掉，还在跑的随之取消。
        menu.generated.retain(|g| jobs.iter().any(|job| g.is_for(job)));
        for job in jobs {
            if menu.generated.iter().any(|g| g.is_for(&job)) {
                continue;
            }
            let env = generators::Environment {
                cwd: menu.cwd.clone().unwrap_or_default(),
                path: menu.shell.path.clone(),
                vars: vars.clone(),
            };
            let (handle, rx) = generators::spawn(job.command.clone(), env, job.parse);
            let (command, from, group) = (job.command.clone(), job.from, job.group);
            // 取消时这个任务随生成器一起丢掉，不会走到这里；收不到结果只可能是解析时 panic 或者
            // 线程没起来，当作没有结果收尾，菜单不会一直停在加载中。
            let task = cx.spawn(async move |this, cx| {
                let results = rx.await.unwrap_or_default();
                this.update(cx, |view, cx| view.generator_done(job, results, cx)).ok();
            });
            menu.generated.push(Generated { command, from, group, results: None, _job: Some(handle), _task: task });
        }
        menu.rebuild();
    }

    /// 一个生成器跑完了：把结果并进菜单。刚按 Tab 打开、候选都齐了时按个数决定怎么处理。
    fn generator_done(&mut self, job: GeneratorJob, results: GeneratorResults, cx: &mut Context<Self>) {
        let Some(menu) = &mut self.completion else {
            return;
        };
        let Some(generated) = menu.generated.iter_mut().find(|g| g.is_for(&job)) else {
            return;
        };
        generated.results = Some(completion::generated(results, &job));
        generated._job = None;
        menu.rebuild();
        if menu.fresh && !menu.loading() {
            let before = menu.before_word.clone();
            match self.completion_request().filter(|request| request.before_word() == before) {
                Some(request) => self.settle_completion(&request),
                None => self.completion = None,
            }
        }
        cx.notify();
    }

    /// 刚按 Tab 打开的菜单候选都齐了：只有一个就直接插入；没有就把这次 Tab 交给 shell；
    /// 有比已经写出的更长的公共开头就先插入它，菜单留着。刚发出的输入还没回显时屏幕上的
    /// 输入可能是旧的，不按它删字，只把菜单留着让用户自己选。
    fn settle_completion(&mut self, request: &Request) {
        let echo_pending = self.echo_pending();
        let Some(menu) = &mut self.completion else {
            return;
        };
        if !menu.fresh || menu.loading() {
            return;
        }
        menu.fresh = false;
        let decisive = completion::decisive(&menu.candidates, &menu.items, &menu.typed);
        match decisive.len() {
            0 => {
                self.completion = None;
                self.send_tab();
            }
            _ if echo_pending => {}
            1 => {
                let candidate = menu.candidates[decisive[0]].clone();
                self.completion = None;
                let edit = request.accept(&candidate);
                if let Some(session) = self.screen.live_mut() {
                    session.edit_input(edit.backspace, &edit.text);
                }
            }
            _ => {
                if let Some((from, prefix)) = completion::common_prefix(&menu.candidates, decisive, &menu.typed) {
                    let edit = request.insert_prefix(from, &prefix);
                    if let Some(session) = self.screen.live_mut() {
                        session.edit_input(edit.backspace, &edit.text);
                    }
                }
            }
        }
    }

    /// 有了新输出：菜单开着时按屏幕上的新输入重新过滤；光标离开了当前词、输入已经提交、
    /// 进了全屏程序时关掉菜单。之后处理等着回显的补全键。
    pub(super) fn completion_output(&mut self, cx: &mut Context<Self>) {
        self.output_at = Some(Instant::now());
        if let Some(menu) = &self.completion {
            let before = menu.before_word.clone();
            match self.completion_request().filter(|request| request.before_word() == before) {
                Some(request) => {
                    if let Some(menu) = &mut self.completion
                        && menu.typed != request.typed()
                    {
                        menu.fresh = false;
                    }
                    self.update_completion(&request, cx);
                }
                None => self.completion = None,
            }
        }
        if self.completion_pending.is_some() {
            self.run_pending_completion(cx);
        }
    }

    /// 刚发出的输入还没回显：之后屏幕上还没有输出，又没过多久。
    fn echo_pending(&self) -> bool {
        self.screen
            .live()
            .and_then(Session::last_input)
            .is_some_and(|at| at.elapsed() < ECHO_WAIT && self.output_at.is_none_or(|output| output < at))
    }

    /// 等回显了（或者等够了 `ECHO_WAIT`）再处理 `key`。
    fn defer_completion(&mut self, key: PendingKey, cx: &mut Context<Self>) {
        self.completion_pending = Some(key);
        self._completion_wait = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ECHO_WAIT).await;
            this.update(cx, |view, cx| {
                view.run_pending_completion(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    fn run_pending_completion(&mut self, cx: &mut Context<Self>) {
        match self.completion_pending.take() {
            Some(PendingKey::Tab) => {
                if !self.open_completion(cx) {
                    self.send_tab();
                }
            }
            Some(PendingKey::Accept(candidate, before)) => self.accept_candidate(candidate, before, cx),
            None => {}
        }
    }

    /// 接受一个候选：按现在屏幕上的输入算出要删的字和要写的字发给 shell。当前词之前的输入
    /// 和打开菜单时不一样了就不接受。
    fn accept_candidate(&mut self, candidate: Candidate, before: String, cx: &mut Context<Self>) {
        if self.echo_pending() {
            self.defer_completion(PendingKey::Accept(candidate, before), cx);
            return;
        }
        let Some(request) = self.completion_request().filter(|request| request.before_word() == before) else {
            return;
        };
        let edit = request.accept(&candidate);
        if let Some(session) = self.screen.live_mut() {
            session.edit_input(edit.backspace, &edit.text);
        }
    }

    /// 这次 Tab 由 shell 自己处理。
    fn send_tab(&mut self) {
        if let Some(session) = self.screen.live_mut() {
            session.send_text(b"\t");
        }
    }

    /// 菜单上次画在网格里的第几行；点击或滚轮落在 `position` 上时用。
    fn completion_row(&self, position: gpui::Point<gpui::Pixels>) -> Option<(i32, &paint::Shown)> {
        let shown = self.completion.as_ref()?.shown.as_ref()?;
        let row = self.grid_point(position)?.y.floor() as i32;
        shown.rows().contains(&row).then_some((row, shown))
    }

    /// 鼠标按在菜单上：点到候选就接受它，点在计数和分组那几行上什么也不做。返回是不是点在
    /// 菜单上，是的话这一下不再用来选择或者上报给程序。
    pub(super) fn completion_click(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) -> bool {
        let Some((row, shown)) = self.completion_row(event.position) else {
            return false;
        };
        if let Some(item) = shown.item_at(row) {
            self.accept_completion_item(item, cx);
        }
        cx.notify();
        true
    }

    /// 菜单上次画在网格里的位置。
    pub(super) fn completion_shown(&self) -> Option<paint::Shown> {
        self.completion.as_ref()?.shown.clone()
    }

    /// 接受 `items` 里的第 `item` 项，关掉菜单。
    fn accept_completion_item(&mut self, item: usize, cx: &mut Context<Self>) {
        if let Some(menu) = self.completion.take()
            && let Some(&index) = menu.items.get(item)
        {
            self.accept_candidate(menu.candidates[index].clone(), menu.before_word, cx);
        }
    }

    /// 报给辅助工具的补全菜单：菜单画在网格上，没有对应的元素，这里另放一个不画东西的列表，按上次
    /// 画的位置盖在那几行上，看得到的每项一个选项。选中的那项是活动子项，焦点留在终端上时辅助工具
    /// 也跟着读它；按下选项和点它一样接受。
    pub(super) fn render_completion_a11y(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.completion.as_ref()?;
        let shown = menu.shown.as_ref()?;
        let metrics = self.metrics?;
        let cols = self.screen.live()?.size().cols;
        let (cw, ch) = (metrics.cell.width, metrics.cell.height);
        let rows = shown.rows();
        let (list_row, visible) = shown.visible();
        let view = cx.entity().downgrade();
        let options = visible.map(|item| {
            let candidate = &menu.candidates[menu.items[item]];
            let view = view.clone();
            div()
                .id(("completion-item", item))
                .role(Role::ListBoxOption)
                .aria_label(candidate.label.clone())
                .when_some(candidate.description.clone(), |option, description| option.aria_description(description))
                .aria_selected(item == menu.selected)
                .when(item == menu.selected, |option| option.aria_active_descendant())
                .h(ch)
                .on_a11y_action(AccessibleAction::Click, move |_, _, cx| {
                    view.update(cx, |view, cx| {
                        view.accept_completion_item(item, cx);
                        cx.notify();
                    })
                    .ok();
                })
        });
        let status = if menu.loading() {
            Some(rust_i18n::t!("completion.loading"))
        } else if menu.items.is_empty() {
            Some(rust_i18n::t!("completion.no_matches"))
        } else {
            None
        };
        let list = div()
            .id("completion-menu")
            .role(Role::ListBox)
            .aria_label(rust_i18n::t!("completion.menu").into_owned())
            .when_some(status, |list, status| list.aria_description(status.into_owned()))
            .w(cw * f32::from(cols))
            .h(ch * rows.len() as f32)
            // 上面计数和分组那几行不是选项。
            .pt(ch * (list_row - rows.start) as f32)
            .flex()
            .flex_col()
            .children(options);
        let origin = self.grid_origin + point(px(0.), ch * rows.start as f32);
        Some(anchored().position(origin).child(list).into_any_element())
    }

    /// 在菜单上滚动滚轮：上下移动选中项。返回是不是滚在菜单上。
    pub(super) fn completion_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) -> bool {
        if self.completion_row(event.position).is_none() {
            return false;
        }
        let (Some(metrics), Some(menu)) = (self.metrics, &mut self.completion) else {
            return false;
        };
        // 滚轮往下（内容往上走）时选中下面的项。
        let whole = take_whole_lines(&mut menu.scroll_remainder, wheel_lines(event.delta, metrics.cell.height));
        if whole != 0 {
            menu.select(whole);
        }
        cx.notify();
        true
    }

    /// 菜单现在该不该显示；不该时（视口离开了底部、有了选区、关掉了这项配置）直接关掉。
    pub(super) fn check_completion(&mut self) {
        if self.completion.is_some()
            && (!self.config.command_completions
                || !self.screen.live().is_some_and(|session| !session.has_selection() && session.viewport_at_bottom()))
        {
            self.completion = None;
        }
    }
}
