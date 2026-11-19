//! 按文件挑语言服务器，路径和 URI 的换算。

use std::path::Path;

use runode_lsp::{servers, uri};

#[test]
fn servers_are_picked_by_extension() {
    let rust = servers::for_path(Path::new("/p/src/Main.RS")).unwrap();
    assert_eq!((rust.program, rust.language_id), ("rust-analyzer", "rust"));
    let tsx = servers::for_path(Path::new("a.tsx")).unwrap();
    assert_eq!(
        (tsx.program, tsx.args, tsx.language_id),
        ("typescript-language-server", &["--stdio"][..], "typescriptreact")
    );
    assert_eq!(servers::for_path(Path::new("notes.txt")), None);
    assert_eq!(servers::for_path(Path::new("Makefile")), None);
}

#[test]
fn paths_round_trip_through_uris() {
    let path = Path::new("/p/我的 项目/a#b%c.rs");
    let uri = uri::from_path(path).unwrap();
    assert_eq!(uri.as_str(), "file:///p/%E6%88%91%E7%9A%84%20%E9%A1%B9%E7%9B%AE/a%23b%25c.rs");
    assert_eq!(uri::to_path(&uri).unwrap(), path);
    let windows = Path::new(r"C:\p\a b.rs");
    let uri = uri::from_path(windows).unwrap();
    assert_eq!(uri.as_str(), "file:///C:/p/a%20b.rs");
    assert_eq!(uri::to_path(&uri).unwrap(), windows);
    let colon = Path::new("/a:b/c.rs");
    assert_eq!(uri::to_path(&uri::from_path(colon).unwrap()).unwrap(), colon);
    assert_eq!(uri::to_path(&"https://example.com/a.rs".parse().unwrap()), None);
    assert_eq!(uri::to_path(&"file://host/a.rs".parse().unwrap()), None);
}
