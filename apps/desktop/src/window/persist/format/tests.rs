use super::*;

fn leaf(cwd: &str) -> SavedNode {
    SavedNode::Leaf { cwd: Some(cwd.into()), session: None, unstarted: false }
}

fn session_leaf(id: u128, unstarted: bool) -> SavedNode {
    SavedNode::Leaf { cwd: Some("/tmp".into()), session: Some(SessionId(id)), unstarted }
}

fn state() -> State {
    let tab = SavedTab {
        root: SavedNode::Split {
            axis: Axis::Horizontal,
            ratio: 0.3,
            first: Box::new(leaf("/tmp")),
            second: Box::new(SavedNode::Split {
                axis: Axis::Vertical,
                ratio: 0.5,
                first: Box::new(SavedNode::Leaf { cwd: None, session: Some(SessionId(7)), unstarted: true }),
                second: Box::new(leaf("/")),
            }),
        },
        focused: 2,
        zoomed: false,
        collapsed: vec![1],
    };
    State::new(vec![SavedWindow {
        bounds: SavedBounds { display: None, mode: WindowMode::Windowed, x: 10., y: 20., width: 960., height: 620. },
        workspaces: vec![SavedWorkspace {
            name: "tmp".into(),
            dir: "/tmp".into(),
            tabs: vec![tab],
            active: 0,
            last_task: None,
            git: true,
            files: false,
            pinned_workflows: vec![PinnedWorkflow { id: 375441921, name: "CI".into() }],
        }],
        active: 0,
        sidebar: None,
        sidebar_width: Some(240.),
        panel_width: Some(200.),
        preview_width: Some(420.),
        preview_wrap: true,
        preview_source: true,
        show_ignored: true,
        hide_dotfiles: true,
        git_tree: true,
        git_hide_subs: true,
        git_hide_worktrees: true,
        file_search_tree: true,
        file_search_options: SavedSearchOptions {
            match_case: true,
            whole_word: false,
            regex: true,
            include: "*.ts".into(),
            exclude: String::new(),
        },
        git_graph_collapsed: true,
        git_graph_height: Some(260.),
        github_actions_heights: [Some(300.), None, None],
    }])
}

/// 每个测试一个空的临时目录。
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("runode-persist-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_saved_file_reads_back() {
    let path = temp_dir("round-trip").join("windows.json");
    assert_eq!(load_at(&path), None, "no file yet");
    write_at(&path, &state()).unwrap();
    assert_eq!(load_at(&path), Some(state()));
    // 临时文件改名换上了，没留下。
    let left: Vec<_> = fs::read_dir(path.parent().unwrap()).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    assert_eq!(left, ["windows.json"]);
}

/// 截断了的、别的版本的存档都读不了：挪到一边留着，原处空出来，之后写存档不会盖掉它。
#[test]
fn unreadable_files_are_set_aside() {
    let text = serde_json::to_string(&state()).unwrap();
    let other_version = text.replacen(&format!(r#""version":{VERSION}"#), r#""version":99"#, 1);
    assert_ne!(other_version, text);
    for (name, saved) in [("truncated", &text[..text.len() / 2]), ("version", other_version.as_str())] {
        let path = temp_dir(name).join("windows.json");
        fs::write(&path, saved).unwrap();
        assert!(read(&path).is_err(), "{name}");
        assert_eq!(load_at(&path), None, "{name}");
        assert!(!path.exists(), "{name}");
        assert_eq!(fs::read_to_string(path.with_extension("json.bad")).unwrap(), saved, "{name}");
        write_at(&path, &state()).unwrap();
        assert_eq!(fs::read_to_string(path.with_extension("json.bad")).unwrap(), saved, "{name}");
    }
}

#[test]
fn round_trips_through_json() {
    let state = state();
    let text = serde_json::to_string(&state).unwrap();
    assert_eq!(serde_json::from_str::<State>(&text).unwrap(), state);
    assert!(text.contains(r#""type":"split","axis":"horizontal""#), "{text}");
}

#[test]
fn leaves_keep_their_sessions_through_json() {
    let state = state();
    let text = serde_json::to_string(&state).unwrap();
    assert!(text.contains(r#""session":"00000000000000000000000000000007","unstarted":true"#), "{text}");
    // 没有会话、启动过的叶子不写这两项。
    assert!(text.contains(r#"{"type":"leaf","cwd":"/tmp"}"#), "{text}");
    assert_eq!(serde_json::from_str::<State>(&text).unwrap(), state);
}

fn info(id: u128, claimed: bool, exited: bool) -> SessionInfo {
    SessionInfo {
        id: SessionId(id),
        size: runode_types::grid::GridSize { cols: 80, rows: 24, cell_width_px: 8, cell_height_px: 16 },
        meta: Default::default(),
        clients: 0,
        claimed,
        exited,
        size_owner: None,
    }
}

fn window(tabs: Vec<SavedNode>) -> SavedWindow {
    let mut window = state().windows.remove(0);
    window.workspaces[0].tabs =
        tabs.into_iter().map(|root| SavedTab { root, focused: 0, zoomed: false, collapsed: Vec::new() }).collect();
    window
}

fn sessions(windows: &[SavedWindow]) -> Vec<Option<u128>> {
    let mut windows = windows.to_vec();
    windows
        .iter_mut()
        .flat_map(|window| &mut window.workspaces)
        .flat_map(|workspace| &mut workspace.tabs)
        .flat_map(|tab| tab.root.leaves_mut())
        .map(|leaf| match leaf {
            SavedNode::Leaf { session, .. } => session.map(|id| id.0),
            SavedNode::Split { .. } => unreachable!(),
        })
        .collect()
}

#[test]
fn live_sessions_are_reattached_and_the_rest_start_over() {
    let split = SavedNode::Split {
        axis: Axis::Vertical,
        ratio: 0.5,
        first: Box::new(session_leaf(1, false)),
        second: Box::new(session_leaf(2, false)),
    };
    let windows = vec![window(vec![split, session_leaf(3, false), leaf("/")]), window(vec![session_leaf(4, false)])];
    // 2 已经不在宿主里了；4 有别的界面连着。
    let live = [info(1, false, false), info(3, false, false), info(4, true, false), info(9, false, false)];
    let plan = plan_restore(windows, &live);
    assert_eq!(sessions(&plan.windows), [Some(1), None, Some(3), None, None]);
    assert!(plan.end.is_empty());
}

#[test]
fn a_session_saved_twice_is_reattached_only_the_first_time() {
    let windows =
        vec![window(vec![session_leaf(1, false), session_leaf(1, false)]), window(vec![session_leaf(1, false)])];
    let plan = plan_restore(windows, &[info(1, false, false)]);
    assert_eq!(sessions(&plan.windows), [Some(1), None, None]);
    assert!(plan.end.is_empty());
}

#[test]
fn exited_and_unstarted_sessions_start_over_and_are_ended() {
    let windows = vec![window(vec![
        session_leaf(1, false),
        session_leaf(2, true),
        session_leaf(2, true),
        session_leaf(3, true),
        session_leaf(4, false),
    ])];
    // 1 的 shell 退出了；2 上次没启动过；3 没启动过但别的界面连着，不归这里结束；4 不在了。
    let live = [info(1, false, true), info(2, false, false), info(3, true, false)];
    let plan = plan_restore(windows, &live);
    assert_eq!(sessions(&plan.windows), [None; 5]);
    assert_eq!(plan.end, [SessionId(1), SessionId(2)]);
}

/// 只结束存档里记着的、shell 已经退出又没人连着的会话；存档里没记着的、有界面连着的、还在跑的不动。
#[test]
fn only_saved_exited_sessions_are_ended() {
    let windows = vec![window(vec![session_leaf(1, false), session_leaf(2, false)])];
    let live = [
        info(1, false, true),
        info(2, false, false),
        info(3, false, true),
        info(4, true, true),
        info(5, false, false),
        info(3, false, true),
    ];
    let plan = plan_restore(windows, &live);
    assert_eq!(sessions(&plan.windows), [None, Some(2)]);
    assert_eq!(plan.end, [SessionId(1)]);
    // 存档里没记着的（3 可能是看板留着的）不结束。
    assert!(plan_restore(Vec::new(), &live).end.is_empty());
}

/// 存档说没启动过，宿主那边却已经有前台程序（标签切过去启动了 shell，存档没来得及重写）：
/// 以宿主为准接上，不结束它。
#[test]
fn a_stale_unstarted_mark_does_not_end_a_running_shell() {
    let windows = vec![window(vec![session_leaf(1, true)])];
    let mut running = info(1, false, false);
    running.meta.foreground = Some("zsh".into());
    let plan = plan_restore(windows, &[running]);
    assert_eq!(sessions(&plan.windows), [Some(1)]);
    assert!(plan.end.is_empty());
}

#[test]
fn nothing_is_reattached_when_the_host_is_new() {
    let windows = vec![window(vec![session_leaf(1, false), session_leaf(2, true)])];
    let plan = plan_restore(windows, &[]);
    assert_eq!(sessions(&plan.windows), [None, None]);
    assert!(plan.end.is_empty());
}

fn held(workspaces: Vec<(&str, Option<&str>, Vec<u128>)>) -> Vec<WindowLayout> {
    use runode_protocol::{PaneLayout, PaneRect, TabLayout, WorkspaceLayout};
    let rect = PaneRect { x: 0, y: 0, width: PaneRect::EXTENT, height: PaneRect::EXTENT };
    let tab = |id| TabLayout {
        index: 1,
        active: true,
        panes: vec![PaneLayout { index: 1, id: SessionId(id), rect, focused: true }],
    };
    let workspaces = workspaces
        .into_iter()
        .map(|(dir, name, ids)| WorkspaceLayout {
            index: 1,
            name: name.map(Into::into),
            dir: Some(dir.into()),
            active: false,
            tabs: ids.into_iter().map(tab).collect(),
        })
        .collect();
    vec![WindowLayout { index: 1, front: true, workspaces }]
}

/// 没开 app 时在已有的工作区里开的终端接在它后面，改的名字跟上；新建的工作区加在窗口末尾，没名字的
/// 按目录取；存档里有的、退出了的、不在了的、别的界面连着的会话不接。
#[test]
fn what_the_host_did_without_the_app_is_adopted() {
    let mut windows = vec![window(vec![session_leaf(1, false)])];
    let dir = windows[0].workspaces[0].dir.to_string_lossy().into_owned();
    let held = held(vec![
        ("/new", None, vec![4, 5, 6]),
        (&dir, Some("renamed"), vec![1, 2, 3]),
        ("/gone", Some("gone"), vec![9]),
    ]);
    // 3 的 shell 退出了，5 不在了，6 有别的界面连着。
    let live = [
        info(1, false, false),
        info(2, false, false),
        info(3, false, true),
        info(4, false, false),
        info(6, true, false),
    ];
    adopt(&mut windows, &held, &live, |dir| format!("named {}", dir.display()));
    let [window] = windows.as_slice() else { panic!("one window") };
    let names: Vec<_> = window.workspaces.iter().map(|workspace| workspace.name.as_str()).collect();
    // 存档里的工作区挪了位置也认得出；会话都没了的工作区照样建。
    assert_eq!(names, ["renamed", "named /new", "gone"]);
    assert_eq!(sessions(&windows), [Some(1), Some(2), Some(4)]);
}

/// 没有存档时新开一个窗口放宿主那边的工作区；目录对不上的同一个序号的工作区不是同一个。
#[test]
fn adopted_workspaces_get_a_window_and_match_by_dir() {
    let mut windows = Vec::new();
    adopt(&mut windows, &held(vec![("/a", Some("a"), vec![1])]), &[info(1, false, false)], |_| unreachable!());
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].workspaces[0].name, "a");

    let mut windows = vec![window(vec![leaf("/")])];
    let saved = windows[0].workspaces[0].name.clone();
    adopt(&mut windows, &held(vec![("/elsewhere", Some("b"), vec![1])]), &[info(1, false, false)], |_| unreachable!());
    let names: Vec<_> = windows[0].workspaces.iter().map(|workspace| workspace.name.clone()).collect();
    assert_eq!(names, [saved, "b".into()]);
}

#[test]
fn start_dir_falls_back_to_the_workspace_without_touching_the_disk() {
    let missing = Path::new("/nonexistent/runode-test");
    assert_eq!(start_dir(Some(missing), Path::new("/tmp")), PathBuf::from(missing));
    assert_eq!(start_dir(None, missing), PathBuf::from(missing));
}
