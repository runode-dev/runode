//! 预览栏编辑器的语言服务器：按文件挑一个装在项目所在电脑上的服务器（`runode_lsp::servers`），一个 workspace、
//! 一个程序起一个进程，开着的文件共用，都关了就让它退出；编辑器从它拿补全、悬停说明、跳到定义和诊断。没装服务器、
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
    time::Duration,
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
        CompletionContext, CompletionParams, CompletionResponse, Diagnostic, GotoDefinitionParams,
        GotoDefinitionResponse, Hover, HoverParams, InitializeResult, LocationLink, PublishDiagnosticsParams,
        ShowDocumentParams, TextDocumentIdentifier, TextDocumentPositionParams, Uri,
        request::{Completion, GotoDefinition, HoverRequest, Request},
    },
    uri,
};

use super::{PreviewPanel, WorkspaceId, tabs::PreviewTabs};
use crate::{config::AppConfig, host_client::ssh::RemoteHost};

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
        self.server.client.change(self.uri.clone(), self.version, text.to_string());
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
    /// 起不来、握手不成，或者远端主机上没装，这个 workspace 里不再试。
    Failed,
}

/// 各 workspace 里起过的语言服务器，按程序名。
#[derive(Default)]
pub(super) struct Servers(HashMap<(WorkspaceId, &'static str), Slot>);

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
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut command, 0x0800_0000);
    let mut child = command.spawn()?;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(io::Error::other("the language server has no stdio"));
    };
    Ok(Some(Process { child, input: Box::new(stdin), output: Box::new(stdout) }))
}

/// 起 `spec` 这个语言服务器、握好手，`host` 不为空时起在那台 SSH 主机上。项目根目录是 `root`；本机上没给
/// 时是 `file` 往上第一个有 `.git` 的目录，都没有时是 `file` 所在的目录。诊断发进 `diagnostics`。用户没装
/// 这个程序时是 `None`。会阻塞。
fn start(
    spec: servers::Server,
    file: &Path,
    root: Option<PathBuf>,
    host: Option<Arc<RemoteHost>>,
    diagnostics: UnboundedSender<PublishDiagnosticsParams>,
) -> io::Result<Option<Server>> {
    let found = host.is_none().then(|| file.ancestors().skip(1).find(|dir| dir.join(".git").exists())).flatten();
    let root = root.as_deref().or(found).or(file.parent()).unwrap_or(file);
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
            let triggers = reply.capabilities.completion_provider.and_then(|options| options.trigger_characters);
            Ok(Some(Server { client, child: Some(child), triggers: triggers.unwrap_or_default() }))
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
    /// shortcut: 列按 Unicode 码点数，只肯用 UTF-16 的服务器在有 BMP 之外字符的行上位置会偏；遇到这样的服务器时按握手定下的编码换算。
    fn at(&self, text: &Rope, offset: usize) -> TextDocumentPositionParams {
        TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: self.uri.clone() },
            position: text.offset_to_position(offset),
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
        cx.background_spawn(async move { Ok(reply.await?.unwrap_or(CompletionResponse::Array(Vec::new()))) })
    }

    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        new_text.chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_')
            || self.server.triggers.iter().any(|trigger| !trigger.is_empty() && new_text.ends_with(trigger.as_str()))
    }
}

impl HoverProvider for Provider {
    fn hover(&self, text: &Rope, offset: usize, _: &mut Window, cx: &mut App) -> Task<anyhow::Result<Option<Hover>>> {
        self.ask::<HoverRequest>(
            HoverParams {
                text_document_position_params: self.at(text, offset),
                work_done_progress_params: Default::default(),
            },
            cx,
        )
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
        cx.background_spawn(async move {
            let locations = match reply.await? {
                Some(GotoDefinitionResponse::Link(links)) => return Ok(links),
                Some(GotoDefinitionResponse::Scalar(location)) => vec![location],
                Some(GotoDefinitionResponse::Array(locations)) => locations,
                None => Vec::new(),
            };
            Ok(locations
                .into_iter()
                .map(|location| LocationLink {
                    origin_selection_range: None,
                    target_uri: location.uri,
                    target_range: location.range,
                    target_selection_range: location.range,
                })
                .collect())
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
        // 远端的目录在这边查不了：文件在主仓库里时用仓库的根。
        let root =
            host.as_ref().and(tabs.git.as_ref()).map(|git| git.main.root.clone()).filter(|root| path.starts_with(root));
        let remote = host.is_some();
        let key = (ws, spec.program);
        match self.servers.0.get(&key) {
            Some(Slot::Starting | Slot::Failed) => return,
            Some(Slot::Running(server)) if server.strong_count() > 0 => {
                let server = server.upgrade();
                self.open_documents(ws, spec.program, server, cx);
                return;
            }
            _ => {}
        }
        self.servers.0.insert(key, Slot::Starting);
        let (sender, mut diagnostics) = futures::channel::mpsc::unbounded();
        let file = path.to_path_buf();
        let job = cx.background_spawn(async move { start(spec, &file, root, host, sender) });
        cx.spawn(async move |this, cx| {
            let started = job.await;
            this.update(cx, |this, cx| match started {
                Ok(Some(server)) => {
                    let server = Rc::new(server);
                    this.servers.0.insert(key, Slot::Running(Rc::downgrade(&server)));
                    this.open_documents(ws, spec.program, Some(server), cx);
                }
                // 远端每试一次都要起一条 ssh。
                Ok(None) if remote => {
                    this.servers.0.insert(key, Slot::Failed);
                }
                Ok(None) => {
                    this.servers.0.remove(&key);
                }
                Err(err) => {
                    tracing::warn!("starting {}: {err}", spec.program);
                    this.servers.0.insert(key, Slot::Failed);
                }
            })
            .ok();
            while let Some(params) = diagnostics.next().await {
                if this.update(cx, |this, cx| this.show_diagnostics(ws, params, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// workspace `ws` 里该用 `program`、还没接上的编辑器都在 `server` 上打开文档，问它要补全、悬停和定义。
    fn open_documents(&mut self, ws: WorkspaceId, program: &str, server: Option<Rc<Server>>, cx: &mut Context<Self>) {
        let (Some(server), Some(tabs)) = (server, self.workspaces.get_mut(&ws)) else {
            return;
        };
        let panel = cx.weak_entity();
        for at in 0..tabs.tabs.len() {
            let path = tabs.tabs[at].path.clone();
            let Some(spec) = server_for(tabs, &path, cx).filter(|spec| spec.program == program) else {
                continue;
            };
            let tab = &mut tabs.tabs[at];
            let (Some(editor), None, Some(uri)) = (&mut tab.editor, &tab.diff, uri::from_path(&path)) else {
                continue;
            };
            if editor.lsp.is_some() {
                continue;
            }
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

    /// 把 workspace `ws` 的语言服务器对一个文档的诊断画到开着它的编辑器里；诊断说的不是编辑器里现在这一版时
    /// 不画。
    fn show_diagnostics(&mut self, ws: WorkspaceId, params: PublishDiagnosticsParams, cx: &mut Context<Self>) {
        // 按路径认文档：同一个路径，服务器写出的 URI 和这边写的转义可以不一样。
        let Some(path) = uri::to_path(&params.uri) else {
            return;
        };
        let tabs =
            self.workspaces.get_mut(&ws).into_iter().flat_map(|tabs| &mut tabs.tabs).filter(|tab| tab.path == path);
        for editor in tabs.filter_map(|tab| tab.editor.as_mut()) {
            let Some(document) = editor.lsp.as_mut() else {
                continue;
            };
            if params.version.is_some_and(|version| version != document.version) {
                continue;
            }
            document.diagnostics.clone_from(&params.diagnostics);
            document.show_diagnostics(&editor.area, cx);
        }
    }
}
