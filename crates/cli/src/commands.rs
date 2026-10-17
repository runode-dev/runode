//! 各个命令：连上宿主，按参数发请求，把结果写到标准输出。

use std::{
    collections::HashMap,
    io::{Read as _, Write},
    path::Path,
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail};
use runode_protocol::{AttachMode, ClientMsg, HostMsg, SessionId, SessionInfo};
use runode_types::{
    agent::{Agent, AgentState},
    input::{Key, Mods, parse_keys},
    session::SessionMeta,
};

use crate::{
    Env,
    args::{Command, Text, Until},
    client::{Connection, is_upgrading, kind},
    help,
    select::{Place, Selector, World, place_name},
};
use unicode_width::UnicodeWidthStr as _;

/// `send --enter` 打完字等这么久再按回车。很多 agent 的界面把同一次读到的文字和回车当成一次
/// 粘贴，回车只换行不提交。发控制键之前也等这么久，道理一样。
const ENTER_DELAY: Duration = Duration::from_millis(100);
/// `open` 给了命令时，等新 shell 显示提示符的最长时间。
const PROMPT_TIMEOUT: Duration = Duration::from_secs(3);
/// `kill` 等会话结束的最长时间。
const KILL_TIMEOUT: Duration = Duration::from_secs(5);
/// `wait --for text`、`--for quiet` 隔这么久读一次屏幕。
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// `send --wait` 既没有 agent、也等不了命令时，屏幕这么久不变就算完。
const SEND_QUIET: Duration = Duration::from_secs(2);
/// `send --wait` 发给 agent 以后，这么久还没看到它干活或提问就不再等：回车可能没提交上，
/// 一直等到 `--timeout` 只会白等。
const SEND_STALL: Duration = Duration::from_secs(10);
/// 宿主升级（`client::Upgrading`）时，`wait` 最多花这么久重新连上新宿主，见 `Watch::recover`。
const RECONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 重连之间隔这么久。
const RECONNECT_INTERVAL: Duration = Duration::from_millis(100);
/// `list` 显示的标识的长度，够区分几十个会话，命令里也能直接用。
pub(crate) const SHORT_ID: usize = 8;

/// `send` 正在发输入（`Paste`、`SendKeys` 这类请求还没回话）时宿主升级了：宿主冻结期间收下的
/// 请求会交给新宿主或者在回滚后照常办，输入可能已经送到。
const UPGRADED_WHILE_SENDING: &str = "the runode host was upgraded while the input was being sent; it may have \
     been delivered, so check with `runode read` before sending it again";
/// `send --wait` 发完输入、在等结果时宿主升级了，而且没能接着等（比如等的是命令运行完）。
const UPGRADED_WHILE_WAITING: &str = "the input was sent, but the runode host was upgraded while waiting for the \
     result; check with `runode read` or `runode wait` instead of sending it again";

/// 命令没做成的原因，各自对应一个退出码。
pub(crate) enum Failure {
    Error(anyhow::Error),
    Timeout,
    Exited,
    /// 等的命令运行失败了，退出码已经打印出来。
    CommandFailed,
}

impl<E: Into<anyhow::Error>> From<E> for Failure {
    fn from(err: E) -> Self {
        Self::Error(err.into())
    }
}

pub(crate) fn run(command: Command, env: &Env, out: &mut dyn Write, err: &mut dyn Write) -> Result<(), Failure> {
    match command {
        Command::Help(topic) => out.write_all(help::text(topic).as_bytes())?,
        Command::Version => writeln!(out, "runode {}", env.build)?,
        Command::List { json } => {
            let connection = Connection::open(env)?;
            list(&World::load(&connection, env, true)?, json, out)?;
        }
        Command::Read { session, lines, command } => {
            let session = own_or(session, env, "read")?;
            let connection = Connection::open(env)?;
            let id = resolve(&connection, env, &session)?.id;
            connection.send(&ClientMsg::ReadScreen { id, lines, command })?;
            match connection.reply()? {
                HostMsg::ScreenText { text, truncated, .. } => {
                    if truncated {
                        writeln!(err, "runode: the start of that output is gone from the scrollback")?;
                    }
                    out.write_all(text.as_bytes())?;
                }
                other => return Err(unexpected(&other)),
            }
        }
        Command::Send { session, text, paste, keys, enter, wait, timeout } => {
            let connection = Connection::open(env)?;
            let info = resolve(&connection, env, &session)?;
            if info.exited {
                return Err(Failure::Exited);
            }
            let id = info.id;
            // 先连上再发，之后 agent 的状态变化和命令结束才不会漏掉。
            let (channel, mut meta) = attach(&connection, id)?;
            // 刚开的终端还没出提示符时认不出 shell 集成，只能等屏幕安静，命令失败也照样算成功。
            // 不按回车时用不着认 shell 集成，不为它白等。
            if wait && (enter || presses_enter(&keys)) && meta.foreground_is_shell {
                meta = wait_for_prompt(&connection, id, meta)?;
            }
            let text = match text {
                Text::Given(text) => text,
                Text::Stdin => stdin_text()?,
            };
            let waiting = wait.then(|| send_wait(&meta, enter || presses_enter(&keys)));
            // 打进去的换行 shell 当回车执行，等命令只等得到第一条运行完；数条数又对不上续行和
            // heredoc，所以什么都没打之前就拒绝。
            if !paste && matches!(waiting, Some((Until::Command, _))) && text.contains(['\n', '\r']) {
                return Err(anyhow!(
                    "typed text with line breaks runs as several commands, and --wait would stop after the first; \
                     use --paste or join the commands with &&"
                )
                .into());
            }
            // 发送前 agent 就在提问时记下屏幕，停滞检测到点时比一比，见 `Stall::asking`。
            let asking = match (&waiting, meta.agent) {
                (Some(_), Some(agent)) if agent.state == AgentState::Blocked => {
                    Some(screen(&connection, id, None, None)?)
                }
                _ => None,
            };
            // 从这里起宿主升级打断时不叫用户重跑：输入可能已经送到了，见 `after_sending`。
            let delivered = (|| -> Result<(), Failure> {
                let mut sent = false;
                if !text.is_empty() {
                    if paste {
                        let req = connection.req();
                        connection.request_done(&ClientMsg::Paste { req, id, text })?;
                    } else {
                        connection.input(channel, text.as_bytes())?;
                    }
                    sent = true;
                }
                if !keys.is_empty() {
                    if sent {
                        thread::sleep(ENTER_DELAY);
                    }
                    let req = connection.req();
                    connection.request_done(&ClientMsg::SendKeys { req, id, keys: keys.clone() })?;
                    sent = true;
                }
                if enter {
                    if sent {
                        thread::sleep(ENTER_DELAY);
                    }
                    connection.input(channel, b"\r")?;
                }
                Ok(())
            })();
            delivered.map_err(|failure| after_sending(failure, UPGRADED_WHILE_SENDING))?;
            if let Some((until, waiting)) = waiting {
                writeln!(err, "runode: waiting {waiting}")?;
                let stall = Stall { after: SEND_STALL, asking };
                wait_until(&mut Watch { env, connection, id }, &meta, &until, timeout, Some(&stall), out)
                    .map_err(|failure| after_sending(failure, UPGRADED_WHILE_WAITING))?;
            }
        }
        Command::Wait { session, until, timeout } => {
            let connection = Connection::open(env)?;
            let info = resolve(&connection, env, &session)?;
            if info.exited {
                return Err(Failure::Exited);
            }
            let (_, meta) = attach(&connection, info.id)?;
            wait_until(&mut Watch { env, connection, id: info.id }, &meta, &until, timeout, None, out)?;
        }
        Command::Open { placement, near, cwd, focus, fold, command } => {
            let connection = Connection::open(env)?;
            let near = match near {
                Some(near) => Some(resolve(&connection, env, &near)?.id),
                // 自己的会话已经不在了（比如 tmux 里留着旧终端的 RUNODE_SESSION），照在 runode 外面办，
                // 交给宿主开在最前面的窗口。
                None if env.session.is_some() => {
                    let world = World::load(&connection, env, false)?;
                    world.own.filter(|own| world.sessions.iter().any(|info| info.id == *own))
                }
                None => None,
            };
            let cwd = cwd.map(|dir| std::env::current_dir().map(|here| here.join(dir))).transpose()?;
            let from = env.session.as_deref().and_then(|id| id.parse().ok());
            let req = connection.req();
            connection.send(&ClientMsg::Open { req, placement, near, cwd, focus, fold, from })?;
            let id = match connection.reply()? {
                HostMsg::Opened { id, .. } => id,
                other => return Err(unexpected(&other)),
            };
            if !command.is_empty() {
                // 终端已经开了：这时宿主升级，叫人重跑会再开一个终端，所以说清开好的是哪个。
                let typed = (|| -> Result<(), Failure> {
                    let (channel, meta) = attach(&connection, id)?;
                    wait_for_prompt(&connection, id, meta)?;
                    connection.input(channel, command.as_bytes())?;
                    thread::sleep(ENTER_DELAY);
                    connection.input(channel, b"\r")?;
                    Ok(())
                })();
                if let Err(Failure::Error(err)) = &typed
                    && is_upgrading(err)
                {
                    return Err(anyhow!(
                        "opened session {id}, but the runode host was upgraded before the command was typed; \
                         check it with `runode read {id}` instead of opening another"
                    )
                    .into());
                }
                typed?;
            }
            writeln!(out, "{id}")?;
        }
        Command::Kill { session } => {
            let connection = Connection::open(env)?;
            let id = resolve(&connection, env, &session)?.id;
            // 先连上，才收得到结束时的 `Exited`，确认真的结束了再返回。
            attach(&connection, id)?;
            connection.send(&ClientMsg::Kill { id })?;
            let deadline = Instant::now() + KILL_TIMEOUT;
            loop {
                match connection.next(Some(deadline))? {
                    Some(HostMsg::Exited { id: exited, .. }) if exited == id => break,
                    Some(_) => {}
                    None => return Err(anyhow!("session {id} did not end").into()),
                }
            }
        }
        Command::Focus { session } => {
            let session = own_or(session, env, "focus")?;
            let connection = Connection::open(env)?;
            let id = resolve(&connection, env, &session)?.id;
            let req = connection.req();
            connection.request_done(&ClientMsg::Reveal { req, id })?;
        }
        Command::RemotePair { addrs } => {
            crate::remote::pair(env, &addrs, out, &mut crate::remote::read_answer, crate::remote::LISTENER_WAIT)?
        }
        Command::RemoteDevices { json } => crate::remote::devices(env, json, out)?,
        Command::RemoteRevoke { device } => crate::remote::revoke(env, &device, out)?,
        Command::ServiceInstall { kind } => crate::service::install(env, kind, out)?,
        Command::ServiceUninstall { kind } => crate::service::uninstall(env, kind, out)?,
        Command::ServiceStatus => crate::service::status(env, out)?,
        Command::TodoAdd { title, prompt, cwd, agent, worktree, start } => {
            crate::todo::add(env, crate::todo::Spec { title, prompt, cwd, agent, worktree }, start, out)?
        }
        Command::TodoList { json } => crate::todo::list(env, json, out)?,
        Command::TodoShow { id, json } => crate::todo::show(env, &id, json, out)?,
        Command::TodoStart { id } => crate::todo::start(env, &id, out)?,
        Command::TodoDone { id } => crate::todo::done(env, id)?,
        Command::TodoRemove { id } => crate::todo::remove(env, id)?,
        Command::ScheduleAdd { title, prompt, cwd, agent, worktree, timing } => {
            crate::todo::add_schedule(env, crate::todo::Spec { title, prompt, cwd, agent, worktree }, timing, out)?
        }
        Command::ScheduleList { json } => crate::todo::list_schedules(env, json, out)?,
        Command::ScheduleRemove { id } => crate::todo::remove_schedule(env, id)?,
        Command::Completions { shell } => out.write_all(crate::complete::script(shell).as_bytes())?,
    }
    Ok(())
}

/// 给了会话就用它，没给时在 runode 的终端里用自己所在的。
fn own_or(session: Option<Selector>, env: &Env, command: &str) -> anyhow::Result<Selector> {
    match session {
        Some(session) => Ok(session),
        None if env.session.is_some() => Ok(Selector::Own),
        None => Err(anyhow!("{command} needs a SESSION outside a runode terminal")),
    }
}

fn list(world: &World, json: bool, out: &mut dyn Write) -> anyhow::Result<()> {
    let relations = world.relations();
    let rel = |id: SessionId| -> Vec<&str> {
        relations.iter().filter(|(related, _)| *related == id).map(|(_, name)| *name).collect()
    };
    let current = |info: &SessionInfo| world.own == Some(info.id);
    // 在窗口里的按窗口、工作区、标签、分屏排在前面，后台会话按标识排在后面。
    let mut sessions: Vec<(&SessionInfo, Option<Place<'_>>)> =
        world.sessions.iter().map(|info| (info, world.place(info.id))).collect();
    sessions.sort_by_key(|(info, place)| {
        (place.is_none(), place.map(|p| (p.window.index, p.workspace.index, p.tab.index, p.pane.index)), info.id)
    });
    let view = |place: Option<Place<'_>>| match place {
        Some(place) if place.shown() => "shown",
        Some(_) => "hidden",
        None => "bg",
    };
    if json {
        let sessions: Vec<_> = sessions
            .iter()
            .map(|(info, place)| {
                serde_json::json!({
                    "id": info.id,
                    "current": current(info),
                    "title": title(&info.meta),
                    "agent": info.meta.agent.map(|agent| agent.kind.label()),
                    "state": info.meta.agent.map(|agent| state_name(agent.state)),
                    "foreground": info.meta.foreground,
                    "cwd": info.meta.cwd,
                    "size": { "cols": info.size.cols, "rows": info.size.rows },
                    "clients": info.clients,
                    "claimed": info.claimed,
                    "exited": info.exited,
                    "driver": info.meta.driver,
                    "place": place.map(|place| serde_json::json!({
                        "window": place.window.index,
                        "workspace": place.workspace.index,
                        "tab": place.tab.index,
                        "pane": place.pane.index,
                        "focused": place.pane.focused,
                        "selector": place_name(&place),
                    })),
                    "rel": rel(info.id),
                    "view": view(*place),
                })
            })
            .collect();
        let own = world.own.map(|own| own.to_string());
        let listing = serde_json::json!({ "self": own, "layout": world.layout, "sessions": sessions });
        serde_json::to_writer_pretty(&mut *out, &listing)?;
        writeln!(out)?;
        return Ok(());
    }
    let positions = world.layout.as_ref().is_some_and(|windows| !windows.is_empty());
    let mut header = vec![" ", "ID"];
    if positions {
        header.extend(["WIN", "WS", "TAB", "PANE", "REL"]);
    }
    header.extend(["AGENT", "STATE", "FG", "TITLE", "DIR", "VIEW"]);
    let header: Vec<String> = header.into_iter().map(String::from).collect();
    let dash = |value: Option<String>| value.filter(|value| !value.is_empty()).unwrap_or_else(|| "-".into());
    let rows: Vec<Vec<String>> = sessions
        .iter()
        .map(|(info, place)| {
            let mut row = vec![if current(info) { "*" } else { " " }.into(), info.id.to_string()[..SHORT_ID].into()];
            if positions {
                let number = |n: Option<u32>| dash(n.map(|n| n.to_string()));
                row.extend([
                    number(place.map(|p| p.window.index)),
                    number(place.map(|p| p.workspace.index)),
                    number(place.map(|p| p.tab.index)),
                    number(place.map(|p| p.pane.index)),
                    dash(Some(rel(info.id).join(","))),
                ]);
            }
            row.extend([
                info.meta.agent.map_or("-", |agent| agent.kind.display_name()).into(),
                if info.exited { "exited" } else { info.meta.agent.map_or("-", |agent| state_name(agent.state)) }
                    .into(),
                dash(info.meta.foreground.clone()),
                title(&info.meta).unwrap_or_default(),
                info.meta.cwd.as_deref().map(|cwd| tilde(cwd, world.home.as_deref())).unwrap_or_default(),
                view(*place).into(),
            ]);
            row
        })
        .collect();
    let rows: Vec<Vec<String>> = rows.iter().map(|row| row.iter().map(|cell| plain(cell)).collect()).collect();
    let mut widths = vec![0; header.len()];
    for row in std::iter::once(&header).chain(&rows) {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.width());
        }
    }
    for row in std::iter::once(&header).chain(&rows) {
        let mut line = String::new();
        for (i, (cell, width)) in row.iter().zip(&widths).enumerate() {
            if i > 1 {
                line.push_str("  ");
            } else if i == 1 {
                line.push(' ');
            }
            line.push_str(cell);
            line.extend(std::iter::repeat_n(' ', width - cell.width()));
        }
        writeln!(out, "{}", line.trim_end())?;
    }
    Ok(())
}

/// `send --wait` 等什么，以及告诉用户的说法：有 agent 时等它干完；前台是开了 shell 集成的
/// shell、又按了回车时等那条命令；别的（全屏程序、没有集成的 shell）等屏幕安静下来。
fn send_wait(meta: &SessionMeta, enter: bool) -> (Until, &'static str) {
    if meta.agent.is_some() {
        (Until::Done, "for the agent to finish (--for done)")
    } else if enter && meta.foreground_is_shell && meta.prompt_cwd.is_some() {
        (Until::Command, "for the command to finish (--for command)")
    } else {
        (Until::Quiet(SEND_QUIET), "for the screen to stay quiet for 2s (--for quiet 2)")
    }
}

/// `send` 发出输入以后的失败：宿主在升级（`client::Upgrading`）时换成 `message`，不说「重跑」
/// （那会把输入再打一遍，比如再执行一次 `git push`）。退出码仍是失败：结果不知道，不能当成做成了。
fn after_sending(failure: Failure, message: &'static str) -> Failure {
    match failure {
        Failure::Error(err) if is_upgrading(&err) => Failure::Error(anyhow!(message)),
        failure => failure,
    }
}

/// 要按的键里有没有不带修饰键的回车。
fn presses_enter(keys: &[String]) -> bool {
    keys.iter()
        .filter_map(|key| parse_keys(key).ok())
        .flatten()
        .any(|chord| chord.key == Key::Enter && chord.mods == Mods::default())
}

/// `wait`（和 `send --wait`）等着的会话和连着它的连接。宿主把会话交给新版本的宿主时（连接上
/// 来了 `Goodbye { reason: Handoff }`），重新连上新宿主接着等，见 `recover`。
struct Watch<'a> {
    env: &'a Env,
    connection: Connection,
    id: SessionId,
}

impl Watch<'_> {
    /// `failure` 是宿主在升级（`client::Upgrading`）时，重新连上宿主、只看状态地重新连上会话，
    /// 返回这时会话的状态；每隔 `RECONNECT_INTERVAL` 试一次，最多 `RECONNECT_TIMEOUT`，也不超过
    /// `deadline`（超过了是 `Timeout`）。新宿主里没有这个会话了（交接时它已经结束）是 `Exited`。
    /// 别的失败原样返回。
    fn recover(&mut self, failure: Failure, deadline: Option<Instant>) -> Result<SessionMeta, Failure> {
        match &failure {
            Failure::Error(err) if is_upgrading(err) => {}
            _ => return Err(failure),
        }
        let give_up = Instant::now() + RECONNECT_TIMEOUT;
        let give_up = deadline.map_or(give_up, |at| at.min(give_up));
        let connection = loop {
            match Connection::open_by(self.env, give_up) {
                Ok(connection) => break connection,
                Err(err) => {
                    let now = Instant::now();
                    if deadline.is_some_and(|at| now >= at) {
                        return Err(Failure::Timeout);
                    }
                    if now >= give_up {
                        return Err(err.context("the runode host was being upgraded and did not come back").into());
                    }
                }
            }
            thread::sleep(RECONNECT_INTERVAL);
        };
        connection.send(&ClientMsg::ListSessions)?;
        match connection.reply_before(deadline).map_err(|err| expired(err, deadline))? {
            HostMsg::SessionList { sessions } if sessions.iter().any(|info| info.id == self.id && !info.exited) => {}
            HostMsg::SessionList { .. } => return Err(Failure::Exited),
            other => return Err(unexpected(&other)),
        }
        let (_, meta) = attach_before(&connection, self.id, deadline).map_err(|err| expired(err, deadline))?;
        self.connection = connection;
        Ok(meta)
    }

    /// 读一次整屏；宿主在升级时先重新连上新宿主再读，见 `recover`。
    fn read_screen(&mut self, deadline: Option<Instant>) -> Result<String, Failure> {
        loop {
            match screen(&self.connection, self.id, None, deadline) {
                Ok(text) => return Ok(text),
                Err(failure) => self.recover(failure, deadline).map(drop)?,
            }
        }
    }
}

/// 等到 `until`，到了就打印结果。`meta` 是连上时的状态。宿主升级时重新连上接着等（见
/// `Watch::recover`），等命令运行完（`Until::Command`）除外：新宿主不会重报交接时正在跑的命令。
/// 等 agent 时给了 `stall`，一段时间里 agent 既没干活也没提问就失败，见 `wait_for_agent`。
fn wait_until(
    watch: &mut Watch<'_>,
    meta: &SessionMeta,
    until: &Until,
    timeout: Option<Duration>,
    stall: Option<&Stall>,
    out: &mut dyn Write,
) -> Result<(), Failure> {
    // 大到加上去就溢出的超时（`--timeout 1.8e19`）当作不设。
    let deadline = timeout.and_then(|timeout| Instant::now().checked_add(timeout));
    let id = watch.id;
    if watch.connection.exited(id) {
        return Err(Failure::Exited);
    }
    match until {
        Until::Command => wait_for_command(&watch.connection, id, meta, deadline, out),
        Until::Text { pattern, lines, new } => {
            let regex = regex::Regex::new(pattern)?;
            // 上一次读到的、对上的行各有几行：`new` 时比它多出来的才算新出现的。
            let mut before: Option<HashMap<String, usize>> = None;
            loop {
                let text = match screen(&watch.connection, id, *lines, deadline) {
                    Ok(text) => text,
                    Err(failure) => {
                        watch.recover(failure, deadline)?;
                        continue;
                    }
                };
                let matched: Vec<&str> = text.lines().filter(|line| regex.is_match(line)).collect();
                let hit = match (&before, new) {
                    (_, false) => matched.first().copied(),
                    (None, true) => None,
                    (Some(before), true) => {
                        let mut seen: HashMap<&str, usize> = HashMap::new();
                        matched.iter().copied().find(|line| {
                            let count = seen.entry(line).or_default();
                            *count += 1;
                            *count > before.get(*line).copied().unwrap_or(0)
                        })
                    }
                };
                if let Some(line) = hit {
                    writeln!(out, "{line}")?;
                    return Ok(());
                }
                let mut counts = HashMap::new();
                for line in matched {
                    *counts.entry(line.to_owned()).or_default() += 1;
                }
                before = Some(counts);
                if let Err(failure) = pause(&watch.connection, id, deadline) {
                    watch.recover(failure, deadline)?;
                }
            }
        }
        Until::Quiet(quiet) => {
            let mut last = watch.read_screen(deadline)?;
            let mut since = Instant::now();
            loop {
                if since.elapsed() >= *quiet {
                    writeln!(out, "quiet")?;
                    return Ok(());
                }
                let text = match pause(&watch.connection, id, deadline)
                    .and_then(|()| screen(&watch.connection, id, None, deadline))
                {
                    Ok(text) => text,
                    Err(failure) => {
                        watch.recover(failure, deadline)?;
                        // 交接本身要花时间，期间屏幕也可能变了：在新宿主上重新读一次、重新计时，
                        // 不把交接的那几秒算成安静。
                        last = watch.read_screen(deadline)?;
                        since = Instant::now();
                        continue;
                    }
                };
                if text != last {
                    last = text;
                    since = Instant::now();
                }
            }
        }
        agent => wait_for_agent(watch, meta.agent, agent, deadline, stall, out),
    }
}

/// 读一次屏幕，丢掉期间跳过的事件（会话结束了的话 `Connection::exited` 记着）。等回话不过
/// `deadline`，过了是 `Timeout`。
fn screen(
    connection: &Connection,
    id: SessionId,
    lines: Option<u32>,
    deadline: Option<Instant>,
) -> Result<String, Failure> {
    connection.send(&ClientMsg::ReadScreen { id, lines, command: None })?;
    let reply = connection.reply_before(deadline).map_err(|err| expired(err, deadline))?;
    // 宿主摘掉了订阅（这边读得太慢）：之后会话结束了也收不到 `Exited`，读下去只会等到超时。
    if connection.lost(id) && !connection.exited(id) {
        return Err(anyhow!("lost track of the session").into());
    }
    connection.drop_pending();
    match reply {
        HostMsg::ScreenText { text, .. } => Ok(text),
        other => Err(unexpected(&other)),
    }
}

/// 等到下一次读屏幕的时候；先到了 `deadline` 就超时，会话结束了就失败。
fn pause(connection: &Connection, id: SessionId, deadline: Option<Instant>) -> Result<(), Failure> {
    if connection.exited(id) {
        return Err(Failure::Exited);
    }
    let now = Instant::now();
    if deadline.is_some_and(|at| now >= at) {
        return Err(Failure::Timeout);
    }
    let wake = deadline.map_or(now + POLL_INTERVAL, |at| at.min(now + POLL_INTERVAL));
    // 期间来的事件也看一眼：会话结束了不必等到下次读屏幕。
    while let Some(message) = connection.next(Some(wake))? {
        match message {
            HostMsg::Exited { id: exited, .. } if exited == id => return Err(Failure::Exited),
            HostMsg::Resync { id: lost, .. } if lost == id => return Err(anyhow!("lost track of the session").into()),
            _ => {}
        }
    }
    Ok(())
}

/// 等回话失败了：已经过了 `deadline` 时是超时，不然是 `err`。
fn expired(err: anyhow::Error, deadline: Option<Instant>) -> Failure {
    if deadline.is_some_and(|at| Instant::now() >= at) { Failure::Timeout } else { err.into() }
}

/// 等 shell 集成报告下一条命令运行完，打印 `exit N`；N 不是 0 时失败。
fn wait_for_command(
    connection: &Connection,
    id: SessionId,
    meta: &SessionMeta,
    deadline: Option<Instant>,
    out: &mut dyn Write,
) -> Result<(), Failure> {
    if meta.prompt_cwd.is_none() {
        return Err(anyhow!(
            "the session has no shell integration to report when commands finish; wait with --for quiet SECS instead"
        )
        .into());
    }
    loop {
        match connection.next(deadline)? {
            None => return Err(Failure::Timeout),
            Some(HostMsg::CommandFinished { id: finished, command }) if finished == id => {
                return match command.exit {
                    Some(0) => {
                        writeln!(out, "exit 0")?;
                        Ok(())
                    }
                    Some(code) => {
                        writeln!(out, "exit {code}")?;
                        Err(Failure::CommandFailed)
                    }
                    None => {
                        writeln!(out, "exit unknown")?;
                        Ok(())
                    }
                };
            }
            Some(HostMsg::Exited { id: exited, .. }) if exited == id => return Err(Failure::Exited),
            Some(HostMsg::Resync { id: lost, .. }) if lost == id => {
                return Err(anyhow!("lost track of the session").into());
            }
            Some(_) => {}
        }
    }
}

/// `send --wait` 的停滞检测：发出输入后 `after` 这么久里 agent 既没干活也没新进入提问，就当输入
/// 没提交上，报错不再等。
struct Stall {
    after: Duration,
    /// 发送前 agent 在提问时的整屏。答完一个提问紧接着问下一个、中间的干活短到没认出来时，状态一直是
    /// Blocked，宿主不发状态，看不出动静；到点时屏幕和它不一样就当成换了提问，照停在提问上算。
    /// 打进去的文字没提交上、回显在屏幕上时也会这样算，那也只是停在 `blocked`，调用方照样要读屏幕。
    asking: Option<String>,
}

/// 等 agent 到 `until` 说的状态，到了就打印它现在的状态。`agent` 是连上时的样子。给了 `stall`
/// 时，这么久里 agent 一直没干活也没提问就失败，不等到 `deadline`；连上时就在提问的不算。
fn wait_for_agent(
    watch: &mut Watch<'_>,
    mut agent: Option<Agent>,
    until: &Until,
    deadline: Option<Instant>,
    stall: Option<&Stall>,
    out: &mut dyn Write,
) -> Result<(), Failure> {
    let mut worked = false;
    let mut stall_at = stall.map(|stall| Instant::now() + stall.after);
    let mut last = agent.map(|agent| agent.state);
    loop {
        let state = agent.map(|agent| agent.state);
        worked |= state == Some(AgentState::Working);
        // 只有新进入的提问才算动静：发送前就在提问的，按 esc 或答「不」后可能不干活直接回到空闲，
        // 不能因此把停滞检测关掉。
        if state == Some(AgentState::Working)
            || (state == Some(AgentState::Blocked) && last != Some(AgentState::Blocked))
        {
            stall_at = None;
        }
        last = state;
        let reached = match until {
            Until::Done => worked && state != Some(AgentState::Working),
            Until::Working => state == Some(AgentState::Working),
            Until::Idle => state == Some(AgentState::Idle),
            Until::Blocked => state == Some(AgentState::Blocked),
            _ => state != Some(AgentState::Working),
        };
        if reached {
            writeln!(out, "{}", state.map_or("no agent", state_name))?;
            return Ok(());
        }
        let id = watch.id;
        let stalls = stall_at.filter(|at| deadline.is_none_or(|deadline| *at < deadline));
        let next = match watch.connection.next(stalls.or(deadline)) {
            Ok(next) => next,
            Err(err) => {
                agent = watch.recover(err.into(), deadline)?.agent;
                continue;
            }
        };
        match next {
            None if stalls.is_some() => {
                let asking = stall.and_then(|stall| stall.asking.as_ref());
                if let Some(before) = asking
                    && state == Some(AgentState::Blocked)
                    && watch.read_screen(deadline)? != *before
                {
                    writeln!(out, "{}", state_name(AgentState::Blocked))?;
                    return Ok(());
                }
                return Err(anyhow!(
                    "the agent showed no activity within {}s of the input: it may not have been submitted, or it \
                     finished without working; check with `runode read` before sending it again",
                    stall.map_or(0, |stall| stall.after.as_secs())
                )
                .into());
            }
            None => return Err(Failure::Timeout),
            Some(HostMsg::Meta { id: changed, meta, .. }) if changed == id => agent = meta.agent,
            Some(HostMsg::Exited { id: exited, .. }) if exited == id => return Err(Failure::Exited),
            Some(HostMsg::Resync { id: lost, .. }) if lost == id => {
                return Err(anyhow!("lost track of the session").into());
            }
            Some(_) => {}
        }
    }
}

/// 等新开的 shell 第一次显示提示符（shell 集成报告了 `SessionMeta::prompt_cwd`）再打字，免得
/// 打的字在 shell 准备好之前就被终端原样回显出来。没开 shell 集成时等不到，最多等
/// `PROMPT_TIMEOUT`。返回最后收到的状态。
fn wait_for_prompt(connection: &Connection, id: SessionId, mut meta: SessionMeta) -> Result<SessionMeta, Failure> {
    let deadline = Instant::now() + PROMPT_TIMEOUT;
    while meta.prompt_cwd.is_none() {
        match connection.next(Some(deadline))? {
            None => break,
            Some(HostMsg::Meta { id: changed, meta: changed_meta, .. }) if changed == id => meta = changed_meta,
            Some(HostMsg::Exited { id: exited, .. }) if exited == id => return Err(Failure::Exited),
            // 宿主摘掉了订阅（这边读得太慢）：之后的状态都收不到了，等下去只会等到超时。
            Some(HostMsg::Resync { id: lost, .. }) if lost == id => {
                return Err(anyhow!("lost track of the session").into());
            }
            Some(_) => {}
        }
    }
    Ok(meta)
}

/// 按写法找正好一个会话，见 `select`。
fn resolve(connection: &Connection, env: &Env, selector: &Selector) -> anyhow::Result<SessionInfo> {
    let world = World::load(connection, env, selector.needs_layout())?;
    world.find(selector).cloned()
}

/// 只看状态地连上会话，返回通道和连上时的状态。通道也能发输入。
fn attach(connection: &Connection, id: SessionId) -> anyhow::Result<(u32, SessionMeta)> {
    attach_before(connection, id, None)
}

/// 同 `attach`，但给了 `deadline` 时等回话不过它，见 `Connection::reply_before`。
fn attach_before(
    connection: &Connection,
    id: SessionId,
    deadline: Option<Instant>,
) -> anyhow::Result<(u32, SessionMeta)> {
    connection.send(&ClientMsg::Attach { id, size: None, mode: AttachMode::MetaOnly })?;
    match connection.reply_before(deadline)? {
        HostMsg::Attached { channel, meta, .. } => Ok((channel, meta)),
        other => bail!("unexpected answer: {}", kind(&other)),
    }
}

/// 从标准输入读要打的字，去掉末尾的一个换行（`echo` 带的那个）。
fn stdin_text() -> anyhow::Result<String> {
    let mut text = Vec::new();
    std::io::stdin().read_to_end(&mut text)?;
    if text.ends_with(b"\n") {
        text.pop();
    }
    String::from_utf8(text).map_err(|_| anyhow!("standard input is not UTF-8 text"))
}

fn unexpected(message: &HostMsg) -> Failure {
    anyhow!("unexpected answer: {}", kind(message)).into()
}

pub(crate) fn title(meta: &SessionMeta) -> Option<String> {
    meta.title.clone().or_else(|| meta.fallback_title.clone())
}

pub(crate) fn state_name(state: AgentState) -> &'static str {
    match state {
        AgentState::Working => "working",
        AgentState::Idle => "idle",
        AgentState::Blocked => "blocked",
    }
}

/// 要打到终端上的、别的程序给的文字（标题、路径、提示词、设备名）：控制字符换成 `?`、tab 换成空格，
/// 免得里面的转义序列清屏、改终端标题，或者把表格的一行拆成几行。
pub(crate) fn plain(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c == '\t' {
                ' '
            } else if c.is_control() {
                '?'
            } else {
                c
            }
        })
        .collect()
}

/// 家目录下的路径写成 `~/…`。
pub(crate) fn tilde(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}
