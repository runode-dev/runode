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
use runode_shared_types::{
    agent::{Agent, AgentState},
    input::{Key, Mods, parse_keys},
    session::SessionMeta,
};

use crate::{
    Env,
    args::{self, Command, Text, Until},
    client::{Connection, kind},
    select::{Place, Selector, World, place_name},
};

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
/// `list` 显示的标识的长度，够区分几十个会话，命令里也能直接用。
pub(crate) const SHORT_ID: usize = 8;

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
        Command::Help => out.write_all(args::HELP.as_bytes())?,
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
            let (channel, meta) = attach(&connection, id)?;
            let text = match text {
                Text::Given(text) => text,
                Text::Stdin => stdin_text()?,
            };
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
            if wait {
                let (until, waiting) = send_wait(&meta, enter || presses_enter(&keys));
                writeln!(err, "runode: waiting {waiting}")?;
                wait_until(&connection, id, &meta, &until, timeout, out)?;
            }
        }
        Command::Wait { session, until, timeout } => {
            let connection = Connection::open(env)?;
            let info = resolve(&connection, env, &session)?;
            if info.exited {
                return Err(Failure::Exited);
            }
            let (_, meta) = attach(&connection, info.id)?;
            wait_until(&connection, info.id, &meta, &until, timeout, out)?;
        }
        Command::Open { placement, near, cwd, focus, command } => {
            let connection = Connection::open(env)?;
            let near = match near.or_else(|| env.session.as_ref().map(|_| Selector::Own)) {
                Some(near) => Some(resolve(&connection, env, &near)?.id),
                None => None,
            };
            let cwd = cwd.map(|dir| std::env::current_dir().map(|here| here.join(dir))).transpose()?;
            let req = connection.req();
            connection.send(&ClientMsg::Open { req, placement, near, cwd, focus })?;
            let id = match connection.reply()? {
                HostMsg::Opened { id, .. } => id,
                other => return Err(unexpected(&other)),
            };
            if !command.is_empty() {
                let (channel, meta) = attach(&connection, id)?;
                wait_for_prompt(&connection, id, meta)?;
                connection.input(channel, command.as_bytes())?;
                thread::sleep(ENTER_DELAY);
                connection.input(channel, b"\r")?;
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
        Command::Setup { target, print } => {
            if print {
                out.write_all(crate::setup::text(target).as_bytes())?;
            } else {
                let home = env.home.as_deref().ok_or_else(|| anyhow!("cannot tell where your home directory is"))?;
                let path = crate::setup(target, home)?;
                writeln!(out, "installed {}", path.display())?;
            }
        }
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
    let mut widths = vec![0; header.len()];
    for row in std::iter::once(&header).chain(&rows) {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
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
            line.extend(std::iter::repeat_n(' ', width - cell.chars().count()));
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

/// 要按的键里有没有不带修饰键的回车。
fn presses_enter(keys: &[String]) -> bool {
    keys.iter()
        .filter_map(|key| parse_keys(key).ok())
        .flatten()
        .any(|chord| chord.key == Key::Enter && chord.mods == Mods::default())
}

/// 等到 `until`，到了就打印结果。`meta` 是连上时的状态。
fn wait_until(
    connection: &Connection,
    id: SessionId,
    meta: &SessionMeta,
    until: &Until,
    timeout: Option<Duration>,
    out: &mut dyn Write,
) -> Result<(), Failure> {
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    if connection.exited(id) {
        return Err(Failure::Exited);
    }
    match until {
        Until::Command => wait_for_command(connection, id, meta, deadline, out),
        Until::Text { pattern, lines, new } => {
            let regex = regex::Regex::new(pattern)?;
            // 上一次读到的、对上的行各有几行：`new` 时比它多出来的才算新出现的。
            let mut before: Option<HashMap<String, usize>> = None;
            loop {
                let text = screen(connection, id, *lines)?;
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
                pause(connection, id, deadline)?;
            }
        }
        Until::Quiet(quiet) => {
            let mut last = screen(connection, id, None)?;
            let mut since = Instant::now();
            loop {
                if since.elapsed() >= *quiet {
                    writeln!(out, "quiet")?;
                    return Ok(());
                }
                pause(connection, id, deadline)?;
                let text = screen(connection, id, None)?;
                if text != last {
                    last = text;
                    since = Instant::now();
                }
            }
        }
        agent => wait_for_agent(connection, id, meta.agent, agent, deadline, out),
    }
}

/// 读一次屏幕，丢掉期间跳过的事件（会话结束了的话 `Connection::exited` 记着）。
fn screen(connection: &Connection, id: SessionId, lines: Option<u32>) -> Result<String, Failure> {
    connection.send(&ClientMsg::ReadScreen { id, lines, command: None })?;
    let reply = connection.reply()?;
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
        if matches!(message, HostMsg::Exited { id: exited, .. } if exited == id) {
            return Err(Failure::Exited);
        }
    }
    Ok(())
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

/// 等 agent 到 `until` 说的状态，到了就打印它现在的状态。`agent` 是连上时的样子。
fn wait_for_agent(
    connection: &Connection,
    id: SessionId,
    mut agent: Option<Agent>,
    until: &Until,
    deadline: Option<Instant>,
    out: &mut dyn Write,
) -> Result<(), Failure> {
    let mut worked = false;
    loop {
        let state = agent.map(|agent| agent.state);
        worked |= state == Some(AgentState::Working);
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
        match connection.next(deadline)? {
            None => return Err(Failure::Timeout),
            Some(HostMsg::Meta { id: changed, meta }) if changed == id => agent = meta.agent,
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
/// `PROMPT_TIMEOUT`。
fn wait_for_prompt(connection: &Connection, id: SessionId, meta: SessionMeta) -> Result<(), Failure> {
    let deadline = Instant::now() + PROMPT_TIMEOUT;
    let mut ready = meta.prompt_cwd.is_some();
    while !ready {
        match connection.next(Some(deadline))? {
            None => break,
            Some(HostMsg::Meta { id: changed, meta }) if changed == id => ready = meta.prompt_cwd.is_some(),
            Some(HostMsg::Exited { id: exited, .. }) if exited == id => return Err(Failure::Exited),
            Some(_) => {}
        }
    }
    Ok(())
}

/// 按写法找正好一个会话，见 `select`。
fn resolve(connection: &Connection, env: &Env, selector: &Selector) -> anyhow::Result<SessionInfo> {
    let world = World::load(connection, env, selector.needs_layout())?;
    world.find(selector).cloned()
}

/// 只看状态地连上会话，返回通道和连上时的状态。通道也能发输入。
fn attach(connection: &Connection, id: SessionId) -> anyhow::Result<(u32, SessionMeta)> {
    connection.send(&ClientMsg::Attach { id, size: None, mode: AttachMode::MetaOnly })?;
    match connection.reply()? {
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

/// 家目录下的路径写成 `~/…`。
pub(crate) fn tilde(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}
