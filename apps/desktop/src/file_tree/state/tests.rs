use super::*;
use runode_git::FileStatus;

const ALL: TreeFilter = TreeFilter { ignored: true, dotfiles: true };
const NO_IGNORED: TreeFilter = TreeFilter { ignored: false, dotfiles: true };

fn snapshot() -> git::Repos {
    git::Repos::new(git::Snapshot {
        root: "/repo".into(),
        git_dir: "/repo/.git".into(),
        prefix: PathBuf::new(),
        kind: git::RepoKind::Main,
        staged: Vec::new(),
        unstaged: Vec::new(),
        statuses: HashMap::from([
            ("src/a/b.rs".into(), FileStatus::Modified),
            ("new.txt".into(), FileStatus::Untracked),
            ("docs/new/a.md".into(), FileStatus::Untracked),
            ("docs/old.md".into(), FileStatus::Deleted),
            ("src/c.rs".into(), FileStatus::Added),
        ]),
        ignored: HashSet::from(["target".into()]),
        info: Default::default(),
    })
}

fn dir(name: &str) -> DirEntry {
    DirEntry { name: name.into(), is_dir: true }
}

fn file(name: &str) -> DirEntry {
    DirEntry { name: name.into(), is_dir: false }
}

fn names(state: &TreeState) -> Vec<(String, usize)> {
    state.rows.iter().map(|row| (row.name.to_string(), row.depth)).collect()
}

#[test]
fn decorates_paths_by_status() {
    let git = snapshot();
    let changes = GitChanges::new(Some(&git));
    let decorator = Decorator::new(Some(&git), &changes);
    let of = |path: &str, is_dir| decorator.of(Path::new(path), is_dir);
    assert_eq!(of("/repo/src/a/b.rs", false), Decoration::Status(FileStatus::Modified));
    // 目录按里面最要紧的改动着色：有修改算修改，只有新文件的才算新增或未跟踪。
    assert_eq!(of("/repo/src", true), Decoration::ContainsChanges(FileStatus::Modified));
    assert_eq!(of("/repo/src/a", true), Decoration::ContainsChanges(FileStatus::Modified));
    assert_eq!(of("/repo/docs/new", true), Decoration::ContainsChanges(FileStatus::Untracked));
    assert_eq!(of("/repo/docs", true), Decoration::ContainsChanges(FileStatus::Modified));
    assert_eq!(of("/repo/lib", true), Decoration::None);
    assert_eq!(of("/repo/new.txt", false), Decoration::Status(FileStatus::Untracked));
    assert_eq!(of("/repo/target", true), Decoration::Ignored);
    assert_eq!(of("/repo/target/debug/x", false), Decoration::Ignored);
    assert_eq!(of("/elsewhere/x", false), Decoration::None);
}

#[test]
fn decorates_paths_inside_sub_repositories() {
    let mut git = snapshot();
    git.main.statuses.insert("libs/lib".into(), FileStatus::Modified);
    git.subs.push(git::Snapshot {
        root: "/repo/libs/lib".into(),
        git_dir: "/repo/.git/modules/libs/lib".into(),
        prefix: "libs/lib".into(),
        kind: git::RepoKind::Submodule,
        staged: Vec::new(),
        unstaged: Vec::new(),
        statuses: HashMap::from([("src/x.rs".into(), FileStatus::Added)]),
        ignored: HashSet::from(["build".into()]),
        info: Default::default(),
    });
    let changes = GitChanges::new(Some(&git));
    let decorator = Decorator::new(Some(&git), &changes);
    let of = |path: &str, is_dir| decorator.of(Path::new(path), is_dir);
    // 子模块里的文件按子模块自己的状态，忽略也按它自己的规则。
    assert_eq!(of("/repo/libs/lib/src/x.rs", false), Decoration::Status(FileStatus::Added));
    assert_eq!(of("/repo/libs/lib/build/out", false), Decoration::Ignored);
    // 子模块那个目录：提交号变了算修改，和里面的新增并在一起还是修改。
    assert_eq!(of("/repo/libs/lib", true), Decoration::ContainsChanges(FileStatus::Modified));
    assert_eq!(of("/repo/libs/lib/src", true), Decoration::ContainsChanges(FileStatus::Added));
    assert_eq!(of("/repo/libs", true), Decoration::ContainsChanges(FileStatus::Modified));
    assert_eq!(of("/repo/src/a/b.rs", false), Decoration::Status(FileStatus::Modified));
}

#[test]
fn flattens_expanded_dirs() {
    let mut state = TreeState::default();
    let root = Path::new("/p");
    state.listings.insert(root.into(), vec![dir("src"), file("README.md")]);
    state.listings.insert("/p/src".into(), vec![file("main.rs")]);
    state.rebuild_rows(root, ALL);
    assert_eq!(state.rows.len(), 2);
    state.expanded.insert("/p/src".into());
    state.rebuild_rows(root, ALL);
    assert_eq!(names(&state), [("src".into(), 0), ("main.rs".into(), 1), ("README.md".into(), 0)]);
}

/// 名字不是合法 UTF-8 时只在显示时换成替换字符，行的路径仍是原样的字节，打开、改名、删除不会落到
/// 名字恰好是替换字符的那个文件上。
#[cfg(unix)]
#[test]
fn keeps_raw_names_in_row_paths() {
    use std::os::unix::ffi::OsStrExt as _;
    let raw = std::ffi::OsStr::from_bytes(b"a\xff");
    let mut state = TreeState::default();
    let root = Path::new("/p");
    state.listings.insert(root.into(), vec![DirEntry { name: raw.into(), is_dir: false }, file("a\u{FFFD}")]);
    state.rebuild_rows(root, ALL);
    assert_eq!(state.rows[0].path, root.join(raw));
    assert_eq!(state.rows[0].name, "a\u{FFFD}");
    assert_ne!(state.rows[0].path, state.rows[1].path);
}

#[test]
fn compacts_single_child_dirs() {
    let mut state = TreeState::default();
    let root = Path::new("/p");
    state.listings.insert(root.into(), vec![dir("crates"), file("Cargo.toml")]);
    state.listings.insert("/p/crates".into(), vec![dir("app")]);
    state.listings.insert("/p/crates/app".into(), vec![dir("src"), file("Cargo.toml")]);
    state.rebuild_rows(root, ALL);
    assert_eq!(names(&state), [("crates / app".into(), 0), ("Cargo.toml".into(), 0)]);
    assert_eq!(state.rows[0].path, PathBuf::from("/p/crates/app"));
    state.expanded.insert("/p/crates/app".into());
    state.rebuild_rows(root, ALL);
    assert_eq!(names(&state)[1..3], [("src".into(), 1), ("Cargo.toml".into(), 1)]);
}

#[test]
fn hides_ignored_entries() {
    let mut state = TreeState { git: Some(Arc::new(snapshot())), ..Default::default() };
    let root = Path::new("/repo");
    state.listings.insert(root.into(), vec![dir("src"), dir("target"), file("new.txt")]);
    state.rebuild_rows(root, NO_IGNORED);
    assert_eq!(names(&state), [("src".into(), 0), ("new.txt".into(), 0)]);
    state.rebuild_rows(root, ALL);
    assert_eq!(state.rows.len(), 3);

    // 只装着被忽略目录的目录不和它并成一行。
    let mut git = snapshot();
    git.main.ignored.insert("out/target".into());
    state.git = Some(Arc::new(git));
    state.listings.insert(root.into(), vec![dir("out")]);
    state.listings.insert("/repo/out".into(), vec![dir("target")]);
    state.rebuild_rows(root, NO_IGNORED);
    assert_eq!(names(&state), [("out".into(), 0)]);
    state.rebuild_rows(root, ALL);
    assert_eq!(names(&state), [("out".into(), 0)]);
    assert_eq!(state.rows[0].decoration, Decoration::None);
}

/// 改动表按 git 快照留着一份，换了一份快照就重算，着色不停在旧的状态上。
#[test]
fn recomputes_changes_when_git_changes() {
    let mut state = TreeState { git: Some(Arc::new(snapshot())), ..Default::default() };
    let root = Path::new("/repo");
    state.listings.insert(root.into(), vec![file("new.txt")]);
    state.rebuild_rows(root, ALL);
    assert_eq!(state.rows[0].decoration, Decoration::Status(FileStatus::Untracked));
    let mut clean = snapshot();
    clean.main.statuses.clear();
    state.git = Some(Arc::new(clean));
    state.rebuild_rows(root, ALL);
    assert_eq!(state.rows[0].decoration, Decoration::None);
    state.git = None;
    state.rebuild_rows(root, ALL);
    assert_eq!(state.rows[0].decoration, Decoration::None);
}

#[test]
fn hides_dotfiles() {
    let mut state = TreeState::default();
    let root = Path::new("/p");
    state.listings.insert(root.into(), vec![dir(".github"), file(".env"), file("a")]);
    state.rebuild_rows(root, TreeFilter { ignored: true, dotfiles: false });
    assert_eq!(names(&state), [("a".into(), 0)]);
    state.rebuild_rows(root, ALL);
    assert_eq!(state.rows.len(), 3);

    // 只装着一个点开头子目录的目录不和它并成一行。
    state.listings.insert(root.into(), vec![dir("visible")]);
    state.listings.insert("/p/visible".into(), vec![dir(".hidden")]);
    state.rebuild_rows(root, TreeFilter { ignored: true, dotfiles: false });
    assert_eq!(names(&state), [("visible".into(), 0)]);
    state.rebuild_rows(root, ALL);
    assert_eq!(names(&state), [("visible / .hidden".into(), 0)]);
}

/// 选中的跟着藏起来时不留着，删除、改名不作用在看不见的文件上；还看得见的照旧选着。
#[test]
fn hiding_the_selected_entry_clears_the_selection() {
    let mut state = TreeState::default();
    let root = Path::new("/p");
    let no_dotfiles = TreeFilter { ignored: true, dotfiles: false };
    state.listings.insert(root.into(), vec![file(".env"), file("a")]);
    state.rebuild_rows(root, ALL);
    state.select_row(1);
    state.refilter(root, no_dotfiles);
    assert_eq!(state.selected, Some(PathBuf::from("/p/a")));
    state.refilter(root, ALL);
    state.select_row(0);
    state.refilter(root, no_dotfiles);
    assert_eq!(state.selected, None);
}

#[test]
fn reveals_the_terminal_dir() {
    let mut state = TreeState::default();
    let root = Path::new("/p");
    state.listings.insert(root.into(), vec![dir("a"), dir("b")]);
    state.listings.insert("/p/b".into(), vec![dir("c"), file("x")]);
    state.listings.insert("/p/b/c".into(), vec![file("y")]);
    state.reveal_dir(Path::new("/p/b/c"), root, ALL);
    assert_eq!(names(&state), [("a".into(), 0), ("b".into(), 0), ("c".into(), 1), ("y".into(), 2), ("x".into(), 1)]);
    assert_eq!(state.selected, Some(PathBuf::from("/p/b/c")));

    // 终端目录在并成一行的链条中间时，展开到链条尽头。
    state.listings.insert("/p/a".into(), vec![dir("m")]);
    state.listings.insert("/p/a/m".into(), vec![file("z")]);
    state.reveal_dir(Path::new("/p/a"), root, ALL);
    assert_eq!(names(&state)[..2], [("a / m".into(), 0), ("z".into(), 1)]);
    assert_eq!(state.selected, Some(PathBuf::from("/p/a/m")));
}

#[test]
fn reveals_files_without_expanding_them() {
    let mut state = TreeState::default();
    let root = Path::new("/p");
    state.listings.insert(root.into(), vec![dir("a"), dir("b")]);
    state.listings.insert("/p/b".into(), vec![dir("c"), file("x")]);
    state.listings.insert("/p/b/c".into(), vec![file("y")]);
    state.reveal_file(Path::new("/p/b/x"), root, ALL);
    assert_eq!(names(&state), [("a".into(), 0), ("b".into(), 0), ("c".into(), 1), ("x".into(), 1)]);
    assert_eq!(state.selected, Some(PathBuf::from("/p/b/x")));
    state.collapse_all(root, ALL);
    assert_eq!(names(&state), [("a".into(), 0), ("b".into(), 0)]);
}

#[test]
fn moves_the_selection_with_the_keyboard() {
    let mut state = TreeState::default();
    let root = Path::new("/p");
    state.listings.insert(root.into(), vec![dir("a"), file("z")]);
    state.listings.insert("/p/a".into(), vec![file("b"), file("c")]);
    state.rebuild_rows(root, ALL);
    assert_eq!(state.select_step(-1), Some(1));
    assert_eq!(state.select_step(1), Some(1));
    assert_eq!(state.select_step(-5), Some(0));
    // 往右先展开，再进到第一项；往左先回到上一级，再收起。
    assert_eq!(state.select_in(root, ALL), Some(0));
    assert_eq!(state.rows.len(), 4);
    assert_eq!(state.select_in(root, ALL), Some(1));
    assert_eq!(state.selected, Some(PathBuf::from("/p/a/b")));
    assert_eq!(state.select_in(root, ALL), None);
    assert_eq!(state.select_out(root, ALL), Some(0));
    assert_eq!(state.select_out(root, ALL), Some(0));
    assert_eq!(state.rows.len(), 2);
    assert_eq!(state.select_out(root, ALL), None);

    // 改名或挪走的目录还是展开的，选中的跟过去。
    state.select_in(root, ALL);
    state.select_step(1);
    state.moved(Path::new("/p/a"), Path::new("/p/d"));
    assert!(state.expanded.contains(Path::new("/p/d")));
    assert!(!state.listings.contains_key(Path::new("/p/a")));
    assert_eq!(state.selected, Some(PathBuf::from("/p/d/b")));
    state.removed(Path::new("/p/d"));
    assert!(state.expanded.is_empty());
}

/// 展开、定位和改了文件时要读的目录不当场读，记下来交给后台；读回来以后行跟着变，定位等着的那一项。
#[test]
fn lists_dirs_later_and_reveals_when_they_arrive() {
    let mut state = TreeState::default();
    let root = Path::new("/p");
    state.listings.insert(root.into(), vec![dir("a"), file("z")]);
    state.rebuild_rows(root, ALL);
    state.toggle_dir(Path::new("/p/a"), root, ALL);
    assert_eq!(std::mem::take(&mut state.to_list), [PathBuf::from("/p/a")]);
    assert_eq!(state.rows.len(), 2);
    state.apply_listings(vec![(PathBuf::from("/p/a"), Some(vec![file("b")]))], 0, root, ALL);
    assert_eq!(names(&state), [("a".into(), 0), ("b".into(), 1), ("z".into(), 0)]);
    // 重读时读不了（删掉了或者没权限）的目录不再显示记着的旧内容，收起来。
    state.relist(vec![PathBuf::from("/p/a")], root, ALL);
    let read = std::mem::take(&mut state.to_list).into_iter().map(|dir| (dir, None)).collect();
    state.apply_listings(read, 0, root, ALL);
    assert_eq!(names(&state), [("a".into(), 0), ("z".into(), 0)]);
    assert!(!state.expanded.contains(Path::new("/p/a")));
    assert!(!state.listings.contains_key(Path::new("/p/a")));
    state.toggle_dir(Path::new("/p/a"), root, ALL);
    state.to_list.clear();
    state.apply_listings(vec![(PathBuf::from("/p/a"), Some(vec![file("b")]))], 0, root, ALL);
    assert_eq!(names(&state), [("a".into(), 0), ("b".into(), 1), ("z".into(), 0)]);

    // 新建的文件要等重读回来才排得出那一行，那时再选中它。
    state.relist(vec![PathBuf::from("/p/a")], root, ALL);
    state.reveal_file(Path::new("/p/a/new"), root, ALL);
    assert_eq!(state.selected, None);
    let read = std::mem::take(&mut state.to_list).into_iter().map(|dir| (dir, Some(vec![file("b"), file("new")])));
    state.apply_listings(read.collect(), 0, root, ALL);
    assert_eq!(state.selected, Some(PathBuf::from("/p/a/new")));

    // 删掉的不等重读回来就从行里去掉，选中的是它或在它下面时不留选中；选中别处的不动。
    state.removed(Path::new("/p/a/new"));
    state.rebuild_rows(root, ALL);
    assert_eq!(names(&state), [("a".into(), 0), ("b".into(), 1), ("z".into(), 0)]);
    assert_eq!(state.selected, None);
    state.selected = Some(PathBuf::from("/p/a/b"));
    state.removed(Path::new("/p/z"));
    assert_eq!(state.selected, Some(PathBuf::from("/p/a/b")));
    state.removed(Path::new("/p/a"));
    assert_eq!(state.selected, None);
    state.listings.insert(root.into(), vec![dir("a"), file("z")]);
    state.listings.insert("/p/a".into(), vec![file("b")]);
    state.expanded.insert("/p/a".into());
    state.rebuild_rows(root, ALL);

    // 读不了的目录读过一次就不再读，不会一直读下去。
    state.reveal_dir(Path::new("/p/gone/x"), root, ALL);
    let read: Listings = std::mem::take(&mut state.to_list).into_iter().map(|dir| (dir, None)).collect();
    assert!(!read.is_empty());
    state.apply_listings(read, 0, root, ALL);
    assert!(state.to_list.is_empty());
    assert!(state.pending_reveal.is_none());

    // 选中的在读不了、收起来的目录里面时，选中改成这个目录，不留着看不见的那一项。
    let open_a = |state: &mut TreeState| {
        state.expanded.insert(PathBuf::from("/p/a"));
        state.listings.insert(PathBuf::from("/p/a"), vec![file("b")]);
        state.rebuild_rows(root, ALL);
        state.select_row(1);
    };
    open_a(&mut state);
    assert_eq!(state.selected, Some(PathBuf::from("/p/a/b")));
    state.relist(vec![PathBuf::from("/p/a")], root, ALL);
    let read = std::mem::take(&mut state.to_list).into_iter().map(|dir| (dir, None)).collect();
    state.apply_listings(read, 0, root, ALL);
    assert_eq!(state.selected, Some(PathBuf::from("/p/a")));
    assert_eq!(state.selected_row(), Some(0));
    // 收起的目录自己的行也没了（上级的列表里已经去掉了它）时不留选中。
    open_a(&mut state);
    assert_eq!(state.selected, Some(PathBuf::from("/p/a/b")));
    state.listings.insert(root.into(), vec![file("z")]);
    state.apply_listings(vec![(PathBuf::from("/p/a"), None)], 0, root, ALL);
    assert_eq!(state.selected, None);
    // 根目录换了以后才读回来的不要。
    state.apply_listings(vec![(PathBuf::from("/q"), Some(vec![file("y")]))], 0, root, ALL);
    assert!(!state.listings.contains_key(Path::new("/q")));
}

/// 定位等着读回来的目录，那个目录却读不了：它收起来，不被等着的定位又展开成一个空的展开目录。
#[test]
fn unreadable_dirs_stay_collapsed_after_a_pending_reveal() {
    let mut state = TreeState::default();
    let root = Path::new("/p");
    state.listings.insert(root.into(), vec![dir("a"), file("z")]);
    state.rebuild_rows(root, ALL);
    state.reveal_file(Path::new("/p/a/file"), root, ALL);
    assert_eq!(state.to_list, [PathBuf::from("/p/a")]);
    let read = std::mem::take(&mut state.to_list).into_iter().map(|dir| (dir, None)).collect();
    state.apply_listings(read, 0, root, ALL);
    assert!(!state.expanded.contains(Path::new("/p/a")));
    assert!(!state.rows[0].expanded);
    assert!(state.to_list.is_empty());
    assert!(state.pending_reveal.is_none());
    // 收着的目录点开时照常再读。
    state.toggle_dir(Path::new("/p/a"), root, ALL);
    assert_eq!(state.to_list, [PathBuf::from("/p/a")]);
    assert!(state.rows[0].expanded);
}

/// 选中项套在几层读不了的目录里：不管读回来的先后，都改选还有行的最外层。
#[test]
fn nested_unreadable_dirs_move_the_selection_to_the_visible_one() {
    let open = || {
        let mut state = TreeState::default();
        let root = Path::new("/p");
        state.listings.insert(root.into(), vec![dir("a"), file("z")]);
        state.listings.insert("/p/a".into(), vec![dir("b"), file("y")]);
        state.listings.insert("/p/a/b".into(), vec![file("file")]);
        state.expanded.extend([PathBuf::from("/p/a"), PathBuf::from("/p/a/b")]);
        state.rebuild_rows(root, ALL);
        state.selected = Some(PathBuf::from("/p/a/b/file"));
        assert_eq!(state.selected_row(), Some(2));
        state
    };
    for order in [["/p/a/b", "/p/a"], ["/p/a", "/p/a/b"]] {
        let mut state = open();
        let read = order.iter().map(|dir| (PathBuf::from(dir), None)).collect();
        state.apply_listings(read, 0, Path::new("/p"), ALL);
        assert_eq!(names(&state), [("a".into(), 0), ("z".into(), 0)], "{order:?}");
        assert_eq!(state.selected, Some(PathBuf::from("/p/a")), "{order:?}");
    }
}

/// 后台列目录发起之后又发起、先换上的扫描写过的目录，读回来的旧内容不盖掉扫描的结果；扫描没碰过的目录、
/// 扫描之后才发起的照常换上。
#[test]
fn older_listings_do_not_overwrite_newer_scans() {
    let mut state = TreeState::default();
    let root = Path::new("/p");
    let listing = |dir: &str, entries| (PathBuf::from(dir), Some(entries));
    let scan = |listings, started| TreeScan { root: root.into(), git: None, listings, reveal: None, started };
    state.apply_scan(scan(vec![listing("/p", vec![dir("a")])], 1), ALL);
    state.toggle_dir(Path::new("/p/a"), root, ALL);
    state.apply_scan(scan(vec![listing("/p/a", vec![dir("sub"), file("c")])], 3), ALL);
    let old = vec![listing("/p/a", vec![dir("sub"), file("b"), file("c")]), listing("/p/a/sub", vec![file("x")])];
    state.apply_listings(old, 2, root, ALL);
    assert_eq!(state.listings[Path::new("/p/a")], [dir("sub"), file("c")]);
    assert_eq!(state.listings[Path::new("/p/a/sub")], [file("x")]);
    state.apply_listings(vec![listing("/p/a", vec![file("d")])], 4, root, ALL);
    assert_eq!(state.listings[Path::new("/p/a")], [file("d")]);
}

/// 扫描比列目录发起得早、换上得晚（新建或改名后重读的那种）：扫描读到的旧内容不盖掉列目录的结果，
/// 等着定位的新文件照样选中；扫描里列目录没碰过的目录照常换上。
#[test]
fn older_scans_do_not_overwrite_newer_listings() {
    let mut state = TreeState::default();
    let root = Path::new("/p");
    let listing = |dir: &str, entries| (PathBuf::from(dir), Some(entries));
    let scan = |listings, started| TreeScan { root: root.into(), git: None, listings, reveal: None, started };
    state.apply_scan(scan(vec![listing("/p", vec![dir("a"), file("z")])], 1), ALL);
    // 扫描 2 发起后新建了 /p/new，重读 /p（列目录 3）并定位它；列目录先读回来。
    state.relist(vec![root.into()], root, ALL);
    state.reveal_file(Path::new("/p/new"), root, ALL);
    assert_eq!(std::mem::take(&mut state.to_list), [PathBuf::from("/p")]);
    state.apply_listings(vec![listing("/p", vec![dir("a"), file("new"), file("z")])], 3, root, ALL);
    assert_eq!(state.selected, Some(PathBuf::from("/p/new")));
    state.apply_scan(scan(vec![listing("/p", vec![dir("a"), file("z")]), listing("/p/a", vec![file("b")])], 2), ALL);
    assert_eq!(state.listings[root], [dir("a"), file("new"), file("z")]);
    assert_eq!(state.listings[Path::new("/p/a")], [file("b")]);
    assert_eq!(state.selected, Some(PathBuf::from("/p/new")));
    assert_eq!(state.selected_row(), Some(1));
}

/// 换上扫描结果：换了根目录时丢掉上一处的目录列表，同一份 git 状态不算变化，读不了的目录收起来，
/// 终端换了目录时定位过去。
#[test]
fn applies_scans() {
    let mut state = TreeState::default();
    let listing = |dir: &str, entries| (PathBuf::from(dir), Some(entries));
    let scan = |listings, reveal: Option<&str>| TreeScan {
        root: "/p".into(),
        git: None,
        listings,
        reveal: reveal.map(PathBuf::from),
        started: 0,
    };
    assert!(state.apply_scan(scan(vec![listing("/p", vec![dir("a"), file("b")])], None), ALL));
    assert_eq!(names(&state), [("a".into(), 0), ("b".into(), 0)]);
    assert!(!state.apply_scan(scan(vec![listing("/p", vec![dir("a"), file("b")])], None), ALL));
    assert!(state.apply_scan(scan(vec![listing("/p/a", vec![file("c")])], Some("/p/a")), ALL));
    assert_eq!(state.selected, Some(PathBuf::from("/p/a")));
    assert_eq!(names(&state), [("a".into(), 0), ("c".into(), 1), ("b".into(), 0)]);
    assert!(state.apply_scan(scan(vec![(PathBuf::from("/p/a"), None)], None), ALL));
    assert!(!state.expanded.contains(Path::new("/p/a")));
    // 换了根目录时上一处选中的路径不留着，免得删除、改名作用在看不见的旧路径上。
    state.selected = Some(PathBuf::from("/p/b"));
    let moved = TreeScan { root: "/q".into(), git: None, listings: Vec::new(), reveal: None, started: 0 };
    assert!(state.apply_scan(moved, ALL));
    assert!(state.listings.is_empty());
    assert_eq!(state.selected, None);
}

/// 重扫后选中项没了行：在读不了、收起来的目录里时改选还有行的那个收起的目录，不看扫回来的先后；
/// 自己被删掉（上级的列表里没了）时不留选中。
#[test]
fn scans_move_a_selection_that_lost_its_row() {
    let root = Path::new("/p");
    let open = |selected: &str| {
        let mut state = TreeState::default();
        let listing = |dir: &str, entries| (PathBuf::from(dir), Some(entries));
        let scan = TreeScan {
            root: root.into(),
            git: None,
            listings: vec![
                listing("/p", vec![dir("a"), file("z")]),
                listing("/p/a", vec![dir("b"), file("y")]),
                listing("/p/a/b", vec![file("file")]),
            ],
            reveal: None,
            started: 0,
        };
        assert!(state.apply_scan(scan, ALL));
        state.expanded.extend([PathBuf::from("/p/a"), PathBuf::from("/p/a/b")]);
        state.rebuild_rows(root, ALL);
        state.selected = Some(PathBuf::from(selected));
        assert!(state.selected_row().is_some());
        state
    };
    let rescan = |state: &mut TreeState, listings| {
        let scan = TreeScan { root: root.into(), git: None, listings, reveal: None, started: 0 };
        assert!(state.apply_scan(scan, ALL));
    };

    // 两层都读不了：里层跟着外层收起没了行，改选外层。
    for order in [["/p/a/b", "/p/a"], ["/p/a", "/p/a/b"]] {
        let mut state = open("/p/a/b/file");
        rescan(&mut state, order.iter().map(|dir| (PathBuf::from(dir), None)).collect());
        assert_eq!(names(&state), [("a".into(), 0), ("z".into(), 0)], "{order:?}");
        assert_eq!(state.selected, Some(PathBuf::from("/p/a")), "{order:?}");
    }
    // 只有里层读不了：它还有行，改选它。
    let mut state = open("/p/a/b/file");
    rescan(&mut state, vec![(PathBuf::from("/p/a/b"), None)]);
    assert_eq!(state.selected, Some(PathBuf::from("/p/a/b")));
    // 选中的文件从上级的列表里去掉了。
    let mut state = open("/p/a/y");
    rescan(&mut state, vec![(PathBuf::from("/p/a"), Some(vec![dir("b")]))]);
    assert_eq!(state.selected, None);
    // 选中的目录自己被删了：收起来，上级的列表里也没了它。
    let mut state = open("/p/a/b");
    rescan(&mut state, vec![(PathBuf::from("/p/a"), Some(vec![file("y")])), (PathBuf::from("/p/a/b"), None)]);
    assert!(!state.expanded.contains(Path::new("/p/a/b")));
    assert_eq!(state.selected, None);
}
