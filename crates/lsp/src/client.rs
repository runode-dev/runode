//! 一条连到语言服务器的 JSON-RPC 连接。

use std::{
    collections::HashMap,
    fmt,
    io::{BufReader, Read, Write},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
        mpsc,
    },
    thread,
};

use lsp_types::{
    ClientCapabilities, CompletionClientCapabilities, CompletionItemCapability, DidChangeTextDocumentParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams, GeneralClientCapabilities,
    GotoCapability, HoverClientCapabilities, InitializeParams, InitializeResult, InitializedParams, MarkupKind,
    PositionEncodingKind, PublishDiagnosticsClientCapabilities, TextDocumentClientCapabilities,
    TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem, TextDocumentSyncClientCapabilities, Uri,
    VersionedTextDocumentIdentifier, WorkspaceFolder,
    notification::{
        DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument, Exit, Initialized,
        Notification,
    },
    request::{Initialize, Request, Shutdown},
};
use serde_json::{Value, json};

use crate::{
    transport::{read_message, write_message},
    uri,
};

/// 一次请求没拿到结果的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// 连接断了：服务器退出了，或者流读写出错。
    Closed,
    /// 服务器答了一个错误。
    Server { code: i64, message: String },
    /// 应答不是这个请求该有的样子。
    Malformed(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => f.write_str("the language server connection is closed"),
            Self::Server { code, message } => write!(f, "the language server answered {code}: {message}"),
            Self::Malformed(why) => write!(f, "the language server's reply doesn't parse: {why}"),
        }
    }
}

impl std::error::Error for Error {}

type Reply = Box<dyn FnOnce(Result<Value, Error>) + Send>;

struct Shared {
    /// 等应答的请求，按请求 id；连接断了以后是 `None`。
    pending: Mutex<Option<HashMap<i64, Reply>>>,
    next_id: AtomicI64,
}

/// 连到一个语言服务器的客户端；克隆出来的是同一条连接。发消息不阻塞，应答和通知的回调在读线程上跑。
/// 读写的线程到对方关了流才结束。
#[derive(Clone)]
pub struct Client {
    shared: Arc<Shared>,
    outgoing: mpsc::Sender<Vec<u8>>,
}

impl Client {
    /// 在 `reader`、`writer` 这对流上起一个客户端，读写各一个线程。服务器发来的通知交给 `on_notification`
    /// （方法名和参数）；服务器发来的请求在这里答掉。
    pub fn start(
        reader: impl Read + Send + 'static,
        mut writer: impl Write + Send + 'static,
        on_notification: impl Fn(&str, Value) + Send + 'static,
    ) -> Self {
        let (outgoing, queue) = mpsc::channel::<Vec<u8>>();
        let shared = Arc::new(Shared { pending: Mutex::new(Some(HashMap::new())), next_id: AtomicI64::new(1) });
        thread::spawn(move || {
            for body in queue {
                if let Err(err) = write_message(&mut writer, &body) {
                    tracing::debug!("writing to the language server: {err}");
                    break;
                }
            }
        });
        let client = Self { shared: shared.clone(), outgoing: outgoing.clone() };
        thread::spawn(move || {
            let mut reader = BufReader::new(reader);
            loop {
                match read_message(&mut reader) {
                    Ok(Some(body)) => match serde_json::from_slice::<Value>(&body) {
                        Ok(message) => receive(&shared, &outgoing, message, &on_notification),
                        Err(err) => tracing::debug!("a message from the language server isn't JSON: {err}"),
                    },
                    Ok(None) => break,
                    Err(err) => {
                        tracing::debug!("reading from the language server: {err}");
                        break;
                    }
                }
            }
            let pending = shared.pending.lock().unwrap().take();
            for reply in pending.into_iter().flat_map(HashMap::into_values) {
                reply(Err(Error::Closed));
            }
        });
        client
    }

    /// 发一个请求，应答到了（或者连接断了）调 `done`。
    pub fn request<R: Request>(&self, params: R::Params, done: impl FnOnce(Result<R::Result, Error>) + Send + 'static) {
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let reply: Reply = Box::new(move |result| {
            done(
                result.and_then(|value| serde_json::from_value(value).map_err(|err| Error::Malformed(err.to_string()))),
            );
        });
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": R::METHOD, "params": params });
        let rejected = match self.shared.pending.lock().unwrap().as_mut() {
            Some(pending) => {
                pending.insert(id, reply);
                None
            }
            None => Some(reply),
        };
        match rejected {
            Some(reply) => reply(Err(Error::Closed)),
            None => self.send(&message),
        }
    }

    /// 发一个通知。
    pub fn notify<N: Notification>(&self, params: N::Params) {
        self.send(&json!({ "jsonrpc": "2.0", "method": N::METHOD, "params": params }));
    }

    fn send(&self, message: &Value) {
        // 写线程已经停了的不管：读线程随后会让等着的请求都落空。
        self.outgoing.send(message.to_string().into_bytes()).ok();
    }

    /// 握手：报上客户端支持什么、项目根目录是 `root`，服务器答了以后告诉它可以开始，再把它的应答交给 `done`。
    /// 握手完成之前不发别的消息。位置里的列优先按 Unicode 码点数。
    pub fn initialize(&self, root: &Path, done: impl FnOnce(Result<InitializeResult, Error>) + Send + 'static) {
        let folder = uri::from_path(root).map(|uri| WorkspaceFolder {
            uri,
            name: root.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default(),
        });
        let params = InitializeParams {
            process_id: Some(std::process::id()),
            workspace_folders: folder.map(|folder| vec![folder]),
            capabilities: capabilities(),
            ..InitializeParams::default()
        };
        let client = self.clone();
        self.request::<Initialize>(params, move |result| {
            if result.is_ok() {
                client.notify::<Initialized>(InitializedParams {});
            }
            done(result);
        });
    }

    /// 告诉服务器打开了 `uri`，内容是 `text`，版本从 0 起。
    pub fn open(&self, uri: Uri, language_id: &str, text: String) {
        self.notify::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem { uri, language_id: language_id.to_owned(), version: 0, text },
        });
    }

    /// 告诉服务器 `uri` 的全文变成了 `text`；`version` 每次比上一次大。
    pub fn change(&self, uri: Uri, version: i32, text: String) {
        self.notify::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier { uri, version },
            content_changes: vec![TextDocumentContentChangeEvent { range: None, range_length: None, text }],
        });
    }

    /// 告诉服务器 `uri` 存盘了。
    pub fn save(&self, uri: Uri) {
        self.notify::<DidSaveTextDocument>(DidSaveTextDocumentParams {
            text_document: TextDocumentIdentifier { uri },
            text: None,
        });
    }

    /// 告诉服务器 `uri` 关了。
    pub fn close(&self, uri: Uri) {
        self.notify::<DidCloseTextDocument>(DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier { uri },
        });
    }

    /// 请服务器收尾后退出。
    pub fn shutdown(&self) {
        let client = self.clone();
        self.request::<Shutdown>((), move |_| client.notify::<Exit>(()));
    }
}

fn capabilities() -> ClientCapabilities {
    let markup = Some(vec![MarkupKind::Markdown, MarkupKind::PlainText]);
    ClientCapabilities {
        general: Some(GeneralClientCapabilities {
            position_encodings: Some(vec![PositionEncodingKind::UTF32, PositionEncodingKind::UTF16]),
            ..GeneralClientCapabilities::default()
        }),
        text_document: Some(TextDocumentClientCapabilities {
            synchronization: Some(TextDocumentSyncClientCapabilities {
                did_save: Some(true),
                ..TextDocumentSyncClientCapabilities::default()
            }),
            completion: Some(CompletionClientCapabilities {
                completion_item: Some(CompletionItemCapability {
                    snippet_support: Some(false),
                    documentation_format: markup.clone(),
                    ..CompletionItemCapability::default()
                }),
                ..CompletionClientCapabilities::default()
            }),
            hover: Some(HoverClientCapabilities { content_format: markup, ..HoverClientCapabilities::default() }),
            definition: Some(GotoCapability { link_support: Some(true), ..GotoCapability::default() }),
            publish_diagnostics: Some(PublishDiagnosticsClientCapabilities::default()),
            ..TextDocumentClientCapabilities::default()
        }),
        ..ClientCapabilities::default()
    }
}

/// 处理服务器发来的一条消息：应答交给等它的请求，通知交给 `on_notification`，请求当场答掉。
fn receive(
    shared: &Shared,
    outgoing: &mpsc::Sender<Vec<u8>>,
    mut message: Value,
    on_notification: &impl Fn(&str, Value),
) {
    let id = message.get("id").cloned();
    let Some(method) = message.get("method").and_then(Value::as_str).map(str::to_owned) else {
        let Some(id) = id.as_ref().and_then(Value::as_i64) else {
            return;
        };
        let reply = shared.pending.lock().unwrap().as_mut().and_then(|pending| pending.remove(&id));
        if let Some(reply) = reply {
            reply(match message.get_mut("error") {
                Some(error) => Err(Error::Server {
                    code: error.get("code").and_then(Value::as_i64).unwrap_or_default(),
                    message: error.get("message").and_then(Value::as_str).unwrap_or_default().to_owned(),
                }),
                None => Ok(message.get_mut("result").map(Value::take).unwrap_or_default()),
            });
        }
        return;
    };
    let params = message.get_mut("params").map(Value::take).unwrap_or_default();
    let Some(id) = id else {
        on_notification(&method, params);
        return;
    };
    let answer = match method.as_str() {
        // 没有给服务器的配置：每一项都答空，服务器用自己的默认值。
        "workspace/configuration" => {
            let items = params.get("items").and_then(Value::as_array).map_or(0, Vec::len);
            json!({ "result": vec![Value::Null; items] })
        }
        "client/registerCapability" | "client/unregisterCapability" | "window/workDoneProgress/create" => {
            json!({ "result": null })
        }
        _ => json!({ "error": { "code": -32601, "message": format!("{method} isn't supported") } }),
    };
    let mut reply = json!({ "jsonrpc": "2.0", "id": id });
    reply.as_object_mut().unwrap().extend(answer.as_object().unwrap().clone());
    outgoing.send(reply.to_string().into_bytes()).ok();
}
