//! 测试里的假语言服务器：在一对管道上按脚本回话。

use std::{
    io::{BufRead, BufReader, PipeReader, PipeWriter, Read, Write},
    sync::mpsc,
};

use runode_lsp::Client;
use serde_json::Value;

/// 假服务器那一头：读客户端发来的消息，往回写。
pub struct Server {
    input: BufReader<PipeReader>,
    output: PipeWriter,
}

impl Server {
    /// 读客户端的下一条消息；客户端关了流时是 `None`。
    pub fn read(&mut self) -> Option<Value> {
        let mut length = 0;
        loop {
            let mut line = String::new();
            if self.input.read_line(&mut line).unwrap() == 0 {
                return None;
            }
            match line.trim_end().split_once(": ") {
                Some((_, value)) => length = value.parse().unwrap(),
                None => break,
            }
        }
        let mut body = vec![0; length];
        self.input.read_exact(&mut body).unwrap();
        Some(serde_json::from_slice(&body).unwrap())
    }

    /// 读到方法是 `method` 的那条消息为止，中间的丢掉。
    pub fn expect(&mut self, method: &str) -> Value {
        loop {
            let message = self.read().unwrap_or_else(|| panic!("the client hung up before sending {method}"));
            if message["method"] == method {
                return message;
            }
        }
    }

    pub fn send(&mut self, message: &Value) {
        let body = message.to_string();
        write!(self.output, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    }

    /// 答 `request` 这个请求。
    pub fn reply(&mut self, request: &Value, result: Value) {
        self.send(&serde_json::json!({ "jsonrpc": "2.0", "id": request["id"], "result": result }));
    }
}

/// 一个连着假服务器的客户端，和它收到的通知（方法名和参数）。
pub fn connect() -> (Client, Server, mpsc::Receiver<(String, Value)>) {
    let (client_reads, server_writes) = std::io::pipe().unwrap();
    let (server_reads, client_writes) = std::io::pipe().unwrap();
    let (notified, notifications) = mpsc::channel();
    let client = Client::start(client_reads, client_writes, move |method, params| {
        notified.send((method.to_owned(), params)).ok();
    });
    (client, Server { input: BufReader::new(server_reads), output: server_writes }, notifications)
}
