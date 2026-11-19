//! 客户端对着假服务器：握手、请求和应答、通知、服务器发来的请求、断线。

mod common;

use std::{path::Path, sync::mpsc, time::Duration};

use runode_lsp::{
    Error,
    types::{
        CompletionParams, CompletionResponse, Position, TextDocumentIdentifier, TextDocumentPositionParams,
        request::Completion,
    },
    uri,
};
use serde_json::json;

const WAIT: Duration = Duration::from_secs(10);

fn completion_params() -> CompletionParams {
    CompletionParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri::from_path(Path::new("/p/a.rs")).unwrap() },
            position: Position::new(1, 2),
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
        context: None,
    }
}

#[test]
fn handshake_reports_the_root_and_confirms() {
    let (client, mut server, _notifications) = common::connect();
    let (done, result) = mpsc::channel();
    client.initialize(Path::new("/p/my project"), Some(7), move |reply| done.send(reply).unwrap());
    let request = server.expect("initialize");
    assert_eq!(request["params"]["workspaceFolders"][0]["uri"], "file:///p/my%20project");
    assert_eq!(request["params"]["workspaceFolders"][0]["name"], "my project");
    assert_eq!(request["params"]["capabilities"]["general"]["positionEncodings"][0], "utf-32");
    server.reply(&request, json!({ "capabilities": { "hoverProvider": true } }));
    let reply = result.recv_timeout(WAIT).unwrap().unwrap();
    assert!(reply.capabilities.hover_provider.is_some());
    server.expect("initialized");
}

#[test]
fn documents_are_synced_as_full_text() {
    let (client, mut server, _notifications) = common::connect();
    let uri = uri::from_path(Path::new("/p/a.rs")).unwrap();
    client.open(uri.clone(), "rust", "fn a() {}".into());
    client.change(uri.clone(), 1, || "fn b() {}".into());
    client.save(uri.clone());
    client.close(uri);
    let opened = server.expect("textDocument/didOpen");
    assert_eq!(
        opened["params"]["textDocument"],
        json!({ "uri": "file:///p/a.rs", "languageId": "rust", "version": 0, "text": "fn a() {}" })
    );
    let changed = server.expect("textDocument/didChange");
    assert_eq!(changed["params"]["textDocument"]["version"], 1);
    assert_eq!(changed["params"]["contentChanges"], json!([{ "text": "fn b() {}" }]));
    server.expect("textDocument/didSave");
    server.expect("textDocument/didClose");
}

#[test]
fn replies_reach_the_request_that_asked() {
    let (client, mut server, _notifications) = common::connect();
    let (done, results) = mpsc::channel();
    for name in ["first", "second"] {
        let done = done.clone();
        client.request::<Completion>(completion_params(), move |reply| done.send((name, reply)).unwrap());
    }
    let first = server.expect("textDocument/completion");
    let second = server.expect("textDocument/completion");
    assert_eq!(first["params"]["position"], json!({ "line": 1, "character": 2 }));
    // 后发的先答，各自回到自己的回调。
    server.reply(&second, json!([{ "label": "two" }]));
    server.send(&json!({ "jsonrpc": "2.0", "id": first["id"], "error": { "code": -32800, "message": "cancelled" } }));
    let (name, reply) = results.recv_timeout(WAIT).unwrap();
    assert_eq!(name, "second");
    let Ok(Some(CompletionResponse::Array(items))) = reply else {
        panic!("not a completion list: {reply:?}");
    };
    assert_eq!(items[0].label, "two");
    let (name, reply) = results.recv_timeout(WAIT).unwrap();
    assert_eq!(name, "first");
    assert_eq!(reply.unwrap_err(), Error::Server { code: -32800, message: "cancelled".into() });
}

#[test]
fn notifications_are_handed_over_and_server_requests_answered() {
    let (_client, mut server, notifications) = common::connect();
    server.send(&json!({ "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": { "uri": "file:///p/a.rs", "diagnostics": [] } }));
    let (method, params) = notifications.recv_timeout(WAIT).unwrap();
    assert_eq!(method, "textDocument/publishDiagnostics");
    assert_eq!(params["uri"], "file:///p/a.rs");

    server.send(
        &json!({ "jsonrpc": "2.0", "id": 7, "method": "workspace/configuration", "params": { "items": [{}, {}] } }),
    );
    assert_eq!(server.read().unwrap(), json!({ "jsonrpc": "2.0", "id": 7, "result": [null, null] }));
    server.send(&json!({ "jsonrpc": "2.0", "id": "x", "method": "workspace/applyEdit", "params": {} }));
    let refused = server.read().unwrap();
    assert_eq!(refused["id"], "x");
    assert_eq!(refused["error"]["code"], -32601);
}

#[test]
fn a_closed_connection_fails_waiting_and_later_requests() {
    let (client, mut server, _notifications) = common::connect();
    let (done, results) = mpsc::channel();
    let waiting = done.clone();
    client.request::<Completion>(completion_params(), move |reply| waiting.send(reply).unwrap());
    server.expect("textDocument/completion");
    drop(server);
    assert_eq!(results.recv_timeout(WAIT).unwrap().unwrap_err(), Error::Closed);
    client.request::<Completion>(completion_params(), move |reply| done.send(reply).unwrap());
    assert_eq!(results.recv_timeout(WAIT).unwrap().unwrap_err(), Error::Closed);
}

#[test]
fn shutdown_asks_then_tells_the_server_to_exit() {
    let (client, mut server, _notifications) = common::connect();
    client.shutdown();
    let request = server.expect("shutdown");
    server.reply(&request, json!(null));
    server.expect("exit");
}
