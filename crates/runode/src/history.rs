//! 命令历史：用户 shell 自己的历史文件，加上 runode 在 shell 集成报告命令运行时记下的命令
//! （带当时的目录和退出码）。输入命令时的灰字建议从这里找，以后的补全也会用它。
//!
//! 全进程共用一份，见 `shared`：第一次用到时在后台线程里读文件，读完之前只有这次运行中
//! 记下的命令。新记下的命令由同一个线程追加到 runode 自己的历史文件，一行一条 JSON。

use std::{
    collections::HashMap,
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, OnceLock, PoisonError, mpsc},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::shell_integration::Shell;

/// 内存里最多留这么多条不同的命令，多出来的丢掉最旧的。
pub const LIMIT: usize = 10_000;
/// 每条命令最多记住它在这么多个目录里用过，多出来的丢掉最久没用的。
const DIRS_PER_COMMAND: usize = 16;
/// runode 自己的历史文件超过这么多行时，读的时候顺便只留最近的 `LIMIT` 行重写一遍。
const FILE_COMPACT_LINES: usize = 2 * LIMIT;

/// 一条命令记录，也是 runode 历史文件里的一行。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub cmd: String,
    /// 命令在哪个目录里运行；shell 的历史文件里没有这一项。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// 退出码；shell 没报告或者历史文件里没有时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    /// 开始运行的时刻，Unix 秒；shell 的历史文件没记时间时为 0。
    #[serde(default)]
    pub ts: u64,
}

impl Entry {
    /// 现在开始运行的一条命令。
    pub fn now(cmd: String, cwd: Option<PathBuf>) -> Self {
        let ts = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        Self { cmd, cwd, exit: None, ts }
    }
}

/// 去重后的一条命令。
struct Item {
    cmd: String,
    /// 用过它的目录，以及在那里最后一次用它的顺序号。
    dirs: Vec<(PathBuf, u64)>,
    /// 最后一次用它的顺序号，越大越新。
    seq: u64,
}

impl Item {
    fn used_in(&mut self, dir: Option<PathBuf>, seq: u64) {
        let Some(dir) = dir else {
            return;
        };
        match self.dirs.iter_mut().find(|(d, _)| *d == dir) {
            Some(entry) => entry.1 = entry.1.max(seq),
            None => {
                self.dirs.push((dir, seq));
                if self.dirs.len() > DIRS_PER_COMMAND
                    && let Some(oldest) = self.dirs.iter().enumerate().min_by_key(|(_, (_, s))| *s).map(|(i, _)| i)
                {
                    self.dirs.swap_remove(oldest);
                }
            }
        }
    }

    fn seq_in(&self, dir: &Path) -> Option<u64> {
        self.dirs.iter().find(|(d, _)| d == dir).map(|(_, seq)| *seq)
    }
}

/// 去重后的命令历史，同一条命令只留一份，按最后一次用的先后排。
#[derive(Default)]
pub struct History {
    /// 从旧到新。
    items: Vec<Item>,
    /// 下一次使用的顺序号。
    next_seq: u64,
    /// 每变一次加一，缓存了查找结果的一方据此知道要重查。
    generation: u64,
    /// 历史文件还没读完时，这次运行中记下的命令；读完后排在文件里的命令之后。
    early: Option<Vec<Entry>>,
}

impl History {
    /// 从旧到新的一串记录建立历史；同一条命令出现多次时按最后一次的位置排。
    pub fn from_entries(entries: Vec<Entry>) -> Self {
        let total = entries.len() as u64;
        // 从新往旧走，每条命令第一次遇到时就是它最后一次用的位置；凑够 `LIMIT` 条以后，
        // 更旧的只用来补充已有命令用过的目录。
        let mut items: Vec<Item> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        for (seq, entry) in (0..total).rev().zip(entries.into_iter().rev()) {
            match index.get(&entry.cmd) {
                Some(&i) => items[i].used_in(entry.cwd, seq),
                None if items.len() < LIMIT => {
                    let mut item = Item { cmd: entry.cmd.clone(), dirs: Vec::new(), seq };
                    item.used_in(entry.cwd, seq);
                    index.insert(entry.cmd, items.len());
                    items.push(item);
                }
                None => {}
            }
        }
        items.reverse();
        Self { items, next_seq: total, generation: 1, early: None }
    }

    /// 还在等历史文件读完的空历史，见 `finish_loading`。
    fn loading() -> Self {
        Self { early: Some(Vec::new()), ..Self::default() }
    }

    /// 取走到现在为止这次运行中记下的命令，读完的历史文件要接上它们。之后记下的命令
    /// 继续攒着，由 `finish_loading` 补上。
    fn take_early(&mut self) -> Vec<Entry> {
        self.early.as_mut().map(std::mem::take).unwrap_or_default()
    }

    /// 换成在锁外建好的历史（历史文件加上 `take_early` 取走的命令），再补上那之后记下的。
    fn finish_loading(&mut self, mut loaded: History) {
        for entry in self.early.take().unwrap_or_default() {
            loaded.push(entry);
        }
        loaded.generation = loaded.generation.max(self.generation + 1);
        *self = loaded;
    }

    /// 记下刚运行的一条命令，它成为最新的一条。
    pub fn push(&mut self, entry: Entry) {
        if let Some(early) = &mut self.early {
            early.push(entry.clone());
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        let mut item = match self.items.iter().rposition(|item| item.cmd == entry.cmd) {
            Some(i) => self.items.remove(i),
            None => Item { cmd: entry.cmd, dirs: Vec::new(), seq },
        };
        item.seq = seq;
        item.used_in(entry.cwd, seq);
        self.items.push(item);
        if self.items.len() > LIMIT {
            self.items.remove(0);
        }
        self.generation += 1;
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// 历史里的各条命令（已去重），从旧到新。
    pub fn commands(&self) -> impl Iterator<Item = &str> {
        self.items.iter().map(|item| item.cmd.as_str())
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// 以 `prefix` 开头、又比它长的一条命令：先找在 `cwd` 里最近用过的，没有再找在任何
    /// 目录里最近用过的。区分大小写；`prefix` 去掉开头空白后为空时不找。
    ///
    /// 每次都扫一遍整个历史；界面上用带缓存的 `Suggester`，这里是它的参照，只在测试里用。
    #[cfg(test)]
    pub fn suggest(&self, prefix: &str, cwd: Option<&Path>) -> Option<&str> {
        if prefix.trim_start().is_empty() {
            return None;
        }
        let candidates: Vec<usize> = (0..self.items.len()).rev().filter(|&i| completes(&self.items[i].cmd, prefix)).collect();
        self.best(&candidates, cwd).map(|i| self.items[i].cmd.as_str())
    }

    /// 从新到旧排好的候选里挑一条：在 `cwd` 里最后一次用得最晚的，没有就是最新的那条。
    fn best(&self, candidates: &[usize], cwd: Option<&Path>) -> Option<usize> {
        let in_cwd = cwd.and_then(|cwd| {
            candidates
                .iter()
                .filter_map(|&i| Some((self.items[i].seq_in(cwd)?, i)))
                .max_by_key(|(seq, _)| *seq)
                .map(|(_, i)| i)
        });
        in_cwd.or_else(|| candidates.first().copied())
    }
}

/// `cmd` 能不能作为 `prefix` 的建议：以它开头、比它长，并且是一行（不含换行等控制字符，
/// 接受建议时这些字符写进 shell 会被当成按键）。
fn completes(cmd: &str, prefix: &str) -> bool {
    cmd.len() > prefix.len() && cmd.starts_with(prefix) && !cmd.contains(char::is_control)
}

/// 按前缀查建议，并缓存这次的全部候选：输入只是在后面追加了字符时，下次在上次的候选里
/// 过滤，不必把整个历史再扫一遍。
#[derive(Default)]
pub struct Suggester {
    /// 候选对应的历史版本，历史变了就作废。
    generation: u64,
    prefix: String,
    /// `prefix` 的全部候选在历史里的下标，从新到旧。
    candidates: Vec<usize>,
}

impl Suggester {
    /// 和 `History::suggest` 的结果一样，只是返回接在 `prefix` 后面的那部分。
    pub fn suggest(&mut self, history: &History, prefix: &str, cwd: Option<&Path>) -> Option<String> {
        if prefix.trim_start().is_empty() {
            self.prefix.clear();
            self.candidates.clear();
            return None;
        }
        let narrowing = self.generation == history.generation
            && !self.prefix.is_empty()
            && prefix.starts_with(self.prefix.as_str());
        if narrowing {
            self.candidates.retain(|&i| completes(&history.items[i].cmd, prefix));
        } else {
            self.candidates.clear();
            self.candidates
                .extend((0..history.items.len()).rev().filter(|&i| completes(&history.items[i].cmd, prefix)));
        }
        self.generation = history.generation;
        prefix.clone_into(&mut self.prefix);
        let i = history.best(&self.candidates, cwd)?;
        Some(history.items[i].cmd[prefix.len()..].to_owned())
    }
}

/// 建议里的下一个词：先带上开头的空白和 `/`，再到下一个空白或 `/` 为止；后面紧跟着 `/`
/// 时把它也带上，接受一段路径时停在目录分隔处。
pub fn next_word(rest: &str) -> &str {
    let is_sep = |c: char| c.is_whitespace() || c == '/';
    let lead = rest.find(|c: char| !is_sep(c)).unwrap_or(rest.len());
    let word = rest[lead..].find(is_sep).map_or(rest.len(), |i| lead + i);
    let end = if rest[word..].starts_with('/') { word + 1 } else { word };
    &rest[..end]
}

/// 值得记下的命令：开头有空白的不记，这是几种 shell 共同的「别记进历史」写法。
pub fn worth_recording(cmd: &str) -> bool {
    !cmd.trim().is_empty() && !cmd.starts_with(char::is_whitespace)
}

struct Shared {
    history: Mutex<History>,
    writer: mpsc::Sender<Entry>,
}

fn shared_state() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(|| {
        let (writer, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("history".into())
            .spawn(move || run_background(rx));
        if let Err(err) = spawned {
            tracing::warn!("failed to start the history thread: {err}");
        }
        Shared { history: Mutex::new(History::loading()), writer }
    })
}

/// 现在就开始在后台读历史文件（如果还没开始）。
pub fn load_in_background() {
    shared_state();
}

/// 全进程共用的命令历史；第一次调用时开始在后台读历史文件。
pub fn shared() -> MutexGuard<'static, History> {
    shared_state().history.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 记下一条运行过的命令：加进共用的历史，并在后台追加到历史文件。
pub fn record(entry: Entry) {
    if !worth_recording(&entry.cmd) {
        return;
    }
    let state = shared_state();
    state.history.lock().unwrap_or_else(PoisonError::into_inner).push(entry.clone());
    // 后台线程没起来时只是不写文件。
    let _ = state.writer.send(entry);
}

/// 后台线程：先读历史文件，再把陆续记下的命令追加到 runode 自己的历史文件。
fn run_background(rx: mpsc::Receiver<Entry>) {
    let own = own_history_path();
    let mut entries = std::env::var("SHELL").ok().and_then(|shell| read_shell_history(&shell)).unwrap_or_default();
    if let Some(path) = &own {
        match read_own_history(path) {
            Ok(own) => entries.extend(own),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => tracing::warn!("failed to read {}: {err}", path.display()),
        }
    }
    // shell 的历史和 runode 的按时间排在一起；没有时间的（记为 0）排在最前，保持原来的顺序。
    entries.sort_by_key(|entry| entry.ts);
    // 建索引要一会儿，放在锁外做，界面查建议时不必等它。
    let lock = || shared_state().history.lock().unwrap_or_else(PoisonError::into_inner);
    entries.extend(lock().take_early());
    let loaded = History::from_entries(entries);
    lock().finish_loading(loaded);

    let Some(path) = own else {
        return;
    };
    for entry in rx {
        if let Err(err) = append(&path, &entry) {
            tracing::warn!("failed to write {}: {err}", path.display());
        }
    }
}

fn own_history_path() -> Option<PathBuf> {
    runode_dirs::Dirs::from_env().history_file()
}

/// 读 runode 自己的历史文件，坏掉的行跳过。行数太多时只留最近的 `LIMIT` 行重写一遍。
fn read_own_history(path: &Path) -> io::Result<Vec<Entry>> {
    let text = fs::read_to_string(path)?;
    let lines: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty()).collect();
    if lines.len() > FILE_COMPACT_LINES {
        let kept = lines[lines.len() - LIMIT..].join("\n") + "\n";
        let tmp = path.with_extension("jsonl.tmp");
        let written = private_file(fs::OpenOptions::new().write(true).create(true).truncate(true))
            .open(&tmp)
            .and_then(|mut file| file.write_all(kept.as_bytes()))
            .and_then(|()| fs::rename(&tmp, path));
        if let Err(err) = written {
            tracing::warn!("failed to compact {}: {err}", path.display());
        }
    }
    Ok(parse_own(&lines))
}

fn parse_own(lines: &[&str]) -> Vec<Entry> {
    lines.iter().filter_map(|line| serde_json::from_str(line).ok()).collect()
}

fn append(path: &Path, entry: &Entry) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut line = serde_json::to_vec(entry)?;
    line.push(b'\n');
    // 整行一次写出，以追加方式打开，几个窗口同时写也不会把行拆开。
    private_file(fs::OpenOptions::new().create(true).append(true)).open(path)?.write_all(&line)
}

/// 命令里可能有密码之类的东西：新建的历史文件只让用户自己读写。已经存在的文件不改权限。
fn private_file(options: &mut fs::OpenOptions) -> &mut fs::OpenOptions {
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(options, 0o600);
    options
}

/// 读用户登录 shell（`$SHELL`）自己的历史文件，从旧到新。认不出 shell 或读不了时为 `None`。
fn read_shell_history(shell: &str) -> Option<Vec<Entry>> {
    let name = Path::new(shell).file_name()?.to_str()?.trim_start_matches('-');
    let shell = Shell::from_name(name)?;
    let path = shell_history_path(shell)?;
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) => {
            if err.kind() != io::ErrorKind::NotFound {
                tracing::warn!("failed to read {}: {err}", path.display());
            }
            return None;
        }
    };
    Some(match shell {
        Shell::Zsh => parse_zsh(&bytes),
        Shell::Bash => parse_bash(&String::from_utf8_lossy(&bytes)),
        Shell::Fish => parse_fish(&String::from_utf8_lossy(&bytes)),
    })
}

fn shell_history_path(shell: Shell) -> Option<PathBuf> {
    let env = |key: &str| std::env::var_os(key).filter(|v| !v.is_empty()).map(PathBuf::from);
    let home = runode_dirs::Dirs::from_env().home?;
    Some(match shell {
        Shell::Zsh => env("HISTFILE").unwrap_or_else(|| env("ZDOTDIR").unwrap_or(home).join(".zsh_history")),
        Shell::Bash => env("HISTFILE").unwrap_or_else(|| home.join(".bash_history")),
        Shell::Fish => env("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share")).join("fish/fish_history"),
    })
}

/// zsh 的历史文件。非 ASCII 字节可能被「元化」：0x83 后面跟着原字节异或 0x20。开了
/// EXTENDED_HISTORY 时每条前面有 `: 开始时间:耗时;`；多行命令的换行前有一个反斜杠。
fn parse_zsh(bytes: &[u8]) -> Vec<Entry> {
    const META: u8 = 0x83;
    let mut plain = Vec::with_capacity(bytes.len());
    let mut iter = bytes.iter();
    while let Some(&b) = iter.next() {
        if b == META {
            if let Some(&next) = iter.next() {
                plain.push(next ^ 0x20);
            }
        } else {
            plain.push(b);
        }
    }
    let text = String::from_utf8_lossy(&plain);
    let mut entries = Vec::new();
    let mut pending: Option<String> = None;
    for line in text.lines() {
        let record = match pending.take() {
            Some(mut record) => {
                record.push('\n');
                record.push_str(line);
                record
            }
            None => line.to_owned(),
        };
        if let Some(head) = record.strip_suffix('\\') {
            pending = Some(head.to_owned());
            continue;
        }
        entries.extend(zsh_entry(record));
    }
    entries.extend(pending.and_then(zsh_entry));
    entries
}

fn zsh_entry(record: String) -> Option<Entry> {
    let (ts, cmd) = match record
        .strip_prefix(": ")
        .and_then(|rest| rest.split_once(';'))
        .and_then(|(meta, cmd)| Some((meta.split_once(':')?.0.trim().parse::<u64>().ok()?, cmd)))
    {
        Some((ts, cmd)) => (ts, cmd.to_owned()),
        None => (0, record),
    };
    (!cmd.trim().is_empty()).then_some(Entry { cmd, cwd: None, exit: None, ts })
}

/// bash 的历史文件：一行一条；设了 HISTTIMEFORMAT 时命令前面有一行 `#开始时间`。
fn parse_bash(text: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut ts = None;
    for line in text.lines() {
        if let Some(stamp) = line.strip_prefix('#')
            && !stamp.is_empty()
            && let Ok(stamp) = stamp.parse::<u64>()
        {
            ts = Some(stamp);
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        entries.push(Entry { cmd: line.to_owned(), cwd: None, exit: None, ts: ts.take().unwrap_or(0) });
    }
    entries
}

/// fish 的历史文件：每条是 `- cmd: 命令`，下面缩进的 `when: 开始时间`。命令里的换行和
/// 反斜杠写成 `\n`、`\\`。
fn parse_fish(text: &str) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    for line in text.lines() {
        if let Some(cmd) = line.strip_prefix("- cmd: ") {
            entries.push(Entry { cmd: unescape_fish(cmd), cwd: None, exit: None, ts: 0 });
        } else if let Some(when) = line.trim_start().strip_prefix("when: ")
            && let Some(last) = entries.last_mut()
        {
            last.ts = when.trim().parse().unwrap_or(0);
        }
    }
    entries.retain(|entry| !entry.cmd.trim().is_empty());
    entries
}

fn unescape_fish(cmd: &str) -> String {
    let mut out = String::with_capacity(cmd.len());
    let mut chars = cmd.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(cmd: &str, cwd: Option<&str>) -> Entry {
        Entry { cmd: cmd.into(), cwd: cwd.map(Into::into), exit: None, ts: 0 }
    }

    fn cmds(entries: &[Entry]) -> Vec<(&str, u64)> {
        entries.iter().map(|e| (e.cmd.as_str(), e.ts)).collect()
    }

    #[test]
    fn zsh_extended_history_and_continuation_lines() {
        let text = b": 1700000000:0;git status\n\
                     : 1700000005:2;for f in a b; do\\\n  echo $f\\\ndone\n\
                     plain command\n";
        assert_eq!(
            cmds(&parse_zsh(text)),
            [("git status", 1700000000), ("for f in a b; do\n  echo $f\ndone", 1700000005), ("plain command", 0)]
        );
    }

    #[test]
    fn zsh_metafied_bytes_are_decoded() {
        // 「à」是 0xC3 0xA0；zsh 把 0x83 到 0xA2 这些字节写成 0x83 加上原字节异或 0x20。
        let text = b": 1:0;echo voil\xc3\x83\x80\n";
        assert_eq!(cmds(&parse_zsh(text)), [("echo voilà", 1)]);
    }

    #[test]
    fn bash_history_with_timestamp_lines() {
        let text = "ls -la\n#1700000000\ncargo build\n\n#notanumber\n";
        assert_eq!(cmds(&parse_bash(text)), [("ls -la", 0), ("cargo build", 1700000000), ("#notanumber", 0)]);
    }

    #[test]
    fn fish_history_format() {
        let text = "- cmd: git log\n  when: 1700000000\n- cmd: echo a\\nb \\\\ c\n  when: 1700000001\n  paths:\n    - b\n";
        assert_eq!(cmds(&parse_fish(text)), [("git log", 1700000000), ("echo a\nb \\ c", 1700000001)]);
    }

    #[test]
    fn own_history_skips_bad_lines() {
        let lines = [r#"{"cmd":"make","cwd":"/src","exit":0,"ts":5}"#, "not json", r#"{"cmd":"ls"}"#];
        assert_eq!(
            parse_own(&lines),
            [Entry { cmd: "make".into(), cwd: Some("/src".into()), exit: Some(0), ts: 5 }, entry("ls", None)]
        );
    }

    #[test]
    fn entries_round_trip_through_json() {
        let e = Entry { cmd: "cargo test".into(), cwd: Some("/x".into()), exit: Some(1), ts: 9 };
        let line = serde_json::to_string(&e).unwrap();
        assert_eq!(line, r#"{"cmd":"cargo test","cwd":"/x","exit":1,"ts":9}"#);
        assert_eq!(serde_json::from_str::<Entry>(&line).unwrap(), e);
    }

    #[test]
    fn same_directory_wins_then_most_recent_anywhere() {
        let history = History::from_entries(vec![
            entry("git status", Some("/a")),
            entry("git stash", Some("/b")),
            entry("git switch main", None),
        ]);
        assert_eq!(history.suggest("git s", Some(Path::new("/a"))), Some("git status"));
        assert_eq!(history.suggest("git s", Some(Path::new("/b"))), Some("git stash"));
        assert_eq!(history.suggest("git s", Some(Path::new("/c"))), Some("git switch main"));
        assert_eq!(history.suggest("git s", None), Some("git switch main"));
    }

    #[test]
    fn suggestions_are_case_sensitive_and_strictly_longer() {
        let history = History::from_entries(vec![entry("ls", None), entry("Make", None)]);
        assert_eq!(history.suggest("ls", None), None);
        assert_eq!(history.suggest("l", None), Some("ls"));
        assert_eq!(history.suggest("ma", None), None);
        assert_eq!(history.suggest("  ", None), None);
        assert_eq!(history.suggest("", None), None);
    }

    #[test]
    fn multi_line_commands_are_never_suggested() {
        let history = History::from_entries(vec![entry("echo a", None), entry("echo b\necho c", None)]);
        assert_eq!(history.suggest("echo", None), Some("echo a"));
    }

    #[test]
    fn duplicates_collapse_and_keep_every_directory() {
        let mut history = History::from_entries(vec![
            entry("make", Some("/a")),
            entry("make test", Some("/b")),
            entry("make", Some("/c")),
        ]);
        assert_eq!(history.len(), 2);
        // `make` 在 /a 用过，那里优先它；/b 里用过的是 `make test`。
        assert_eq!(history.suggest("mak", Some(Path::new("/a"))), Some("make"));
        assert_eq!(history.suggest("mak", Some(Path::new("/b"))), Some("make test"));
        history.push(entry("make test", Some("/a")));
        assert_eq!(history.len(), 2);
        assert_eq!(history.suggest("mak", Some(Path::new("/a"))), Some("make test"));
        assert_eq!(history.suggest("mak", None), Some("make test"));
    }

    #[test]
    fn history_keeps_only_the_newest_commands() {
        let mut history = History::from_entries((0..LIMIT + 5).map(|i| entry(&format!("cmd {i}"), None)).collect());
        assert_eq!(history.len(), LIMIT);
        assert_eq!(history.items[0].cmd, "cmd 5");
        assert_eq!(history.suggest("cmd 1000", None), Some("cmd 10004"));
        history.push(entry("new", None));
        assert_eq!(history.len(), LIMIT);
        assert_eq!(history.items[0].cmd, "cmd 6");
    }

    #[cfg(unix)]
    #[test]
    fn new_history_files_are_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("runode-history-test-{}", std::process::id()));
        let path = dir.join("history.jsonl");
        append(&path, &entry("ls", None)).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        // 压缩时重写的临时文件也一样。
        let lines: Vec<String> = (0..FILE_COMPACT_LINES + 1).map(|i| format!(r#"{{"cmd":"c{i}"}}"#)).collect();
        fs::write(&path, lines.join("\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(read_own_history(&path).unwrap().len(), FILE_COMPACT_LINES + 1);
        let compacted = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let kept = fs::read_to_string(&path).unwrap().lines().count();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(mode, 0o600);
        assert_eq!(compacted, 0o600);
        assert_eq!(kept, LIMIT);
    }

    #[test]
    fn commands_recorded_before_loading_come_after_the_files() {
        let mut history = History::loading();
        history.push(entry("git push", Some("/a")));
        let mut files = vec![entry("git pull", Some("/a")), entry("git push", None), entry("git prune", None)];
        files.extend(history.take_early());
        // 在锁外建索引的这段时间里又记下一条。
        history.push(entry("git pull", None));
        let before = history.generation();
        history.finish_loading(History::from_entries(files));
        assert!(history.generation() > before);
        assert_eq!(history.len(), 3);
        assert_eq!(history.suggest("git p", None), Some("git pull"));
        assert_eq!(history.suggest("git pu", Some(Path::new("/a"))), Some("git push"));
        // 读完以后不再攒着。
        history.push(entry("ls", None));
        assert!(history.early.is_none());
    }

    #[test]
    fn suggester_matches_a_full_scan_while_narrowing() {
        let mut history = History::from_entries(vec![
            entry("cargo build", Some("/a")),
            entry("cargo test", None),
            entry("cat file", None),
        ]);
        let mut suggester = Suggester::default();
        let a = Some(Path::new("/a"));
        assert_eq!(suggester.suggest(&history, "c", a), Some("argo build".into()));
        assert_eq!(suggester.suggest(&history, "ca", None), Some("t file".into()));
        assert_eq!(suggester.suggest(&history, "car", None), Some("go test".into()));
        assert_eq!(suggester.suggest(&history, "cargo t", None), Some("est".into()));
        // 删掉字符后前缀变短，要重新扫。
        assert_eq!(suggester.suggest(&history, "cargo ", a), Some("build".into()));
        // 历史变了，旧的候选作废。
        history.push(entry("cargo clippy", Some("/a")));
        assert_eq!(suggester.suggest(&history, "cargo c", a), Some("lippy".into()));
        assert_eq!(suggester.suggest(&history, "cargo b", a), Some("uild".into()));
        assert_eq!(suggester.suggest(&history, " ", a), None);
    }

    #[test]
    fn next_word_stops_at_spaces_and_path_separators() {
        assert_eq!(next_word(" status --short"), " status");
        assert_eq!(next_word("atus --short"), "atus");
        assert_eq!(next_word("src/runode/main.rs"), "src/");
        assert_eq!(next_word("/usr/bin"), "/usr/");
        assert_eq!(next_word("bin"), "bin");
        assert_eq!(next_word("  "), "  ");
    }

    #[test]
    fn commands_starting_with_a_space_are_not_recorded() {
        assert!(worth_recording("ls"));
        assert!(!worth_recording(" secret"));
        assert!(!worth_recording("   "));
    }
}
