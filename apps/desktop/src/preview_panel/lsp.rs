//! 预览栏编辑器的语言服务器：按文件挑一个装在项目所在电脑上的服务器（`runode_lsp::servers`），一个 workspace 里
//! 每个 git 仓库、每个程序起一个进程，开着的文件共用，都关了就让它退出；编辑器从它拿补全、悬停说明、跳到定义和诊断。没装服务器、
//! 配置关着（`language-server`）、项目在配对的电脑或 Windows 的 SSH 主机上时编辑照常，只是没有这些。

pub(crate) mod remote;

use std::{
    collections::HashMap,
    ffi::OsString,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    rc::{Rc, Weak},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use futures::{
    StreamExt as _,
    channel::{mpsc::UnboundedSender, oneshot},
};
use gpui::{App, AppContext as _, Context, Entity, Task, Window};
use gpui_component::input::{CompletionProvider, DefinitionProvider, EditorState, HoverProvider, Rope, RopeExt as _};
use runode_lsp::{
    Client, servers,
    types::{
        CompletionContext, CompletionParams, CompletionResponse, CompletionTextEdit, Diagnostic, GotoDefinitionParams,
        GotoDefinitionResponse, Hover, HoverParams, InitializeResult, LocationLink, Position, PositionEncodingKind,
        PublishDiagnosticsParams, Range, ShowDocumentParams, TextDocumentIdentifier, TextDocumentPositionParams, Uri,
        request::{Completion, GotoDefinition, HoverRequest, Request},
    },
    uri,
};

use super::{PreviewPanel, WorkspaceId, tabs::PreviewTabs};
use crate::{config::AppConfig, host_client::ssh::RemoteHost};

/// 语言服务器跑了这么久以后自己退出的重新起；没跑这么久就退的不再起，免得起了又退地转。
const RESTART_AFTER: Duration = Duration::from_secs(60);
/// 语言服务器握手最多等这么久，过了算起不来。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// 请语言服务器退出后隔这么久看一次它退了没有，看这么多次还没退就结束掉它。
const EXIT_POLL: Duration = Duration::from_millis(100);
const EXIT_POLLS: u32 = 30;

/// 一个跑着的语言服务器。丢掉时请它退出，不肯退的结束掉。
pub(super) struct Server {
    client: Client,
    child: Option<Child>,
    /// 服务器要求的、打了就问补全的字符。
    triggers: Vec<String>,
    /// 协议里位置的列按 UTF-16 码元数；否则按 Unicode 码点数，和编辑器的一样。
    utf16: bool,
}

/// 把 `position` 的列在 UTF-16 码元数和 Unicode 码点数之间换算，`text` 是它所在的文档；`to_utf16` 时从码点数
/// 换成码元数，否则反过来。
fn convert_column(text: &Rope, position: Position, to_utf16: bool) -> Position {
    let (mut chars, mut units) = (0, 0);
    for c in text.slice_line(position.line as usize).chars() {
        if (if to_utf16 { chars } else { units }) >= position.character {
            break;
        }
        chars += 1;
        units += c.len_utf16() as u32;
    }
    Position { line: position.line, character: if to_utf16 { units } else { chars } }
}

impl Server {
    /// 服务器给的 `range` 换成编辑器的写法，`text` 是它所在的文档。
    fn range_from_wire(&self, text: &Rope, range: Range) -> Range {
        if !self.utf16 {
            return range;
        }
        Range { start: convert_column(text, range.start, false), end: convert_column(text, range.end, false) }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.client.shutdown();
        let Some(mut child) = self.child.take() else {
            return;
        };
        std::thread::spawn(move || {
            for _ in 0..EXIT_POLLS {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(EXIT_POLL);
            }
            child.kill().ok();
            child.wait().ok();
        });
    }
}

/// 一个编辑器在语言服务器上开着的文档。丢掉时告诉服务器它关了。
pub(super) struct Document {
    server: Rc<Server>,
    uri: Uri,
    version: i32,
    /// 服务器最近报的诊断。
    diagnostics: Vec<Diagnostic>,
}

impl Document {
    /// 把最近的诊断画进编辑器 `area`。编辑器每次改动都清掉画着的诊断，服务器只在诊断变了时重发，所以改动后
    /// 也要画一遍。
    pub(super) fn show_diagnostics(&self, area: &Entity<EditorState>, cx: &mut App) {
        area.update(cx, |area, cx| {
            let text = area.text().clone();
            if let Some(shown) = area.diagnostics_mut() {
                shown.reset(&text);
                shown.extend(self.diagnostics.iter().cloned());
            }
            cx.notify();
        });
    }

    /// 编辑器里的文字变成了 `text`。
    pub(super) fn changed(&mut self, text: &Rope) {
        self.version += 1;
        let text = text.clone();
        self.server.client.change(self.uri.clone(), self.version, move || text.to_string());
    }

    /// 编辑器里的文字存盘了。
    pub(super) fn saved(&self) {
        self.server.client.save(self.uri.clone());
    }
}

impl Drop for Document {
    fn drop(&mut self) {
        self.server.client.close(self.uri.clone());
    }
}

enum Slot {
    Starting,
    Running(Weak<Server>),
    /// 起不来、握手不成、刚起来就退出，或者远端主机上没装，这个仓库里不再试。
    Failed,
}

/// 一个语言服务器管的范围：workspace、文件所在 git 仓库的根（`project_root`）、程序名。SSH 主机上的项目
/// 一个 workspace 只分程序，根为空。
type Key = (WorkspaceId, Option<PathBuf>, &'static str);

/// 各 workspace 里起过的语言服务器。
#[derive(Default)]
pub(super) struct Servers(HashMap<Key, Slot>);

/// 本机项目 `tabs` 里的 `path` 所在 git 仓库的根：往上第一个有 `.git` 的目录。不在仓库里的、SSH 主机上的
/// （那边的目录在这边查不了）没有。
fn project_root(tabs: &PreviewTabs, path: &Path) -> Option<PathBuf> {
    if tabs.project.is_remote() {
        return None;
    }
    path.ancestors().skip(1).find(|dir| dir.join(".git").exists()).map(Path::to_path_buf)
}

/// `tabs` 里 `path` 用的语言服务器；配置关着、项目所在的电脑上起不了、没有对应的服务器时没有。
fn server_for(tabs: &PreviewTabs, path: &Path, cx: &App) -> Option<servers::Server> {
    let reachable = !tabs.project.is_remote() || remote::host(&tabs.project).is_some();
    (cx.global::<AppConfig>().0.language_server && reachable).then(|| servers::for_path(path))?
}

/// 一个起好的语言服务器进程和它的 stdin、stdout；远端的是连着它的那个 ssh。
struct Process {
    child: Child,
    input: Box<dyn Write + Send>,
    output: Box<dyn Read + Send>,
}

/// `program` 在用户登录 shell 的 PATH 里的位置，和那个 PATH；没装时没有。会阻塞。
fn locate(program: &str) -> Option<(PathBuf, OsString)> {
    let path = crate::process_env::user_path(None)?;
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).filter(|dir| dir.is_absolute()).collect();
    Some((runode_terminal::shells::find_command(&dirs, program)?, path))
}

/// 在本机的 `root` 里起 `spec`；用户没装这个程序时是 `None`。会阻塞。
fn spawn(spec: servers::Server, root: &Path) -> io::Result<Option<Process>> {
    let Some((program, path)) = locate(spec.program) else {
        return Ok(None);
    };
    let mut command = Command::new(program);
    // 服务器自己还要找编译器、包管理器，给它用户 shell 里的 PATH。
    command
        .args(spec.args)
        .current_dir(root)
        .env("PATH", path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // app 没有控制台，不这样每起一个服务器就闪一个黑窗。
    crate::subprocess::hide_window(&mut command);
    let mut child = command.spawn()?;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(io::Error::other("the language server has no stdio"));
    };
    Ok(Some(Process { child, input: Box::new(stdin), output: Box::new(stdout) }))
}

/// 起 `spec` 这个语言服务器、握好手，`host` 不为空时起在那台 SSH 主机上。项目根目录是 `root`，没给时是
/// `file` 所在的目录。诊断发进 `diagnostics`。用户没装
/// 这个程序时是 `None`。会阻塞。
fn start(
    spec: servers::Server,
    file: &Path,
    root: Option<PathBuf>,
    host: Option<Arc<RemoteHost>>,
    diagnostics: UnboundedSender<PublishDiagnosticsParams>,
) -> io::Result<Option<Server>> {
    let root = root.as_deref().or(file.parent()).unwrap_or(file);
    let process = match &host {
        Some(host) => remote::spawn(host, spec)?,
        None => match spawn(spec, root)? {
            Some(process) => process,
            None => return Ok(None),
        },
    };
    let Process { mut child, input, output } = process;
    let client = Client::start(output, input, move |method, params| {
        if method == "textDocument/publishDiagnostics"
            && let Ok(params) = serde_json::from_value(params)
        {
            diagnostics.unbounded_send(params).ok();
        }
    });
    let (done, result) = mpsc::channel();
    client.initialize(root, host.is_none().then(std::process::id), move |reply| {
        done.send(reply).ok();
    });
    let reply: Result<InitializeResult, _> = match result.recv_timeout(HANDSHAKE_TIMEOUT) {
        Ok(reply) => reply,
        Err(_) => {
            child.kill().ok();
            child.wait().ok();
            return Err(io::ErrorKind::TimedOut.into());
        }
    };
    match reply {
        Ok(reply) => {
            let utf16 = reply.capabilities.position_encoding != Some(PositionEncodingKind::UTF32);
            let triggers = reply.capabilities.completion_provider.and_then(|options| options.trigger_characters);
            Ok(Some(Server { client, child: Some(child), triggers: triggers.unwrap_or_default(), utf16 }))
        }
        Err(err) => {
            // 流断了的是进程自己在退，等得到它的退出码；别的错结束掉它。
            if err != runode_lsp::Error::Closed {
                child.kill().ok();
            }
            let code = child.wait().ok().and_then(|status| status.code());
            if host.is_some() && code == Some(remote::NOT_INSTALLED) { Ok(None) } else { Err(io::Error::other(err)) }
        }
    }
}

/// 编辑器问语言服务器的那一头：`uri` 这个文档在 `server` 上。
struct Provider {
    server: Rc<Server>,
    uri: Uri,
}

impl Provider {
    /// `text` 里字节位置 `offset` 在协议里的写法。
    fn at(&self, text: &Rope, offset: usize) -> TextDocumentPositionParams {
        let position = text.offset_to_position(offset);
        TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: self.uri.clone() },
            position: if self.server.utf16 { convert_column(text, position, true) } else { position },
        }
    }

    fn ask<R: Request>(&self, params: R::Params, cx: &App) -> Task<anyhow::Result<R::Result>>
    where
        R::Result: Send + 'static,
    {
        let (done, reply) = oneshot::channel();
        self.server.client.request::<R>(params, move |reply| {
            done.send(reply).ok();
        });
        cx.background_spawn(async move { Ok(reply.await??) })
    }
}

impl CompletionProvider for Provider {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        trigger: CompletionContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        let reply = self.ask::<Completion>(
            CompletionParams {
                text_document_position: self.at(text, offset),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: Some(trigger),
            },
            cx,
        );
        let (server, text) = (self.server.clone(), text.clone());
        cx.spawn(async move |_| {
            let mut items = match reply.await? {
                Some(CompletionResponse::Array(items)) => items,
                Some(CompletionResponse::List(list)) => list.items,
                None => Vec::new(),
            };
            for item in &mut items {
                match &mut item.text_edit {
                    Some(CompletionTextEdit::Edit(edit)) => edit.range = server.range_from_wire(&text, edit.range),
                    Some(CompletionTextEdit::InsertAndReplace(edit)) => {
                        edit.insert = server.range_from_wire(&text, edit.insert);
                        edit.replace = server.range_from_wire(&text, edit.replace);
                    }
                    None => {}
                }
                for edit in item.additional_text_edits.iter_mut().flatten() {
                    edit.range = server.range_from_wire(&text, edit.range);
                }
            }
            Ok(CompletionResponse::Array(items))
        })
    }

    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        new_text.chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_')
            || self.server.triggers.iter().any(|trigger| !trigger.is_empty() && new_text.ends_with(trigger.as_str()))
    }
}

impl HoverProvider for Provider {
    fn hover(&self, text: &Rope, offset: usize, _: &mut Window, cx: &mut App) -> Task<anyhow::Result<Option<Hover>>> {
        let reply = self.ask::<HoverRequest>(
            HoverParams {
                text_document_position_params: self.at(text, offset),
                work_done_progress_params: Default::default(),
            },
            cx,
        );
        let (server, text) = (self.server.clone(), text.clone());
        cx.spawn(async move |_| {
            let mut hover = reply.await?;
            if let Some(hover) = &mut hover {
                hover.range = hover.range.map(|range| server.range_from_wire(&text, range));
            }
            Ok(hover)
        })
    }
}

impl DefinitionProvider for Provider {
    fn definitions(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Vec<LocationLink>>> {
        let reply = self.ask::<GotoDefinition>(
            GotoDefinitionParams {
                text_document_position_params: self.at(text, offset),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
            cx,
        );
        let (server, text, own) = (self.server.clone(), text.clone(), self.uri.clone());
        cx.spawn(async move |_| {
            let link = |uri, range| LocationLink {
                origin_selection_range: None,
                target_uri: uri,
                target_range: range,
                target_selection_range: range,
            };
            let mut links = match reply.await? {
                Some(GotoDefinitionResponse::Link(links)) => links,
                Some(GotoDefinitionResponse::Scalar(location)) => vec![link(location.uri, location.range)],
                Some(GotoDefinitionResponse::Array(locations)) => {
                    locations.into_iter().map(|location| link(location.uri, location.range)).collect()
                }
                None => Vec::new(),
            };
            for link in &mut links {
                link.origin_selection_range =
                    link.origin_selection_range.map(|range| server.range_from_wire(&text, range));
                // 别的文件里的目标只用到行号，行号不用换算。
                if uri::to_path(&link.target_uri) == uri::to_path(&own) {
                    link.target_range = server.range_from_wire(&text, link.target_range);
                    link.target_selection_range = server.range_from_wire(&text, link.target_selection_range);
                }
            }
            Ok(links)
        })
    }
}

impl PreviewPanel {
    /// 给 workspace `ws` 里 `path` 刚装上的编辑器接语言服务器：已经跑着的直接用，没起过的在后台起，起好了
    /// 把这个 workspace 里等着它的编辑器都接上。
    pub(super) fn attach_language_server(&mut self, ws: WorkspaceId, path: &Path, cx: &mut Context<Self>) {
        let Some(tabs) = self.workspaces.get(&ws) else {
            return;
        };
        let Some(spec) = server_for(tabs, path, cx) else {
            return;
        };
        let host = remote::host(&tabs.project);
        let remote = host.is_some();
        let key: Key = (ws, project_root(tabs, path), spec.program);
        // SSH 主机上的文件在主仓库里时用仓库的根。
        let main = tabs.git.as_ref().filter(|_| remote).map(|git| git.main.root.clone());
        let root = key.1.clone().or(main.filter(|root| path.starts_with(root)));
        match self.servers.0.get(&key) {
            Some(Slot::Starting | Slot::Failed) => return,
            Some(Slot::Running(server)) if server.strong_count() > 0 => {
                let server = server.upgrade();
                self.open_documents(&key, server, cx);
                return;
            }
            _ => {}
        }
        self.servers.0.insert(key.clone(), Slot::Starting);
        let (sender, mut diagnostics) = futures::channel::mpsc::unbounded();
        let file = path.to_path_buf();
        let job = cx.background_spawn(async move { start(spec, &file, root, host, sender) });
        cx.spawn(async move |this, cx| {
            let started = job.await;
            let since = Instant::now();
            let mut mine = Weak::new();
            this.update(cx, |this, cx| match started {
                Ok(Some(server)) => {
                    let server = Rc::new(server);
                    mine = Rc::downgrade(&server);
                    this.servers.0.insert(key.clone(), Slot::Running(mine.clone()));
                    this.open_documents(&key, Some(server), cx);
                }
                // 远端每试一次都要起一条 ssh。
                Ok(None) if remote => {
                    this.servers.0.insert(key.clone(), Slot::Failed);
                }
                Ok(None) => {
                    this.servers.0.remove(&key);
                }
                Err(err) => {
                    tracing::warn!("starting {}: {err}", spec.program);
                    this.servers.0.insert(key.clone(), Slot::Failed);
                }
            })
            .ok();
            while let Some(params) = diagnostics.next().await {
                if this.update(cx, |this, cx| this.show_diagnostics(ws, &mine, params, cx)).is_err() {
                    return;
                }
            }
            // 通知到头是连接断了。
            this.update(cx, |this, cx| this.server_exited(key, &mine, since.elapsed() >= RESTART_AFTER, cx)).ok();
        })
        .detach();
    }

    /// `key` 的服务器 `server` 的连接断了。还有编辑器接着它时是它自己退出了：把这些编辑器摘下来，`restart` 时
    /// 重新起一个接上，否则这个仓库里不再试。
    fn server_exited(&mut self, key: Key, server: &Weak<Server>, restart: bool, cx: &mut Context<Self>) {
        let running = matches!(self.servers.0.get(&key), Some(Slot::Running(running)) if running.ptr_eq(server));
        let (true, Some(server), Some(tabs)) = (running, server.upgrade(), self.workspaces.get_mut(&key.0)) else {
            return;
        };
        tracing::warn!("{} exited on its own", key.2);
        let mut orphan = None;
        for tab in &mut tabs.tabs {
            let Some(editor) = tab.editor.as_mut() else {
                continue;
            };
            if !editor.lsp.as_ref().is_some_and(|document| Rc::ptr_eq(&document.server, &server)) {
                continue;
            }
            editor.lsp = None;
            editor.area.update(cx, |area, cx| {
                let text = area.text().clone();
                if let Some(shown) = area.diagnostics_mut() {
                    shown.reset(&text);
                }
                cx.notify();
                let lsp = area.lsp_mut();
                lsp.completion_provider = None;
                lsp.hover_provider = None;
                lsp.definition_provider = None;
                lsp.show_document = None;
            });
            orphan = Some(tab.path.clone());
        }
        if restart {
            self.servers.0.remove(&key);
            if let Some(path) = orphan {
                self.attach_language_server(key.0, &path, cx);
            }
        } else {
            self.servers.0.insert(key, Slot::Failed);
        }
    }

    /// 归 `key` 管、还没接上的编辑器都在 `server` 上打开文档，问它要补全、悬停和定义。
    fn open_documents(&mut self, key: &Key, server: Option<Rc<Server>>, cx: &mut Context<Self>) {
        let (ws, root, program) = key;
        let (Some(server), Some(tabs)) = (server, self.workspaces.get_mut(ws)) else {
            return;
        };
        let panel = cx.weak_entity();
        for at in 0..tabs.tabs.len() {
            let path = tabs.tabs[at].path.clone();
            let Some(spec) = server_for(tabs, &path, cx).filter(|spec| spec.program == *program) else {
                continue;
            };
            if tabs.tabs[at].editor.as_ref().is_none_or(|editor| editor.lsp.is_some())
                || project_root(tabs, &path) != *root
            {
                continue;
            }
            let tab = &mut tabs.tabs[at];
            let (Some(editor), None, Some(uri)) = (&mut tab.editor, &tab.diff, uri::from_path(&path)) else {
                continue;
            };
            server.client.open(uri.clone(), spec.language_id, editor.area.read(cx).text().to_string());
            let provider = Rc::new(Provider { server: server.clone(), uri: uri.clone() });
            let (panel, own) = (panel.clone(), path);
            editor.area.update(cx, |area, _| {
                let lsp = area.lsp_mut();
                lsp.completion_provider = Some(provider.clone());
                lsp.hover_provider = Some(provider.clone());
                lsp.definition_provider = Some(provider);
                // 定义在别的文件里的开成标签；在这个文件里的由编辑器自己跳，网址由它交给浏览器。
                lsp.show_document = Some(Rc::new(move |target: &ShowDocumentParams, _: &mut Window, cx: &mut App| {
                    let Some(path) = uri::to_path(&target.uri).filter(|path| *path != own) else {
                        return false;
                    };
                    let line = target.selection.map_or(0, |range| range.start.line as usize);
                    let panel = panel.clone();
                    cx.defer(move |cx| {
                        panel.update(cx, |this, cx| this.open_at(&path, line, Some(0..0), false, cx)).ok();
                    });
                    true
                }));
            });
            editor.lsp = Some(Document { server: server.clone(), uri, version: 0, diagnostics: Vec::new() });
        }
    }

    /// 把 workspace `ws` 的语言服务器 `server` 对一个文档的诊断画到在它上面开着这个文档的编辑器里；诊断说的
    /// 不是编辑器里现在这一版时不画。
    fn show_diagnostics(
        &mut self,
        ws: WorkspaceId,
        server: &Weak<Server>,
        params: PublishDiagnosticsParams,
        cx: &mut Context<Self>,
    ) {
        // 按路径认文档：同一个路径，服务器写出的 URI 和这边写的转义可以不一样。
        let Some(path) = uri::to_path(&params.uri) else {
            return;
        };
        let tabs =
            self.workspaces.get_mut(&ws).into_iter().flat_map(|tabs| &mut tabs.tabs).filter(|tab| tab.path == path);
        for editor in tabs.filter_map(|tab| tab.editor.as_mut()) {
            let Some(document) = editor.lsp.as_mut().filter(|document| Rc::as_ptr(&document.server) == server.as_ptr())
            else {
                continue;
            };
            if params.version.is_some_and(|version| version != document.version) {
                continue;
            }
            let text = editor.area.read(cx).text();
            document.diagnostics = params
                .diagnostics
                .iter()
                .cloned()
                .map(|mut diagnostic| {
                    diagnostic.range = document.server.range_from_wire(text, diagnostic.range);
                    diagnostic
                })
                .collect();
            document.show_diagnostics(&editor.area, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_convert_between_code_points_and_utf16_units() {
        let text = Rope::from("x\na😀b中c");
        let at = |character| Position { line: 1, character };
        for (chars, units) in [(0, 0), (1, 1), (2, 3), (3, 4), (4, 5), (5, 6)] {
            assert_eq!(convert_column(&text, at(chars), true), at(units));
            assert_eq!(convert_column(&text, at(units), false), at(chars));
        }
        // 列超出行尾的停在行尾。
        assert_eq!(convert_column(&text, at(9), true), at(6));
        assert_eq!(convert_column(&text, at(9), false), at(5));
    }
}
