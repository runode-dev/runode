//! 消息里路径的写法：一律写成字符串，不是合法 UTF-8 的字节换成 U+FFFD。serde 给 `PathBuf` 的默认
//! 写法遇到这种路径直接报错，整条消息（会话列表、git 状态）就发不出去；前端（含手机）都按字符串读，
//! 所以宁可丢掉那几个字节。合法 UTF-8 时和默认写法一模一样，读的一方照旧按 `PathBuf` 读。
//! 字段上写 `#[serde(serialize_with = "…::lossy_path::serialize")]`，`Option` 的用 `serialize_opt`，`Vec` 的用 `serialize_vec`。

use std::path::Path;

use serde::Serializer;

pub fn serialize<P: AsRef<Path>, S: Serializer>(path: &P, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&path.as_ref().to_string_lossy())
}

pub fn serialize_opt<P: AsRef<Path>, S: Serializer>(path: &Option<P>, serializer: S) -> Result<S::Ok, S::Error> {
    match path {
        Some(path) => serializer.serialize_some(&*path.as_ref().to_string_lossy()),
        None => serializer.serialize_none(),
    }
}

pub fn serialize_vec<S: Serializer>(paths: &[std::path::PathBuf], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(paths.iter().map(|path| path.to_string_lossy()))
}
