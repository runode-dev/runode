//! 列一个目录里能跑的项目命令（`ClientMsg::ListProjectTasks`）：往上找最近的 Makefile 和 package.json，
//! 拼好在这个目录里能直接跑的命令行；列不了时回 `Error`，连接照旧。

mod common;

use std::{fs, path::PathBuf};

use common::{Peer, listen, temp_dir};
use runode_host::{ClientMsg, HostMsg};
use runode_protocol::{ProjectTask, TaskSource, TaskSourceKind};

/// 列 `dir` 的命令。开发机上 runode 根目录里自己加的通用命令也会列出来，和这里的测试无关，去掉。
fn list(peer: &mut Peer, req: u32, dir: PathBuf) -> HostMsg {
    peer.send(&ClientMsg::ListProjectTasks { req, dir });
    let mut reply = peer.reply();
    if let HostMsg::ProjectTasks { sources, .. } = &mut reply {
        sources.retain(|source| !matches!(source.kind, TaskSourceKind::Custom | TaskSourceKind::Global));
    }
    reply
}

fn task(name: &str, command: &str, description: Option<&str>) -> ProjectTask {
    ProjectTask { name: name.into(), command: command.into(), description: description.map(String::from) }
}

/// Makefile 在上两级、package.json 在上一级：make 带 `-C`，scripts 按上面的锁文件用 pnpm 跑，按文件里的
/// 先后列，值不是字符串的跳过。
#[test]
fn nearest_makefile_and_package_json_are_listed() {
    let dir = temp_dir("tasks");
    let (_host, socket) = listen(&dir);
    let root = dir.join("repo");
    let src = root.join("web").join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(root.join("Makefile"), ".PHONY: build\nbuild: ## 编译全部\n\tcargo build\ntest:\n\tcargo test\n")
        .unwrap();
    fs::write(root.join("pnpm-lock.yaml"), "").unwrap();
    fs::write(root.join("web").join("package.json"), r#"{"scripts":{"dev":"vite","build:prod":"vite build","n":1}}"#)
        .unwrap();

    let mut peer = Peer::hello(&socket, false);
    let reply = list(&mut peer, 1, src.clone());
    let expected = HostMsg::ProjectTasks {
        req: 1,
        dir: src,
        sources: vec![
            TaskSource {
                kind: TaskSourceKind::Makefile,
                file: root.join("Makefile"),
                project: None,
                tasks: vec![
                    task("build", "make -C ../.. build", Some("编译全部")),
                    task("test", "make -C ../.. test", None),
                ],
                truncated: false,
            },
            TaskSource {
                kind: TaskSourceKind::PackageJson,
                file: root.join("web").join("package.json"),
                project: None,
                tasks: vec![
                    task("dev", "pnpm run dev", Some("vite")),
                    task("build:prod", "pnpm run build:prod", Some("vite build")),
                ],
                truncated: false,
            },
        ],
    };
    assert_eq!(reply, expected);
}

/// `packageManager` 字段比锁文件说了算；名字里有空格的加引号。
#[test]
fn the_declared_package_manager_wins() {
    let dir = temp_dir("taskspm");
    let (_host, socket) = listen(&dir);
    let app = dir.join("app");
    fs::create_dir_all(&app).unwrap();
    fs::write(app.join("package-lock.json"), "{}").unwrap();
    fs::write(app.join("package.json"), r#"{"packageManager":"yarn@4.1.0","scripts":{"say hi":"echo hi"}}"#).unwrap();

    let mut peer = Peer::hello(&socket, false);
    match list(&mut peer, 2, app) {
        HostMsg::ProjectTasks { req: 2, sources, .. } => {
            assert_eq!(sources.len(), 1);
            assert_eq!(sources[0].tasks, [task("say hi", "yarn run 'say hi'", Some("echo hi"))]);
        }
        other => panic!("expected project tasks, got {other:?}"),
    }
}

/// 拼不成安全命令行的 scripts 不列：以 `-` 开头的会被当成选项，含 `\` 的在 fish 的单引号里会被转义，
/// 含换行的会拆成两条命令。
#[test]
fn scripts_that_cannot_be_quoted_are_skipped() {
    let dir = temp_dir("tasksquote");
    let (_host, socket) = listen(&dir);
    let app = dir.join("app");
    fs::create_dir_all(&app).unwrap();
    let scripts = r#"{"scripts":{"-x":"a","back\\slash":"b","two\nlines":"c","it's":"d"}}"#;
    fs::write(app.join("package.json"), scripts).unwrap();

    let mut peer = Peer::hello(&socket, false);
    match list(&mut peer, 6, app) {
        HostMsg::ProjectTasks { sources, .. } => {
            assert_eq!(sources[0].tasks, [task("it's", r"npm run 'it'\''s'", Some("d"))]);
        }
        other => panic!("expected project tasks, got {other:?}"),
    }
}

/// 不是绝对路径、不是目录时回带着请求编号的 `Error`，连接照旧能用。
#[test]
fn bad_directories_are_errors() {
    let dir = temp_dir("taskserr");
    let (_host, socket) = listen(&dir);
    let mut peer = Peer::hello(&socket, false);
    assert!(matches!(list(&mut peer, 3, "relative".into()), HostMsg::Error { req: Some(3), .. }));
    assert!(matches!(list(&mut peer, 4, dir.join("missing")), HostMsg::Error { req: Some(4), .. }));
    fs::write(dir.join("Makefile"), "all:\n").unwrap();
    match list(&mut peer, 5, dir) {
        HostMsg::ProjectTasks { req: 5, sources, .. } => assert_eq!(sources[0].tasks, [task("all", "make all", None)]),
        other => panic!("expected project tasks, got {other:?}"),
    }
}
