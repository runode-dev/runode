//! 手机端测试读的消息样例 `tests/fixtures/messages.json`：按名字存着各条消息（以及消息里带的
//! `TermSettings`、`IntegrationMode`）由这里的类型序列化出来的 JSON，手机据此核对自己的编解码。
//! 样例由下面的 `samples` 生成，改了消息格式后设 `RUNODE_BLESS=1` 跑这个测试重新写出样例，
//! 再跑手机端的测试；不设时样例和生成的结果对不上就失败，多一条少一条也算。

use std::{collections::BTreeMap, path::PathBuf};

use runode_protocol::{
    AttachMode, BuildId, Caps, ClientKind, ClientMsg, FinishedCommand, GoodbyeReason, HostMsg, PaneLayout, PaneRect,
    Placement, ProjectTask, SessionId, SessionInfo, TabLayout, TaskSource, TaskSourceKind, WindowLayout,
    WorkspaceLayout,
    git::{
        GitBranch, GitFile, GitFileDiff, GitFileStatus, GitHunk, GitLine, GitLineKind, GitOperation, GitRequest,
        GitStatus,
    },
};
use runode_shared_types::{
    agent::{Agent, AgentKind, AgentState},
    color::{Rgb, TerminalColor},
    grid::GridSize,
    session::{DriveAction, Driver, SessionMeta},
    settings::{CursorStyle, OptionAsAlt, TermSettings},
    shell::{IntegrationMode, Shell, ShellNames},
};
use serde::Serialize;
use serde_json::{Map, Value};

const ID: SessionId = SessionId(0x0123_4567_89ab_cdef_0011_2233_4455_6677);

const NOTE: &str = "由 runode_protocol 的测试 message_fixture 用宿主的 runode_protocol / runode_shared_types（Rust）\
                    按 serde 序列化生成，手机端的测试据此核对 Swift 这边的编解码。不要手改：改了消息格式后在仓库根跑 \
                    `RUNODE_BLESS=1 cargo test -p runode-protocol --test message_fixture` 重新生成。";

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/messages.json")
}

fn size() -> GridSize {
    GridSize { cols: 80, rows: 24, cell_width_px: 16, cell_height_px: 32 }
}

fn meta() -> SessionMeta {
    SessionMeta {
        title: Some("修 bug".into()),
        fallback_title: Some("zsh".into()),
        agent: Some(Agent { kind: AgentKind::Claude, state: AgentState::Blocked }),
        cwd: Some("/Users/ethan/中文".into()),
        prompt_cwd: Some("/tmp".into()),
        foreground_is_shell: false,
        shell_path: Some("/usr/bin:/bin".into()),
        shell_names: ShellNames { aliases: vec!["ll".into()], ..ShellNames::default() }.into(),
        foreground: Some("claude".into()),
        driver: Some(Driver { by: None, action: DriveAction::Paste, at_ms: 1 }),
    }
}

fn custom_settings() -> TermSettings {
    TermSettings {
        cursor_color: Some(TerminalColor::Rgb(Rgb(1, 2, 3))),
        cursor_text: Some(TerminalColor::CellBackground),
        cursor_blink: Some(false),
        cursor_style: CursorStyle::BlockHollow,
        option_as_alt: OptionAsAlt::Left,
        palette: vec![(1, Rgb(255, 0, 0)), (200, Rgb(0, 0, 255))],
        ..TermSettings::default()
    }
}

fn layout() -> Vec<WindowLayout> {
    let pane = |index, id, x, width, focused| PaneLayout {
        index,
        id: SessionId(id),
        rect: PaneRect { x, y: 0, width, height: 1000 },
        focused,
    };
    vec![WindowLayout {
        index: 1,
        front: true,
        workspaces: vec![
            WorkspaceLayout {
                index: 1,
                name: Some("runode".into()),
                dir: Some("/Users/ethan/dev/runode".into()),
                active: true,
                tabs: vec![
                    TabLayout { index: 1, active: false, panes: vec![pane(1, ID.0, 0, 1000, true)] },
                    TabLayout {
                        index: 2,
                        active: true,
                        panes: vec![
                            pane(1, 0x1111_1111_1111_1111_1111_1111_1111_1111, 0, 500, false),
                            pane(2, 0x2222_2222_2222_2222_2222_2222_2222_2222, 500, 500, true),
                        ],
                    },
                ],
            },
            WorkspaceLayout { index: 2, name: Some("blog".into()), dir: None, active: false, tabs: vec![] },
        ],
    }]
}

fn git_file(path: &str, old_path: Option<&str>, status: GitFileStatus, added: u32, removed: u32) -> GitFile {
    GitFile {
        path: path.into(),
        old_path: old_path.map(Into::into),
        status,
        added,
        removed,
        binary: false,
        gitlink: false,
    }
}

fn project_tasks() -> Vec<TaskSource> {
    let task = |name: &str, command: &str, description: Option<&str>| ProjectTask {
        name: name.into(),
        command: command.into(),
        description: description.map(Into::into),
    };
    vec![
        TaskSource {
            kind: TaskSourceKind::Makefile,
            file: "/Users/ethan/dev/app/Makefile".into(),
            tasks: vec![task("build", "make -C .. build", Some("编译全部")), task("test", "make -C .. test", None)],
            truncated: false,
        },
        TaskSource {
            kind: TaskSourceKind::PackageJson,
            file: "/Users/ethan/dev/app/web/package.json".into(),
            tasks: vec![task("dev", "pnpm run dev", Some("vite"))],
            truncated: true,
        },
    ]
}

/// 样例里的每一条，按名字。
fn samples() -> BTreeMap<&'static str, Value> {
    fn json(value: impl Serialize) -> Value {
        serde_json::to_value(value).unwrap()
    }
    fn git(req: u32, request: GitRequest) -> Value {
        json(ClientMsg::Git { req, id: ID, request })
    }

    let client = [
        (
            "hello",
            ClientMsg::Hello {
                protocol: 4,
                build: BuildId("ios-0.1.0+1".into()),
                client: ClientKind::Mobile,
                caps: Caps { snapshot: false, vt_replay: true },
                session: None,
                device: Some("Ethan 的 iPhone".into()),
            },
        ),
        ("list", ClientMsg::ListSessions),
        ("layout_request", ClientMsg::Layout { req: 0 }),
        ("open", ClientMsg::Open { req: 7, placement: Placement::Tab, near: None, cwd: None, focus: false }),
        (
            "open_workspace",
            ClientMsg::OpenWorkspace { req: 8, dir: "/Users/ethan/dev/中文".into(), focus: false, name: None },
        ),
        (
            "open_workspace_named",
            ClientMsg::OpenWorkspace {
                req: 8,
                dir: "/Users/ethan/dev/中文".into(),
                focus: false,
                name: Some("后端".into()),
            },
        ),
        ("rename_workspace", ClientMsg::RenameWorkspace { req: 9, window: 1, workspace: 2, name: "前端".into() }),
        ("list_dirs", ClientMsg::ListDirs { req: 5, path: Some("/Users/ethan".into()) }),
        ("list_dirs_home", ClientMsg::ListDirs { req: 5, path: None }),
        ("list_project_tasks", ClientMsg::ListProjectTasks { req: 5, dir: "/Users/ethan/dev/中文".into() }),
        (
            "spawn",
            ClientMsg::Spawn {
                req: 3,
                size: size(),
                cwd: None,
                integration: IntegrationMode::Detect,
                start: true,
                shell: None,
                settings: None,
            },
        ),
        ("attach_vt", ClientMsg::Attach { id: ID, size: None, mode: AttachMode::VtReplay }),
        ("attach_meta", ClientMsg::Attach { id: ID, size: None, mode: AttachMode::MetaOnly }),
        ("attach_size", ClientMsg::Attach { id: ID, size: Some(size()), mode: AttachMode::VtReplay }),
        ("detach", ClientMsg::Detach { id: ID }),
        ("resize", ClientMsg::Resize { id: ID, size: size() }),
        ("focus", ClientMsg::Focus { id: ID, focused: true }),
        ("kill", ClientMsg::Kill { id: ID }),
        ("read_screen", ClientMsg::ReadScreen { id: ID, lines: Some(8), command: None }),
        ("read_screen_command", ClientMsg::ReadScreen { id: ID, lines: None, command: Some(1) }),
        (
            "send_keys",
            ClientMsg::SendKeys { req: 5, id: ID, keys: ["1", "enter", "esc", "up"].map(String::from).to_vec() },
        ),
        ("paste", ClientMsg::Paste { req: 6, id: ID, text: "继续，用方案 2\n".into() }),
    ];

    let host = [
        (
            "welcome",
            HostMsg::Welcome {
                protocol: 4,
                build: BuildId("0.1.0+abc".into()),
                host_pid: 123,
                snapshot_format: 1,
                standalone: true,
                handoff: 1,
            },
        ),
        ("incompatible", HostMsg::Incompatible { protocol: 5, build: BuildId("x".into()), reason: "old".into() }),
        (
            "session_list",
            HostMsg::SessionList {
                sessions: vec![SessionInfo {
                    id: ID,
                    size: size(),
                    meta: meta(),
                    clients: 2,
                    claimed: true,
                    exited: false,
                    size_owner: Some("Ethan 的 MacBook".into()),
                }],
            },
        ),
        ("spawned", HostMsg::Spawned { req: 3, id: ID }),
        (
            "attached",
            HostMsg::Attached {
                id: ID,
                channel: 7,
                size: size(),
                mode: AttachMode::VtReplay,
                meta: meta(),
                settings: Some(TermSettings::default()),
            },
        ),
        ("snapshot_end", HostMsg::SnapshotEnd { id: ID }),
        ("resized", HostMsg::Resized { id: ID, size: size() }),
        ("theme_applied", HostMsg::ThemeApplied { id: ID, settings: custom_settings() }),
        ("meta", HostMsg::Meta { id: ID, meta: SessionMeta::default() }),
        (
            "command_finished",
            HostMsg::CommandFinished {
                id: ID,
                command: FinishedCommand { cmd: "ls".into(), cwd: None, exit: Some(0), ts: 5 },
            },
        ),
        ("resync", HostMsg::Resync { id: ID, reason: "slow".into() }),
        ("exited", HostMsg::Exited { id: ID, status: Some(1) }),
        ("exited_none", HostMsg::Exited { id: ID, status: None }),
        ("bell", HostMsg::Bell { id: ID }),
        ("opened", HostMsg::Opened { req: 7, id: ID }),
        ("done", HostMsg::Done { req: 9 }),
        (
            "git_status",
            HostMsg::GitStatus {
                req: 1,
                id: ID,
                status: Some(GitStatus {
                    root: "/Users/me/app".into(),
                    branch: Some("main".into()),
                    head: Some("1a2b3c4".into()),
                    upstream: Some("origin/main".into()),
                    ahead: 2,
                    behind: 1,
                    has_remote: true,
                    operation: Some(GitOperation::CherryPick),
                    staged: vec![git_file("src/new.rs", Some("src/old.rs"), GitFileStatus::Renamed, 3, 1)],
                    unstaged: vec![git_file("notes.txt", None, GitFileStatus::Untracked, 1, 0)],
                }),
            },
        ),
        ("git_status_none", HostMsg::GitStatus { req: 2, id: ID, status: None }),
        (
            "git_diff",
            HostMsg::GitDiff {
                req: 3,
                id: ID,
                diff: Some(GitFileDiff {
                    file: git_file("a.txt", None, GitFileStatus::Modified, 1, 1),
                    hunks: vec![GitHunk {
                        header: "@@ -1,2 +1,2 @@ fn main".into(),
                        lines: vec![
                            GitLine { kind: GitLineKind::Context, old: Some(1), new: Some(1), text: "keep".into() },
                            GitLine { kind: GitLineKind::Removed, old: Some(2), new: None, text: "old".into() },
                            GitLine { kind: GitLineKind::Added, old: None, new: Some(2), text: "new".into() },
                        ],
                    }],
                    truncated: false,
                }),
            },
        ),
        (
            "git_branches",
            HostMsg::GitBranches {
                req: 4,
                id: ID,
                branches: vec![
                    GitBranch {
                        name: "main".into(),
                        remote: false,
                        current: true,
                        upstream: Some("origin/main".into()),
                        subject: "init".into(),
                        date: "2 days ago".into(),
                    },
                    GitBranch {
                        name: "origin/feat".into(),
                        remote: true,
                        current: false,
                        upstream: None,
                        subject: "wip".into(),
                        date: "1 hour ago".into(),
                    },
                ],
            },
        ),
        ("screen_text", HostMsg::ScreenText { id: ID, text: "a\n".into(), truncated: false }),
        ("layout", HostMsg::Layout { req: 0, windows: layout() }),
        (
            "dirs",
            HostMsg::Dirs {
                req: 5,
                path: "/Users/ethan".into(),
                dirs: [".config", "dev", "中文"].map(String::from).to_vec(),
                truncated: true,
            },
        ),
        (
            "project_tasks",
            HostMsg::ProjectTasks { req: 6, dir: "/Users/ethan/dev/app/web".into(), sources: project_tasks() },
        ),
        ("error", HostMsg::Error { req: None, id: Some(ID), message: "no session".into() }),
        ("goodbye", HostMsg::Goodbye { reason: GoodbyeReason::Error { message: "bye".into() } }),
        ("goodbye_handoff", HostMsg::Goodbye { reason: GoodbyeReason::Handoff }),
        ("size_owner", HostMsg::SizeOwner { id: ID, mine: false, owner: Some("Ethan 的 MacBook".into()) }),
    ];

    let mut samples: BTreeMap<_, _> = client.into_iter().map(|(name, msg)| (name, json(msg))).collect();
    samples.extend(host.into_iter().map(|(name, msg)| (name, json(msg))));
    samples.extend([
        ("git_status_request", git(1, GitRequest::Status)),
        ("git_diff_request", git(2, GitRequest::Diff { path: "src/a.rs".into(), staged: true })),
        ("git_unstage_request", git(3, GitRequest::Unstage { paths: vec!["b.rs".into(), "a.rs".into()] })),
        ("git_commit_request", git(4, GitRequest::Commit { message: "修好了".into(), stage_all: true })),
        ("git_checkout_request", git(5, GitRequest::Checkout { branch: "origin/feat".into(), remote: true })),
        ("settings_default", json(TermSettings::default())),
        ("settings_custom", json(custom_settings())),
        ("integration_force", json(IntegrationMode::Force(Shell::Zsh))),
    ]);
    samples
}

/// 对象的键按字节序排好：`serde_json` 开着 `preserve_order` 时（工作区里别的 crate 会打开它）
/// 照字段的先后写，写回的样例要和开没开它无关。
fn sorted(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<_> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().map(|(key, value)| (key, sorted(value))).collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

#[test]
fn fixture_matches_the_serialized_messages() {
    let samples = samples();
    let path = fixture_path();

    if std::env::var_os("RUNODE_BLESS").is_some_and(|value| value == "1") {
        let messages: Map<_, _> = samples.into_iter().map(|(name, value)| (name.to_owned(), value)).collect();
        let root = sorted(serde_json::json!({ "_说明": NOTE, "messages": messages }));
        let text = serde_json::to_string_pretty(&root).unwrap() + "\n";
        std::fs::write(&path, text).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        return;
    }

    let text = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    let root: Value = serde_json::from_str(&text).unwrap();
    let fixture = root["messages"].as_object().expect("messages 是个对象");

    let expected: Vec<_> = samples.keys().copied().collect();
    let mut actual: Vec<_> = fixture.keys().map(String::as_str).collect();
    actual.sort_unstable();
    assert_eq!(actual, expected, "样例里的名字和生成的对不上，设 RUNODE_BLESS=1 重新生成");

    for (name, value) in &samples {
        assert_eq!(&fixture[*name], value, "{name} 和生成的对不上，设 RUNODE_BLESS=1 重新生成");
    }
}
