//! 各个命令：连上宿主，按参数发请求，把结果写到标准输出。

use std::{
    io::{Read as _, Write},
    path::Path,
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail};
use runode_protocol::{AttachMode, ClientMsg, HostMsg, SessionId, SessionInfo};
use runode_shared_types::{
    agent::{Agent, AgentState},
    session::SessionMeta,
};

use crate::{
    Env,
    args::{self, Command, Text, Until},
    client::{Connection, kind},
};

/// `send --enter` 打完字等这么久再按回车。很多 agent 的界面把同一次读到的文字和回车当成一次
/// 粘贴，回车只换行不提交。
const ENTER_DELAY: Duration = Duration::from_millis(100);
/// `open` 给了命令时，等新 shell 显示提示符的最长时间。
const PROMPT_TIMEOUT: Duration = Duration::from_secs(3);
/// `kill` 等会话结束的最长时间。
const KILL_TIMEOUT: Duration = Duration::from_secs(5);
/// `list` 显示的标识的长度，够区分几十个会话，命令里也能直接用。
const SHORT_ID: usize = 8;

/// 命令没做成的原因，各自对应一个退出码。
pub(crate) enum Failure {
    Error(anyhow::Error),
    Timeout,
    Exited,
}

impl<E: Into<anyhow::Error>> From<E> for Failure {
    fn from(err: E) -> Self {
        Self::Error(err.into())
    }
}

pub(crate) fn run(command: Command, env: &Env, out: &mut dyn Write) -> Result<(), Failure> {
    match command {
        Command::Help => out.write_all(args::HELP.as_bytes())?,
        Command::Version => writeln!(out, "runode {}", env.build)?,
        Command::List { json } => list(&mut Connection::open(env)?, env, json, out)?,
        Command::Read { session, lines } => {
            let session = session
                .or_else(|| env.session.clone())
                .ok_or_else(|| anyhow!("read needs a SESSION outside a runode terminal"))?;
            let mut connection = Connection::open(env)?;
            let id = resolve(&mut connection, &session)?.id;
            connection.send(&ClientMsg::ReadScreen { id, lines, command: None })?;
            match connection.reply()? {
                HostMsg::ScreenText { text, .. } => out.write_all(text.as_bytes())?,
                other => return Err(unexpected(&other)),
            }
        }
        Command::Send { session, text, enter, wait, timeout } => {
            let mut connection = Connection::open(env)?;
            let info = resolve(&mut connection, &session)?;
            if info.exited {
                return Err(Failure::Exited);
            }
            let (channel, meta) = attach(&mut connection, info.id)?;
            let text = match text {
                Text::Given(text) => text.into_bytes(),
                Text::Stdin => stdin_text()?,
            };
            if !text.is_empty() {
                connection.input(channel, &text)?;
            }
            if enter {
                if !text.is_empty() {
                    thread::sleep(ENTER_DELAY);
                }
                connection.input(channel, b"\r")?;
            }
            if wait {
                wait_for(&connection, info.id, meta.agent, Until::Done, timeout, out)?;
            }
        }
        Command::Wait { session, until, timeout } => {
            let mut connection = Connection::open(env)?;
            let info = resolve(&mut connection, &session)?;
            if info.exited {
                return Err(Failure::Exited);
            }
            let (_, meta) = attach(&mut connection, info.id)?;
            wait_for(&connection, info.id, meta.agent, until, timeout, out)?;
        }
        Command::Open { placement, near, cwd, focus, command } => {
            let mut connection = Connection::open(env)?;
            let near = match near.or_else(|| env.session.clone()) {
                Some(near) => Some(resolve(&mut connection, &near)?.id),
                None => None,
            };
            let cwd = cwd.map(|dir| std::env::current_dir().map(|here| here.join(dir))).transpose()?;
            connection.send(&ClientMsg::Open { req: 1, placement, near, cwd, focus })?;
            let id = match connection.reply()? {
                HostMsg::Opened { id, .. } => id,
                other => return Err(unexpected(&other)),
            };
            if !command.is_empty() {
                let (channel, meta) = attach(&mut connection, id)?;
                wait_for_prompt(&connection, id, meta)?;
                connection.input(channel, command.as_bytes())?;
                thread::sleep(ENTER_DELAY);
                connection.input(channel, b"\r")?;
            }
            writeln!(out, "{id}")?;
        }
        Command::Kill { session } => {
            let mut connection = Connection::open(env)?;
            let id = resolve(&mut connection, &session)?.id;
            // 先连上，才收得到结束时的 `Exited`，确认真的结束了再返回。
            attach(&mut connection, id)?;
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
            let session = session
                .or_else(|| env.session.clone())
                .ok_or_else(|| anyhow!("focus needs a SESSION outside a runode terminal"))?;
            let mut connection = Connection::open(env)?;
            let id = resolve(&mut connection, &session)?.id;
            connection.send(&ClientMsg::Reveal { req: 1, id })?;
            match connection.reply()? {
                HostMsg::Done { .. } => {}
                other => return Err(unexpected(&other)),
            }
        }
    }
    Ok(())
}

fn list(connection: &mut Connection, env: &Env, json: bool, out: &mut dyn Write) -> anyhow::Result<()> {
    let sessions = sessions(connection)?;
    let current = |info: &SessionInfo| env.session.as_deref() == Some(&info.id.to_string());
    if json {
        let sessions: Vec<_> = sessions
            .iter()
            .map(|info| {
                serde_json::json!({
                    "id": info.id,
                    "current": current(info),
                    "title": title(&info.meta),
                    "agent": info.meta.agent.map(|agent| agent.kind.label()),
                    "state": info.meta.agent.map(|agent| state_name(agent.state)),
                    "cwd": info.meta.cwd,
                    "clients": info.clients,
                    "exited": info.exited,
                })
            })
            .collect();
        serde_json::to_writer_pretty(&mut *out, &sessions)?;
        writeln!(out)?;
        return Ok(());
    }
    let home = runode_paths::Dirs::from_env().home;
    let rows: Vec<[String; 6]> = sessions
        .iter()
        .map(|info| {
            [
                if current(info) { "*" } else { " " }.into(),
                info.id.to_string()[..SHORT_ID].into(),
                info.meta.agent.map_or("-", |agent| agent.kind.display_name()).into(),
                if info.exited { "exited" } else { info.meta.agent.map_or("-", |agent| state_name(agent.state)) }
                    .into(),
                title(&info.meta).unwrap_or_default(),
                info.meta.cwd.as_deref().map(|cwd| tilde(cwd, home.as_deref())).unwrap_or_default(),
            ]
        })
        .collect();
    let header = [" ", "ID", "AGENT", "STATE", "TITLE", "DIR"].map(String::from);
    let mut widths = [0; 6];
    for row in std::iter::once(&header).chain(&rows) {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    for row in std::iter::once(&header).chain(&rows) {
        let mut line = String::new();
        for (i, (cell, width)) in row.iter().zip(widths).enumerate() {
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

/// 等 agent 到 `until` 说的状态，到了就打印它现在的状态。`agent` 是连上时的样子。
fn wait_for(
    connection: &Connection,
    id: SessionId,
    mut agent: Option<Agent>,
    until: Until,
    timeout: Option<Duration>,
    out: &mut dyn Write,
) -> Result<(), Failure> {
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    let mut worked = false;
    loop {
        let state = agent.map(|agent| agent.state);
        worked |= state == Some(AgentState::Working);
        let reached = match until {
            Until::Stopped => state != Some(AgentState::Working),
            Until::Done => worked && state != Some(AgentState::Working),
            Until::Working => state == Some(AgentState::Working),
            Until::Idle => state == Some(AgentState::Idle),
            Until::Blocked => state == Some(AgentState::Blocked),
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

fn sessions(connection: &mut Connection) -> anyhow::Result<Vec<SessionInfo>> {
    connection.send(&ClientMsg::ListSessions)?;
    match connection.reply()? {
        HostMsg::SessionList { sessions } => Ok(sessions),
        other => bail!("unexpected answer: {}", kind(&other)),
    }
}

/// 按标识或它的前缀找会话，不分大小写；对上的不是正好一个时报错。
fn resolve(connection: &mut Connection, given: &str) -> anyhow::Result<SessionInfo> {
    let prefix = given.to_ascii_lowercase();
    let mut matches = sessions(connection)?.into_iter().filter(|info| info.id.to_string().starts_with(&prefix));
    match (matches.next(), matches.next()) {
        (Some(info), None) if !prefix.is_empty() => Ok(info),
        (Some(_), Some(_)) => bail!("{given} matches more than one session; give more of the id"),
        _ => bail!("no session {given}; see `runode list`"),
    }
}

/// 只看状态地连上会话，返回通道和连上时的状态。通道也能发输入。
fn attach(connection: &mut Connection, id: SessionId) -> anyhow::Result<(u32, SessionMeta)> {
    connection.send(&ClientMsg::Attach { id, size: None, mode: AttachMode::MetaOnly })?;
    match connection.reply()? {
        HostMsg::Attached { channel, meta, .. } => Ok((channel, meta)),
        other => bail!("unexpected answer: {}", kind(&other)),
    }
}

/// 从标准输入读要打的字，去掉末尾的一个换行（`echo` 带的那个）。
fn stdin_text() -> anyhow::Result<Vec<u8>> {
    let mut text = Vec::new();
    std::io::stdin().read_to_end(&mut text)?;
    if text.ends_with(b"\n") {
        text.pop();
    }
    Ok(text)
}

fn unexpected(message: &HostMsg) -> Failure {
    anyhow!("unexpected answer: {}", kind(message)).into()
}

fn title(meta: &SessionMeta) -> Option<String> {
    meta.title.clone().or_else(|| meta.fallback_title.clone())
}

fn state_name(state: AgentState) -> &'static str {
    match state {
        AgentState::Working => "working",
        AgentState::Idle => "idle",
        AgentState::Blocked => "blocked",
    }
}

/// 家目录下的路径写成 `~/…`。
fn tilde(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}
