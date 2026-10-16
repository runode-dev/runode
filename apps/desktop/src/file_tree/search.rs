//! 文件树上面的搜索框：按名称找文件，或者按内容找文件里的行。搜索词不为空时文件树换成结果，
//! 单击在预览栏里打开（内容结果跳到那一行），双击开成固定标签，回车打开选中的那一条，Esc 清空。
//! 结果可以排成列表，也可以按目录排成树（和 Git 面板共用 `path_tree`），目录能收起。
//!
//! 在后台搜：仓库里按名称找的是 `runode_git::list_files` 列出的文件，不在仓库里时从根目录往下走，
//! 列一次记着，文件树重读过时再列；按内容找调 `runode_git::grep`，等打字停一下再起，正则按 git 的写法，
//! 写得不对时显示 git 的报错。按名称找时搜索词全小写就不分大小写；按内容找时可以区分大小写、
//! 全字匹配、用正则，还能只找或不找某些文件（逗号隔开的 glob，写法和 VS Code 一样）。文件树
//! 重读过（文件可能变了）、根目录、搜索词、方式或选项变了时重搜。

use std::{
    collections::HashSet,
    fs,
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use crate::ui::{
    a11y::{A11yPress, Press},
    file_icons::{file_icon, folder_icon},
    font_size::{FontSize, scaled},
    icons::{CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON, CLOSE_ICON},
    input::{Input, InputEvent, InputState},
    panel::panel_message,
    path_tree::{TreeItem, path_tree},
    theme::ActiveTheme as _,
    tooltip::tooltip,
};
use gpui::{
    AnyElement, App, ClickEvent, Context, Div, Entity, HighlightStyle, Role, ScrollStrategy, SharedString, StyledText,
    Subscription, UniformListScrollHandle, div, img, prelude::*, px, svg, uniform_list,
};
use regex::{Regex, RegexBuilder};
use runode_git::GrepQuery;
use serde::{Deserialize, Serialize};

use crate::file_tree::{FILTER_ICON, FileTree, FileTreeEvent};

/// 按名称最多列出的文件数。
const MAX_FILES: usize = 500;
/// 按内容最多列出的行数。
const MAX_LINES: usize = 2000;
/// 按内容找时停止打字这么久才搜。
const CONTENT_DEBOUNCE: Duration = Duration::from_millis(200);
/// 不在仓库里时从根目录往下最多看这么多个文件，免得在家目录这种地方走个没完。
const MAX_WALK: usize = 50_000;
/// 内容结果里命中处前面最多留的字节数，再多就从前面截掉，命中的词才露得出来。
const SNIPPET_LEAD: usize = 24;
/// 内容结果一行最多留的字节数，压缩过的长行不整行排版。
const SNIPPET_MAX: usize = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SearchMode {
    Name,
    Content,
}

/// 按内容搜索时的选项：区分大小写、全字匹配、用正则，以及要包含、要排除的文件（逗号
/// 隔开的 glob）。随窗口存档；搜索时也拿它比较两次搜索的条件一不一样。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchOptions {
    pub match_case: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub include: String,
    pub exclude: String,
}

/// 找到的一个文件；按内容找时带着其中命中的行。
#[derive(Clone, Debug, PartialEq, Eq)]
struct Found {
    path: PathBuf,
    /// 相对根目录。
    rel: PathBuf,
    name: SharedString,
    /// 所在目录，相对根目录；列表形式时写在名字后面。
    dir: SharedString,
    lines: Vec<FoundLine>,
}

/// 按内容找到的一行：`line` 从 1 数，`hit` 是 `text` 里命中的那段。
#[derive(Clone, Debug, PartialEq, Eq)]
struct FoundLine {
    line: u32,
    text: SharedString,
    hit: Option<Range<usize>>,
}

/// 结果排成的一行。`file` 是 `FileSearch::found` 的下标，`line` 是那个文件 `lines` 的下标。
#[derive(Clone, Debug, PartialEq, Eq)]
enum SearchRow {
    /// 树形式时的目录，`path` 相对根目录。
    Dir {
        path: PathBuf,
        name: SharedString,
        depth: usize,
        expanded: bool,
    },
    File {
        file: usize,
        depth: usize,
    },
    Line {
        file: usize,
        line: usize,
        depth: usize,
    },
}

/// 把 `found` 排成行：列表形式按搜到的顺序，树形式按目录分层，`collapsed` 里的目录收起；
/// 命中的行跟在各自的文件后面、缩进一层。
fn layout_rows(found: &[Found], tree: bool, collapsed: &HashSet<PathBuf>) -> Vec<SearchRow> {
    let files: Vec<_> = if tree {
        let paths: Vec<_> = found.iter().enumerate().map(|(ix, found)| (ix, found.rel.as_path())).collect();
        path_tree(&paths, |dir| !collapsed.contains(dir))
    } else {
        (0..found.len()).map(|index| TreeItem::File { index, depth: 0 }).collect()
    };
    let mut rows = Vec::new();
    for item in files {
        match item {
            TreeItem::Dir { path, name, depth, expanded } => {
                rows.push(SearchRow::Dir { path, name: name.into(), depth, expanded });
            }
            TreeItem::File { index, depth } => {
                rows.push(SearchRow::File { file: index, depth });
                rows.extend((0..found[index].lines.len()).map(|line| SearchRow::Line {
                    file: index,
                    line,
                    depth: depth + 1,
                }));
            }
        }
    }
    rows
}

/// 一次搜索：在哪个根目录、按什么词、哪种方式和选项。
#[derive(Clone, Debug, PartialEq, Eq)]
struct SearchKey {
    root: PathBuf,
    query: String,
    mode: SearchMode,
    options: SearchOptions,
}

/// `root` 下按名称找的文件，相对 `root`，第一次搜时在后台列出；同时在跑的几次搜索等同一次列完。
struct NameList {
    root: PathBuf,
    files: OnceLock<Vec<PathBuf>>,
}

pub(crate) struct FileSearch {
    field: Entity<InputState>,
    mode: SearchMode,
    /// 按内容找时的区分大小写、全字匹配和正则开关，以及要包含、要排除的文件。
    match_case: bool,
    whole_word: bool,
    regex: bool,
    include: Entity<InputState>,
    exclude: Entity<InputState>,
    /// 上次搜出了错（比如正则按 git 的写法不对）时是报错。
    error: Option<String>,
    /// 排成树还是列表，整个窗口一个设置，存进窗口存档；树形式时收起的目录。
    pub(crate) tree: bool,
    collapsed: HashSet<PathBuf>,
    found: Vec<Found>,
    /// `found` 按 `tree` 和 `collapsed` 排成的行。
    rows: Vec<SearchRow>,
    /// `found` 是哪次搜索的结果；为空时要重搜。
    searched: Option<SearchKey>,
    /// 后台正在跑的那次搜索和取消它的标志；换了搜索词时置位，停下上一次。
    pending: Option<(SearchKey, Arc<AtomicBool>)>,
    /// 同一次搜索还在跑时文件树又重读过：跑完再搜一次。
    rerun: bool,
    /// 按名称找时列出的文件，接着打字时不用每个键都再列一遍；文件树重读过、换了根目录时换一份。
    names: Option<Arc<NameList>>,
    selected: Option<usize>,
    scroll: UniformListScrollHandle,
    _events: [Subscription; 3],
}

impl FileSearch {
    pub(crate) fn new(cx: &mut Context<FileTree>) -> Self {
        let placeholder = rust_i18n::t!("files.search_placeholder").into_owned();
        let field = cx
            .new(|cx| InputState::new(String::new(), cx).with_label(placeholder.clone()).with_placeholder(placeholder));
        let events = cx.subscribe(&field, |this, field, event: &InputEvent, cx| match event {
            InputEvent::Changed(_) => this.sync_file_search(cx),
            // 没选中时打开第一个文件，跳过树形式开头的目录。
            InputEvent::Next => {
                let rows = &this.search.rows;
                let first = rows.iter().position(|row| !matches!(row, SearchRow::Dir { .. }));
                if let Some(ix) = this.search.selected.or(first) {
                    this.open_search_row(ix, false, cx);
                }
            }
            InputEvent::Previous => {}
            // 有搜索词时清空，已经空着时把焦点交回终端。
            InputEvent::Dismiss => {
                if field.read(cx).query().is_empty() {
                    cx.emit(FileTreeEvent::FocusTerminal);
                } else {
                    field.update(cx, |field, cx| field.set_query(String::new(), cx));
                    this.sync_file_search(cx);
                }
            }
        });
        let filter = |label: &str, placeholder: &str, cx: &mut Context<FileTree>| {
            let (label, placeholder) = (rust_i18n::t!(label).into_owned(), rust_i18n::t!(placeholder).into_owned());
            let field = cx.new(|cx| InputState::new(String::new(), cx).with_label(label).with_placeholder(placeholder));
            let events = cx.subscribe(&field, |this, _, event: &InputEvent, cx| {
                if let InputEvent::Changed(_) = event {
                    this.sync_file_search(cx);
                    cx.emit(FileTreeEvent::OptionsChanged);
                }
            });
            (field, events)
        };
        let (include, include_events) = filter("files.include", "files.include_placeholder", cx);
        let (exclude, exclude_events) = filter("files.exclude", "files.exclude_placeholder", cx);
        Self {
            field,
            mode: SearchMode::Name,
            match_case: false,
            whole_word: false,
            regex: false,
            include,
            exclude,
            error: None,
            tree: false,
            collapsed: HashSet::new(),
            found: Vec::new(),
            rows: Vec::new(),
            searched: None,
            pending: None,
            rerun: false,
            names: None,
            selected: None,
            scroll: UniformListScrollHandle::new(),
            _events: [events, include_events, exclude_events],
        }
    }
}

/// 搜索词全小写时不分大小写。
fn ignore_case(query: &str) -> bool {
    !query.chars().any(char::is_uppercase)
}

/// 不在仓库里时按名称找的文件：从 `root` 往下走，不跟符号链接，跳过 `.git`，最多 `MAX_WALK` 个，
/// 相对 `root`。
fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut dirs = vec![PathBuf::new()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = fs::read_dir(root.join(&dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name == ".git" || name == ".DS_Store" {
                continue;
            }
            let path = dir.join(name);
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                dirs.push(path);
            } else {
                files.push(path);
                if files.len() >= MAX_WALK {
                    return files;
                }
            }
        }
    }
    files
}

/// `files` 里名字含有 `query` 的，搜索词带 `/` 时比整个相对路径。名字以它开头的排在前面，
/// 其余保持原来的顺序。`cancel` 置位时停下，返回已经找到的。
fn match_names<'a>(files: &'a [PathBuf], query: &str, cancel: &AtomicBool) -> Vec<&'a PathBuf> {
    let fold = |text: &str| if ignore_case(query) { text.to_lowercase() } else { text.to_owned() };
    let needle = fold(query);
    let mut hits: Vec<_> = files
        .iter()
        .take_while(|_| !cancel.load(Ordering::Relaxed))
        .filter_map(|rel| {
            let haystack = if query.contains('/') { rel.to_string_lossy() } else { rel.file_name()?.to_string_lossy() };
            let at = fold(&haystack).find(&needle)?;
            Some((at != 0, rel))
        })
        .collect();
    hits.sort_by_key(|(later, _)| *later);
    hits.into_iter().map(|(_, rel)| rel).collect()
}

/// 内容结果里标出命中处用的正则，尽量和交给 git grep 的条件一致。只用来标出命中处，搜索本身和正则对不对
/// 都以 git 为准：git 用的是 POSIX 扩展正则，Rust 的 regex 不认的写法（比如反向引用）这里是错误，只是不标。
fn matcher(query: &str, options: &SearchOptions) -> Result<Regex, regex::Error> {
    let pattern = if options.regex { query.to_owned() } else { regex::escape(query) };
    let pattern = if options.whole_word { format!(r"\b(?:{pattern})\b") } else { pattern };
    RegexBuilder::new(&pattern).case_insensitive(!options.match_case).build()
}

/// 「要包含的文件」「要排除的文件」里逗号隔开的 glob 换成 git 的 pathspec。和 VS Code 一样，
/// 不以 `**/` 或 `./` 开头的在任何一层都算（`*.ts` 是 `**/*.ts`），写的是目录时也算它里面的文件。
fn pathspecs(include: &str, exclude: &str) -> Vec<String> {
    let specs = |text: &str, magic: &'static str| {
        let globs = split_commas(text).into_iter().map(str::trim).filter(|glob| !glob.is_empty());
        globs
            .flat_map(expand_braces)
            .flat_map(move |glob| {
                let glob = match glob.strip_prefix("./") {
                    Some(rel) => rel.to_owned(),
                    None if glob.starts_with("**/") => glob.clone(),
                    None => format!("**/{glob}"),
                };
                let glob = glob.trim_end_matches('/');
                [format!(":({magic}){glob}"), format!(":({magic}){glob}/**")]
            })
            .collect::<Vec<_>>()
    };
    [specs(include, "glob"), specs(exclude, "exclude,glob")].concat()
}

/// 按花括号外面的逗号拆开。
fn split_commas(text: &str) -> Vec<&str> {
    let (mut parts, mut depth, mut start) = (Vec::new(), 0usize, 0);
    for (at, c) in text.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&text[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// `*.{ts,tsx}` 展开成 `*.ts`、`*.tsx`（git 的 glob 不认花括号），可以嵌套、有好几组；没配对的花括号原样留着。
fn expand_braces(glob: &str) -> Vec<String> {
    let Some(open) = glob.find('{') else {
        return vec![glob.to_owned()];
    };
    let mut depth = 0;
    let close = glob[open..].char_indices().find_map(|(at, c)| {
        match c {
            '{' => depth += 1,
            '}' if depth == 1 => return Some(open + at),
            '}' => depth -= 1,
            _ => {}
        }
        None
    });
    let Some(close) = close else {
        return vec![glob.to_owned()];
    };
    let (head, tail) = (&glob[..open], &glob[close + 1..]);
    split_commas(&glob[open + 1..close])
        .into_iter()
        .flat_map(|alt| expand_braces(&format!("{head}{alt}{tail}")))
        .collect()
}

/// 内容结果里显示的一行：去掉开头的缩进，命中处太靠后时把前面截掉，太长时截短；返回显示的
/// 文字和其中按 `matcher` 命中的第一段；没有 `matcher` 时不标。
fn snippet(text: &str, matcher: Option<&Regex>) -> (String, Option<Range<usize>>) {
    let text = text.trim_start();
    let at = matcher.and_then(|matcher| matcher.find(text)).map(|hit| hit.range()).filter(|hit| !hit.is_empty());
    let start = match &at {
        Some(at) if at.start > SNIPPET_LEAD => text.floor_char_boundary(at.start - SNIPPET_LEAD),
        _ => 0,
    };
    let end = text.floor_char_boundary((start + SNIPPET_MAX).min(text.len()));
    let prefix = if start > 0 { "…" } else { "" };
    let shift = |at: usize| at - start + prefix.len();
    let hit = at.filter(|at| at.end <= end).map(|at| shift(at.start)..shift(at.end));
    (format!("{prefix}{}", &text[start..end]), hit)
}

/// 交给搜索的词：按名称找时去掉两头的空白；按内容找时原样搜，搜 `fn ` 不变成搜 `fn`，只有整串都是空白时
/// 当作没输入，和 `FileTree::searching` 一致。
fn search_query(mode: SearchMode, text: &str) -> String {
    match mode {
        SearchMode::Name => text.trim().to_owned(),
        SearchMode::Content if text.trim().is_empty() => String::new(),
        SearchMode::Content => text.to_owned(),
    }
}

fn found(root: &Path, rel: PathBuf, lines: Vec<FoundLine>) -> Found {
    let name = rel.file_name().unwrap_or(rel.as_os_str()).to_string_lossy().into_owned();
    let dir = rel.parent().map(|dir| dir.to_string_lossy().into_owned()).unwrap_or_default();
    Found { path: root.join(&rel), rel, name: name.into(), dir: dir.into(), lines }
}

/// 按 `key` 搜；在后台跑。按名称找时在 `names` 里找，还没列出时先列。git grep 报错时是它的报错。
fn run_search(key: &SearchKey, names: &NameList, cancel: &AtomicBool) -> Result<Vec<Found>, String> {
    let SearchKey { root, query, options, .. } = key;
    match key.mode {
        SearchMode::Name => {
            let files = names.files.get_or_init(|| runode_git::list_files(root).unwrap_or_else(|| walk_files(root)));
            // 列表可能是一会儿前列的，git 也列出工作区里删掉了的已跟踪文件，只查要显示的这些在不在。
            let hits = match_names(files, query, cancel).into_iter();
            let hits = hits.filter(|rel| root.join(rel).symlink_metadata().is_ok()).take(MAX_FILES);
            Ok(hits.map(|rel| found(root, rel.clone(), Vec::new())).collect())
        }
        SearchMode::Content => {
            let matcher = matcher(query, options).ok();
            let pathspecs = pathspecs(&options.include, &options.exclude);
            let grep = GrepQuery {
                pattern: query,
                ignore_case: !options.match_case,
                whole_word: options.whole_word,
                regex: options.regex,
                pathspecs: &pathspecs,
            };
            let matches = runode_git::grep(root, &grep, MAX_LINES, cancel).map_err(|err| err.message)?;
            // git grep 按文件一段段地输出，同一个文件的行连在一起。
            let groups = matches.chunk_by(|a, b| a.path == b.path);
            let found = groups
                .map(|group| {
                    let lines = group
                        .iter()
                        .map(|hit| {
                            let (text, range) = snippet(&hit.text, matcher.as_ref());
                            FoundLine { line: hit.line, text: text.into(), hit: range }
                        })
                        .collect();
                    found(root, group[0].path.clone(), lines)
                })
                .collect();
            Ok(found)
        }
    }
}

impl FileTree {
    /// 正在搜：搜索词不为空。新建、改名的输入框在文件树里，那时照样显示文件树。
    pub(super) fn searching(&self, cx: &gpui::App) -> bool {
        self.edit.is_none() && !self.search.field.read(cx).query().trim().is_empty()
    }

    /// 根目录、搜索词或方式和上次搜的不一样时在后台重搜，停下还在跑的上一次。
    pub(super) fn sync_file_search(&mut self, cx: &mut Context<Self>) {
        self.search_files(false, cx);
    }

    /// 同 `sync_file_search`；`force` 时条件没变也重搜（文件树重读过），新结果到之前旧结果照旧显示。
    fn search_files(&mut self, force: bool, cx: &mut Context<Self>) {
        let search = &self.search;
        let options = match search.mode {
            SearchMode::Name => SearchOptions::default(),
            SearchMode::Content => search.options(cx),
        };
        let query = search_query(search.mode, search.field.read(cx).query());
        let key = SearchKey { root: self.files_root(), query, mode: search.mode, options };
        let search = &mut self.search;
        cx.notify();
        // 文件一直在变时重读比按内容找的去抖还勤，每次都打断就永远搜不完：不打断，跑完再搜一次。
        if force && search.pending.as_ref().is_some_and(|(pending, _)| *pending == key) {
            search.rerun = true;
            return;
        }
        search.rerun = false;
        // 文件树重读过，文件可能变了。
        if force || search.names.as_ref().is_some_and(|names| names.root != key.root) {
            search.names = None;
        }
        let current = search.pending.as_ref().map(|(key, _)| key).or(search.searched.as_ref());
        if !force && current == Some(&key) {
            return;
        }
        if let Some((_, cancel)) = search.pending.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        if key.query.is_empty() {
            search.found.clear();
            search.rows.clear();
            search.searched = None;
            search.selected = None;
            search.error = None;
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        search.pending = Some((key.clone(), cancel.clone()));
        let names = search
            .names
            .get_or_insert_with(|| Arc::new(NameList { root: key.root.clone(), files: OnceLock::new() }))
            .clone();
        cx.spawn(async move |this, cx| {
            // 按内容找每次都要 git grep 走一遍整个目录，等打字停一下再起；按名称找只在内存里匹配，不用等。
            if key.mode == SearchMode::Content {
                cx.background_executor().timer(CONTENT_DEBOUNCE).await;
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
            }
            let found = cx
                .background_executor()
                .spawn({
                    let key = key.clone();
                    let cancel = cancel.clone();
                    async move { run_search(&key, &names, &cancel) }
                })
                .await;
            this.update(cx, |this, cx| {
                // 搜的时候换了词，结果作废。
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let search = &mut this.search;
                search.pending = None;
                // 只是重读后重搜（同一个根目录，词、方式和选项都没变）时留着选中的行和滚动位置。
                let same = search.searched.as_ref() == Some(&key);
                if !same {
                    search.selected = None;
                    search.collapsed.clear();
                    search.scroll.scroll_to_item(0, ScrollStrategy::Top);
                }
                (search.found, search.error) = match found {
                    Ok(found) => (found, None),
                    Err(err) => (Vec::new(), Some(err)),
                };
                search.searched = Some(key);
                search.relayout();
                cx.notify();
                if std::mem::take(&mut this.search.rerun) {
                    this.search_files(true, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// 文件树重读过，文件可能变了：有搜索词时重搜。
    pub(crate) fn refresh_file_search(&mut self, cx: &mut Context<Self>) {
        self.search_files(true, cx);
    }

    pub(super) fn toggle_search_tree(&mut self, cx: &mut Context<Self>) {
        let tree = !self.search.tree;
        self.search.set_tree(tree);
        cx.emit(FileTreeEvent::OptionsChanged);
        cx.notify();
    }

    fn set_search_mode(&mut self, mode: SearchMode, cx: &mut Context<Self>) {
        self.search.mode = mode;
        self.sync_file_search(cx);
    }

    /// 在预览栏里打开第 `ix` 行结果，`pin` 时开成固定标签；内容结果跳到那一行，目录展开或收起。
    fn open_search_row(&mut self, ix: usize, pin: bool, cx: &mut Context<Self>) {
        let search = &mut self.search;
        match search.rows.get(ix).cloned() {
            Some(SearchRow::Dir { path, .. }) => {
                if !search.collapsed.remove(&path) {
                    search.collapsed.insert(path);
                }
                search.relayout();
            }
            Some(SearchRow::File { file, .. }) => {
                let path = search.found[file].path.clone();
                cx.emit(FileTreeEvent::Open { path, line: None, pin });
            }
            Some(SearchRow::Line { file, line, .. }) => {
                let found = &search.found[file];
                let (path, line) = (found.path.clone(), found.lines[line].line);
                cx.emit(FileTreeEvent::Open { path, line: Some(line.saturating_sub(1) as usize), pin });
            }
            None => return,
        }
        self.search.selected = Some(ix);
        cx.notify();
    }

    /// 搜索框，下面是切换按名称、按内容找的两段按钮；按内容找时搜索框里多出区分大小写、全字
    /// 匹配和正则三个开关，下面多出要包含、要排除的文件两个输入框。
    pub(super) fn render_file_search_box(&self, cx: &mut Context<Self>) -> Div {
        let search = &self.search;
        let content = search.mode == SearchMode::Content;
        let theme = cx.theme();
        let (fg, bg, input_bg, on_bg, track_bg) =
            (theme.foreground, theme.background, theme.mix(0.06), theme.mix(0.18), theme.mix(0.08));
        // 输入框，比文件树的底色浅一档。
        let input_box = |field: &Entity<InputState>| Input::new(field).gap(px(4.)).bg(input_bg);
        // 搜索框里的小按钮：开着的开关垫一层底色。
        let small_button = |id: &'static str, tip: String, on: bool| {
            div()
                .id(id)
                .aria_label(tip.clone())
                .flex_none()
                .h(scaled(20.))
                .min_w(scaled(20.))
                .px(px(3.))
                .rounded(px(4.))
                .flex()
                .items_center()
                .justify_center()
                .text_size(FontSize::Small)
                .map(|item| {
                    if on {
                        item.bg(on_bg).text_color(fg)
                    } else {
                        item.text_color(fg.opacity(0.55)).hover(|item| item.text_color(fg))
                    }
                })
                .tooltip(tooltip(tip, None))
        };
        type Flip = fn(&mut FileSearch);
        let option = |id, label: &'static str, tip: &str, on: bool, flip: Flip, cx: &mut Context<Self>| {
            small_button(id, rust_i18n::t!(tip).into_owned(), on)
                .role(Role::CheckBox)
                .aria_toggled(on.into())
                .child(label)
                .on_press(cx, move |this, _, cx| {
                    flip(&mut this.search);
                    this.sync_file_search(cx);
                    cx.emit(FileTreeEvent::OptionsChanged);
                })
        };
        let has_query = !search.field.read(cx).query().is_empty();
        let clear = has_query.then(|| {
            small_button("search-clear", rust_i18n::t!("files.clear_search").into_owned(), false)
                .role(Role::Button)
                .child(svg().path(CLOSE_ICON).size(px(12.)).text_color(fg.opacity(0.6)))
                .on_press(cx, |this, _, cx| {
                    this.search.field.update(cx, |field, cx| field.set_query(String::new(), cx));
                    this.sync_file_search(cx);
                })
        });
        let options = content.then(|| {
            [
                option("search-case", "Aa", "files.match_case", search.match_case, |s| s.match_case ^= true, cx),
                option("search-word", "ab", "files.whole_word", search.whole_word, |s| s.whole_word ^= true, cx)
                    .underline(),
                option("search-regex", ".*", "files.use_regex", search.regex, |s| s.regex ^= true, cx),
            ]
        });
        let filter_icon = svg().path(FILTER_ICON).flex_none().size(px(14.)).text_color(fg.opacity(0.5));
        let field =
            input_box(&search.field).prefix(filter_icon).children(clear).children(options.into_iter().flatten());
        let mode = search.mode;
        let segment = |id, label: SharedString, value: SearchMode, cx: &mut Context<Self>| {
            let on = mode == value;
            div()
                .id(id)
                .role(Role::RadioButton)
                .aria_label(label.clone())
                .aria_toggled(on.into())
                .flex_1()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .text_size(FontSize::Body)
                .map(|item| {
                    if on {
                        item.bg(bg).text_color(fg)
                    } else {
                        item.text_color(fg.opacity(0.6)).hover(|item| item.text_color(fg))
                    }
                })
                .child(label)
                .on_press(cx, move |this, _, cx| this.set_search_mode(value, cx))
        };
        let segments = div()
            .id("search-mode")
            .role(Role::RadioGroup)
            .aria_label(rust_i18n::t!("files.search_mode").into_owned())
            .h(scaled(26.))
            .p(px(2.))
            .flex()
            .rounded(px(7.))
            .bg(track_bg)
            .child(segment("search-name", rust_i18n::t!("files.search_name").into_owned().into(), SearchMode::Name, cx))
            .child(segment(
                "search-content",
                rust_i18n::t!("files.search_content").into_owned().into(),
                SearchMode::Content,
                cx,
            ));
        let labeled = |label: &str, field: &Entity<InputState>| {
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .text_size(FontSize::Body)
                        .text_color(fg.opacity(0.7))
                        .child(rust_i18n::t!(label).into_owned()),
                )
                .child(input_box(field))
        };
        let filters = content.then(|| {
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(labeled("files.include", &search.include))
                .child(labeled("files.exclude", &search.exclude))
        });
        div()
            .flex_none()
            .px(px(10.))
            .pb(px(8.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(field)
            .child(segments)
            .children(filters)
    }

    /// 搜索结果，没有正在搜时为空（显示文件树）。
    pub(super) fn render_file_search_results(&self, font_size: f32, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.searching(cx) {
            return None;
        }
        let search = &self.search;
        // 换了 workspace 后、重搜完之前，不显示另一个根目录的结果。
        let fresh = search.searched.as_ref().is_some_and(|key| key.root == self.files_root());
        if !fresh || search.rows.is_empty() {
            let text = if search.pending.is_some() {
                String::new()
            } else if let Some(error) = &search.error {
                rust_i18n::t!("files.search_failed", error = error).into_owned()
            } else {
                rust_i18n::t!("files.search_empty").into_owned()
            };
            return Some(panel_message(text, cx).into_any_element());
        }
        // 结果统计：按内容找时是命中的行数和文件数，找够上限时说只列出了前面这些。
        let files = search.found.len();
        let lines: usize = search.found.iter().map(|found| found.lines.len()).sum();
        let mut summary = match search.mode {
            SearchMode::Name => rust_i18n::t!("files.search_files", files = files).into_owned(),
            SearchMode::Content => rust_i18n::t!("files.search_summary", count = lines, files = files).into_owned(),
        };
        let limit = match search.mode {
            SearchMode::Name => (files >= MAX_FILES).then_some(MAX_FILES),
            SearchMode::Content => (lines >= MAX_LINES).then_some(MAX_LINES),
        };
        if let Some(limit) = limit {
            summary.push_str(&rust_i18n::t!("files.search_truncated", limit = limit));
        }
        let summary_text = summary;
        // 行号一栏按最大的行号定宽，等宽数字大约 0.6 个字号宽。
        let max_line = search.found.iter().flat_map(|found| &found.lines).map(|line| line.line).max().unwrap_or(0);
        let gutter = (max_line.to_string().len() as f32 * font_size * 0.62).ceil();
        let list = uniform_list(
            "file-search",
            search.rows.len(),
            cx.processor(move |this, range: Range<usize>, _, cx| {
                range.map(|ix| this.render_search_row(ix, font_size, gutter, cx)).collect::<Vec<_>>()
            }),
        )
        .track_scroll(&search.scroll)
        .size_full()
        .p(px(4.));
        let summary = div()
            .id("file-search-summary")
            .role(Role::Label)
            .aria_label(summary_text.clone())
            .flex_none()
            .px(px(10.))
            .pb(px(4.))
            .text_size(FontSize::Body)
            .text_color(cx.theme().foreground.opacity(0.5));
        Some(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(summary.child(summary_text))
                // 列表形式时命中的行也缩进在文件下面，统一报成树；只画看得见的行，辅助工具也只读到这些行。
                .child(
                    div()
                        .id("file-search-results")
                        .role(Role::Tree)
                        .aria_label(rust_i18n::t!("files.search_results").into_owned())
                        .flex_1()
                        .min_h_0()
                        .child(list),
                )
                .into_any_element(),
        )
    }

    fn render_search_row(&self, ix: usize, font_size: f32, gutter: f32, cx: &mut Context<Self>) -> AnyElement {
        let search = &self.search;
        let row = &search.rows[ix];
        let theme = cx.theme();
        let (fg, hit_bg, selected_bg, hover_bg) = (theme.foreground, theme.mix(0.25), theme.mix(0.12), theme.mix(0.06));
        let dim = fg.opacity(0.5);
        // 树形式时文件和目录前面留出箭头的位置，名字对齐。
        let chevron = |expanded: Option<bool>| {
            let icon = expanded.map(|expanded| {
                svg()
                    .path(if expanded { CHEVRON_DOWN_ICON } else { CHEVRON_RIGHT_ICON })
                    .size(px(font_size))
                    .text_color(dim)
            });
            div().flex_none().w(px(font_size)).flex().items_center().children(icon)
        };
        // 报给辅助工具的名字：目录和文件是名字，命中的行是行号和那一行。
        let mut expanded_state = None;
        let (depth, tip, content, label): (_, _, _, SharedString) = match row {
            SearchRow::Dir { path, name, depth, expanded } => {
                expanded_state = Some(*expanded);
                let last = path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
                let content = div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(chevron(Some(*expanded)))
                    .child(img(folder_icon(&last, *expanded)).flex_none().size(px(font_size + 2.)))
                    .child(div().min_w_0().truncate().text_color(fg.opacity(0.85)).child(name.clone()));
                (*depth, path.display().to_string(), content, name.clone())
            }
            SearchRow::File { file, depth } => {
                let found = &search.found[*file];
                let hits = found.lines.len();
                let content = div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .when(search.tree, |item| item.child(chevron(None)))
                    .child(img(file_icon(&found.name)).flex_none().size(px(font_size + 2.)))
                    .child(
                        div()
                            .flex_none()
                            .max_w_full()
                            .truncate()
                            .text_color(fg.opacity(0.85))
                            .child(found.name.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .pl(px(2.))
                            .truncate()
                            .text_size(FontSize::Body)
                            .text_color(dim)
                            .when(!search.tree, |item| item.child(found.dir.clone())),
                    )
                    .when(hits > 0, |item| item.child(div().flex_none().text_color(dim).child(hits.to_string())));
                (*depth, found.path.display().to_string(), content, found.name.clone())
            }
            SearchRow::Line { file, line, depth } => {
                let found = &search.found[*file];
                let line = &found.lines[*line];
                let highlight =
                    HighlightStyle { color: Some(fg), background_color: Some(hit_bg), ..Default::default() };
                let text =
                    StyledText::new(line.text.clone()).with_highlights(line.hit.clone().map(|hit| (hit, highlight)));
                let content = div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_none()
                            .w(px(gutter))
                            .flex()
                            .justify_end()
                            .text_size(FontSize::Body)
                            .text_color(dim)
                            .child(line.line.to_string()),
                    )
                    .child(div().flex_1().min_w_0().truncate().text_color(fg.opacity(0.7)).child(text));
                let label = format!("{}: {}", line.line, line.text).into();
                (*depth, format!("{}:{}", found.path.display(), line.line), content, label)
            }
        };
        let selected = search.selected == Some(ix);
        let view = cx.entity().downgrade();
        // id 按显示的东西取，不按行号：收起目录、重搜时行号会变。`tip` 里目录是相对路径，文件是绝对路径，
        // 命中的行带行号，彼此不重。
        Self::file_row_shell(SharedString::from(tip.clone()), depth, font_size, cx)
            .role(Role::TreeItem)
            .aria_label(label)
            .aria_description(tip.clone())
            .aria_level(depth + 1)
            .aria_selected(selected)
            .when_some(expanded_state, |item, expanded| item.aria_expanded(expanded))
            // 点击要看点了几下，辅助工具按下另外登记，和单击一样。
            .on_a11y_press(view, move |this, _, cx| this.open_search_row(ix, false, cx))
            .map(|item| if selected { item.bg(selected_bg) } else { item.hover(|item| item.bg(hover_bg)) })
            .child(content)
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.open_search_row(ix, event.click_count() >= 2, cx);
            }))
            .tooltip(tooltip(tip, None))
            .into_any_element()
    }
}

impl FileSearch {
    /// 按内容找时的选项，存进窗口存档。
    pub(crate) fn options(&self, cx: &gpui::App) -> SearchOptions {
        SearchOptions {
            match_case: self.match_case,
            whole_word: self.whole_word,
            regex: self.regex,
            include: self.include.read(cx).query().to_owned(),
            exclude: self.exclude.read(cx).query().to_owned(),
        }
    }

    /// 恢复窗口时换上存档里的选项；这时还没有搜索词，不用重搜。
    pub(crate) fn set_options(&mut self, options: SearchOptions, cx: &mut gpui::App) {
        self.match_case = options.match_case;
        self.whole_word = options.whole_word;
        self.regex = options.regex;
        self.include.update(cx, |field, cx| field.set_query(options.include, cx));
        self.exclude.update(cx, |field, cx| field.set_query(options.exclude, cx));
    }

    /// 框里的搜索词。
    pub(crate) fn query(&self, cx: &App) -> String {
        self.field.read(cx).query().to_owned()
    }

    /// 换上 `query`，由调用方接着重搜。
    pub(crate) fn set_query(&self, query: String, cx: &mut App) {
        self.field.update(cx, |field, cx| field.set_query(query, cx));
    }

    pub(crate) fn set_tree(&mut self, tree: bool) {
        self.tree = tree;
        self.selected = None;
        self.relayout();
    }

    /// 搜到了新结果、换了排法或者收起展开了目录，重排 `rows`。
    fn relayout(&mut self) {
        self.rows = layout_rows(&self.found, self.tree, &self.collapsed);
        self.selected = self.selected.filter(|&ix| ix < self.rows.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_names_prefix_first_and_paths_with_a_slash() {
        let files = ["src/main.rs", "src/domain.rs", "README.md", "lib/main/x.rs"].map(PathBuf::from).to_vec();
        let go = AtomicBool::new(false);
        assert_eq!(match_names(&files, "main", &go), [Path::new("src/main.rs"), Path::new("src/domain.rs")]);
        // 带大写字母时区分大小写。
        assert!(match_names(&files, "Main", &go).is_empty());
        assert_eq!(match_names(&files, "readme", &go), [Path::new("README.md")]);
        assert_eq!(match_names(&files, "main/", &go), [Path::new("lib/main/x.rs")]);
        assert!(match_names(&files, "main", &AtomicBool::new(true)).is_empty());
    }

    /// 同一份列表里接着找时不再列，只查要显示的在不在；换一份列表才看得到新文件。
    #[test]
    fn name_search_reuses_the_listing_and_skips_deleted_files() {
        let root = std::env::temp_dir().join(format!("rfs-{}-names", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/a.rs"), "").unwrap();
        fs::write(root.join("src/ab.rs"), "").unwrap();
        let key = SearchKey {
            root: root.clone(),
            query: "a".into(),
            mode: SearchMode::Name,
            options: SearchOptions::default(),
        };
        let names = NameList { root: root.clone(), files: OnceLock::new() };
        let search = |names: &NameList| {
            let found = run_search(&key, names, &AtomicBool::new(false)).unwrap();
            let mut rels: Vec<_> = found.into_iter().map(|found| found.rel).collect();
            rels.sort();
            rels
        };
        assert_eq!(search(&names), [PathBuf::from("src/a.rs"), PathBuf::from("src/ab.rs")]);
        fs::remove_file(root.join("src/ab.rs")).unwrap();
        fs::write(root.join("src/abc.rs"), "").unwrap();
        assert_eq!(search(&names), [PathBuf::from("src/a.rs")]);
        let fresh = NameList { root: root.clone(), files: OnceLock::new() };
        assert_eq!(search(&fresh), [PathBuf::from("src/a.rs"), PathBuf::from("src/abc.rs")]);
        // 取消了的不找。
        assert!(run_search(&key, &fresh, &AtomicBool::new(true)).unwrap().is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn lays_out_rows_as_a_list_or_a_tree() {
        let line = FoundLine { line: 3, text: "x".into(), hit: None };
        let found = vec![
            found(Path::new("/r"), "src/b.rs".into(), vec![line.clone(), line]),
            found(Path::new("/r"), "a.rs".into(), Vec::new()),
        ];
        let none = HashSet::new();
        let list = layout_rows(&found, false, &none);
        assert_eq!(
            list,
            [
                SearchRow::File { file: 0, depth: 0 },
                SearchRow::Line { file: 0, line: 0, depth: 1 },
                SearchRow::Line { file: 0, line: 1, depth: 1 },
                SearchRow::File { file: 1, depth: 0 },
            ]
        );
        let dir = |expanded| SearchRow::Dir { path: "src".into(), name: "src".into(), depth: 0, expanded };
        assert_eq!(
            layout_rows(&found, true, &none),
            [
                dir(true),
                SearchRow::File { file: 0, depth: 1 },
                SearchRow::Line { file: 0, line: 0, depth: 2 },
                SearchRow::Line { file: 0, line: 1, depth: 2 },
                SearchRow::File { file: 1, depth: 0 },
            ]
        );
        let collapsed = HashSet::from([PathBuf::from("src")]);
        assert_eq!(layout_rows(&found, true, &collapsed), [dir(false), SearchRow::File { file: 1, depth: 0 }]);
    }

    fn literal(query: &str) -> Regex {
        matcher(query, &SearchOptions::default()).unwrap()
    }

    #[test]
    fn snippets_trim_indent_and_keep_the_hit_in_view() {
        assert_eq!(snippet("    let foo = 1;", Some(&literal("foo"))), ("let foo = 1;".to_owned(), Some(4..7)));
        assert_eq!(snippet("Foo", Some(&literal("foo"))), ("Foo".to_owned(), Some(0..3)));
        assert_eq!(snippet("Ärger", Some(&literal("ä"))), ("Ärger".to_owned(), Some(0..2)));
        let case = SearchOptions { match_case: true, ..SearchOptions::default() };
        assert_eq!(snippet("foo", Some(&matcher("Foo", &case).unwrap())), ("foo".to_owned(), None));
        // git 认、Rust 的 regex 不认的正则照样列出这一行，只是不标命中处。
        assert_eq!(snippet("  (a)a", None), ("(a)a".to_owned(), None));
        let long = format!("{}needle", "x".repeat(100));
        let (text, hit) = snippet(&long, Some(&literal("needle")));
        assert!(text.starts_with('…'));
        assert_eq!(&text[hit.unwrap()], "needle");
    }

    #[test]
    fn content_search_keeps_spaces_in_the_query() {
        assert_eq!(search_query(SearchMode::Content, "fn "), "fn ");
        assert_eq!(search_query(SearchMode::Content, " a b "), " a b ");
        assert_eq!(search_query(SearchMode::Content, "  \t"), "");
        assert_eq!(search_query(SearchMode::Name, " main.rs "), "main.rs");
        assert_eq!(search_query(SearchMode::Name, "   "), "");
    }
}

#[cfg(test)]
mod option_tests {
    use super::*;

    #[test]
    fn matchers_follow_the_word_and_regex_switches() {
        let word = SearchOptions { whole_word: true, ..SearchOptions::default() };
        assert!(matcher("foo", &word).unwrap().find("foobar").is_none());
        assert!(matcher("a.c", &SearchOptions::default()).unwrap().find("abc").is_none());
        let regex = SearchOptions { regex: true, ..SearchOptions::default() };
        assert!(matcher("a.c", &regex).unwrap().find("abc").is_some());
        assert!(matcher("(", &regex).is_err());
    }

    #[test]
    fn globs_become_pathspecs_matching_at_any_depth() {
        assert_eq!(
            pathspecs("*.ts, ./src/", "dist/**"),
            [
                ":(glob)**/*.ts",
                ":(glob)**/*.ts/**",
                ":(glob)src",
                ":(glob)src/**",
                ":(exclude,glob)**/dist/**",
                ":(exclude,glob)**/dist/**/**",
            ]
        );
        assert!(pathspecs(" , ", "").is_empty());
    }

    #[test]
    fn braces_in_globs_expand_into_separate_pathspecs() {
        assert_eq!(
            pathspecs("*.{ts,tsx}, src/{a,b{c,d}}.rs", ""),
            [
                ":(glob)**/*.ts",
                ":(glob)**/*.ts/**",
                ":(glob)**/*.tsx",
                ":(glob)**/*.tsx/**",
                ":(glob)**/src/a.rs",
                ":(glob)**/src/a.rs/**",
                ":(glob)**/src/bc.rs",
                ":(glob)**/src/bc.rs/**",
                ":(glob)**/src/bd.rs",
                ":(glob)**/src/bd.rs/**",
            ]
        );
        assert_eq!(pathspecs("a{b", ""), [":(glob)**/a{b", ":(glob)**/a{b/**"]);
    }
}
