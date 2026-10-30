//! 界面这边的配置：把加载好的配置和命令的快捷键（`TaskKeys`，在 `tasks.json` 里）放进全局，这些文件
//! 保存后自动重载，系统深浅色变了时换到对应的主题，以及用文本编辑器打开配置文件。在界面里改设置见 `settings`。配置的读取和解析见 `runode_config`。

use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use futures::StreamExt as _;
use gpui::{App, Global, WindowAppearance};
use notify::Watcher as _;
use runode_config::{Config, ConfigFile};

/// 当前生效的配置。视图通过 `observe_global` 在重载后重新应用。
pub struct AppConfig(pub Arc<Config>);

impl Global for AppConfig {}

/// 盯不了目录时，退回隔这么久比一次配置文件的修改时间。
const WATCH_INTERVAL: Duration = Duration::from_secs(1);
/// 目录里有动静后先等这么久再比。
pub(crate) const WATCH_DEBOUNCE: Duration = Duration::from_millis(100);

/// 加载配置并开始监视配置文件，保存后自动重载。启动时后台线程已经读过一遍（见
/// `host_client::take_config`），拿它对得上系统外观时直接用，不再读。
pub fn install(cx: &mut App) {
    let dark = system_is_dark(cx);
    let loaded = crate::host_client::take_config().filter(|(config, _)| config.fits_appearance(dark));
    match loaded {
        Some((mut config, before)) => {
            config.dark = dark;
            let seen = seen_after(&before, &config);
            apply(cx, config, seen);
        }
        None => reload(cx),
    }
    if let Some(path) = runode_config::config_path()
        && let Err(err) = runode_config::create_config_file(&path)
    {
        tracing::warn!("failed to create {}: {err}", path.display());
    }
    let (changed, mut events) = futures::channel::mpsc::unbounded::<()>();
    let mut files = {
        let changed = changed.clone();
        FileWatch::new(move || {
            changed.unbounded_send(()).ok();
        })
    }
    .inspect_err(|err| tracing::warn!("cannot watch the config files, checking them every second: {err}"))
    .ok();
    if files.is_some() {
        // 第一轮挑出要盯的目录；之后每次重载都再挑一遍，新配置可能多了文件（比如换了主题）。
        changed.unbounded_send(()).ok();
        cx.observe_global::<AppConfig>(move |_| {
            changed.unbounded_send(()).ok();
        })
        .detach();
    }
    cx.spawn(async move |cx| {
        // 要盯的目录都盯上了。有盯不上的（比如系统的监视数用完了）时那边改了没有动静，照样隔一会儿比一次，
        // 顺便重试。
        let mut all_watched = true;
        loop {
            if files.is_some() && all_watched {
                if events.next().await.is_none() {
                    return;
                }
                // 保存一次常连着来几个事件，攒一下再比，也免得读到写了一半的文件。
                cx.background_executor().timer(WATCH_DEBOUNCE).await;
            } else {
                cx.background_executor().timer(WATCH_INTERVAL).await;
            }
            while events.try_recv().is_ok() {}
            // 解符号链接、找已有的目录、取修改时间都在单独的线程上做：`config-file` 引入的文件可以在挂死的
            // 网络卷上。先盯上再比：比完才盯上的话，中间那一下保存既没比到、也没有事件。新配置要多盯的目录在
            // 重载后的下一轮盯上（重载会再发一个事件），那一轮盯上以后还会再比一次。起不了线程时不再盯目录，
            // 退回隔一会儿比一次。
            let config = cx.update(|cx| cx.global::<AppConfig>().0.clone());
            let (back, watched, stamp) = crate::fs_probe::off_thread(
                {
                    let config = config.clone();
                    move || {
                        let watched = files.as_mut().is_none_or(|files| files.follow(&watch_paths(&config)));
                        (files, watched, Some(watch_stamp(&config)))
                    }
                },
                (None, false, None),
            )
            .await;
            (files, all_watched) = (back, watched);
            cx.update(|cx| {
                // 比的时候设置页写回、重载过的，按新配置记的修改时间对不上这一份，等它发的那个事件下一轮再比。
                let current = Arc::ptr_eq(&cx.global::<AppConfig>().0, &config);
                if let Some(stamp) = stamp
                    && current
                    && cx.try_global::<Seen>().is_some_and(|seen| seen.0 != stamp)
                {
                    reload(cx);
                }
            });
        }
    })
    .detach();
}

/// 盯着一组文件所在的目录，有动静就调 `changed`，由调用方比修改时间看是不是真变了。盯目录不盯文件：
/// 编辑器保存时常是写好临时文件再改名换上，盯着文件本身会跟丢，文件起初不存在时也盯不了。
pub(crate) struct FileWatch {
    watcher: notify::RecommendedWatcher,
    /// 文件所在的目录还不存在时，只在这几个目录（配置目录、数据目录）里面往上找已有的目录来盯。
    roots: Vec<PathBuf>,
    /// 正盯着的目录，都是真实路径。
    dirs: Vec<PathBuf>,
}

impl FileWatch {
    /// 先什么都不盯，由 `follow` 给出要盯的文件。
    pub(crate) fn new(changed: impl Fn() + Send + 'static) -> notify::Result<Self> {
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            // 出错（比如事件太多丢了一些）时也叫调用方比一遍。
            if !event.is_ok_and(|event| event.kind.is_access()) {
                changed();
            }
        })?;
        let dirs = runode_paths::Dirs::from_env();
        Ok(Self { watcher, roots: [dirs.config, dirs.data].into_iter().flatten().collect(), dirs: Vec::new() })
    }

    /// 改成盯着 `paths` 所在的目录（见 `watch_dirs`）。盯不上的下次再试，这时返回 false。
    pub(crate) fn follow(&mut self, paths: &[PathBuf]) -> bool {
        let wanted = watch_dirs(paths, &self.roots);
        if wanted == self.dirs {
            return true;
        }
        for dir in self.dirs.iter().filter(|dir| !wanted.contains(dir)) {
            let _ = self.watcher.unwatch(dir);
        }
        let mut watched = Vec::new();
        let all = wanted.len();
        for dir in wanted {
            let ok = self.dirs.contains(&dir)
                || self
                    .watcher
                    .watch(&dir, notify::RecursiveMode::NonRecursive)
                    .inspect_err(|err| tracing::warn!("cannot watch {}: {err}", dir.display()))
                    .is_ok();
            if ok {
                watched.push(dir);
            }
        }
        self.dirs = watched;
        self.dirs.len() == all
    }

    /// 正盯着的目录（真实路径）。要盯的文件没变，盯的目录也会变：文件所在的目录建出来了、符号链接改指别处。
    pub(crate) fn watching(&self) -> &[PathBuf] {
        &self.dirs
    }
}

/// 盯哪些目录才看得到 `paths` 变了，按真实路径去重：每个文件所在的目录；是符号链接时还有它指向的文件
/// 所在的目录，改那边的文件时链接所在的目录没有动静。目录还不存在时盯已有的上层，建出来就看得到，但只在
/// `roots` 里面往上找：家目录、`~/Library/Application Support` 这类地方别的程序一直在写，盯着白白被叫醒。
///
/// shortcut: `roots` 外面还不存在的目录（比如还没装 Ghostty 时它在 Application Support 下的目录）建出来时
/// 看不到，要等盯着的目录里再有动静才补上；真有人抱怨再给这种目录加慢速轮询。
fn watch_dirs(paths: &[PathBuf], roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for path in paths {
        let target = std::fs::canonicalize(path).ok();
        for file in std::iter::once(path).chain(&target) {
            let Some(parent) = file.parent() else { continue };
            let existing = parent
                .ancestors()
                .find(|dir| dir.is_dir())
                .filter(|dir| *dir == parent || roots.iter().any(|root| dir.starts_with(root)));
            if let Some(dir) = existing {
                let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_owned());
                if !dirs.contains(&dir) {
                    dirs.push(dir);
                }
            }
        }
    }
    dirs
}

/// 各配置文件和看到的修改时间（不存在时为 `None`），见 `watch_stamp`。
pub(crate) type Stamps = Vec<(PathBuf, Option<SystemTime>)>;

/// 上次加载配置时各配置文件的修改时间，读之前取的（见 `seen_after`）。每次加载后按新配置重新记录：
/// 重载可能引入新的文件（比如换了主题），设置页写回后也会自己重载，这样监视的那一轮不会因为这些
/// 再重载一次。
struct Seen(Stamps);

impl Global for Seen {}

/// 重新读取全部配置文件并广播给各视图。
pub fn reload(cx: &mut App) {
    let dark = system_is_dark(cx);
    let before = match cx.try_global::<AppConfig>() {
        Some(config) => watch_stamp(&config.0),
        // 第一次读时还没有配置，按默认配置要盯的文件取。
        None => watch_stamp(&Config::default()),
    };
    let config = Config::load(dark);
    let seen = seen_after(&before, &config);
    apply(cx, config, seen);
}

/// 让 `config` 生效并广播给各视图，`seen` 记作读它时各配置文件的修改时间。
fn apply(cx: &mut App, config: Config, seen: Stamps) {
    // 先换语言再广播，观察配置的菜单和视图重画时就是新语言。
    crate::i18n::set(&crate::i18n::from_config(config.language.as_deref()));
    // 宿主先换主题，视图等它在各个会话的输出流里标出位置后再跟着换。
    crate::host_client::configure(&config);
    cx.set_global(Seen(seen));
    // 在配置之前换上：装快捷键的跟着配置重载，那时读到的已经是新的。
    let task_keys = crate::window::TaskKeys::read().or_else(|| cx.try_global::<crate::window::TaskKeys>().cloned());
    cx.set_global(task_keys.unwrap_or_default());
    cx.set_global(runode_ui::font_size::UiFontSize(config.ui_font_size));
    // 主题 token 的初值；各窗口画的时候再换成自己的配色，见 `WindowView::follow_colors`。
    let palette = runode_ui::theme::ansi_colors(&config.palette);
    cx.set_global(runode_ui::theme::Theme::new(config.foreground, config.background, palette));
    cx.set_global(AppConfig(Arc::new(config)));
}

/// 把 runode 配置文件里 `key` 的值换成 `value` 并重载配置，见 `write_values`。
pub fn set(key: &str, value: &str, cx: &mut App) -> anyhow::Result<()> {
    let path = runode_config::config_path().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    write_values(&path, key, &[value.to_owned()], cx)?;
    Ok(())
}

/// 把配置文件 `path` 里 `key` 的值换成 `values`（为空就是删掉）并重载配置，文件里别的行原样留着，
/// 返回写好的文件。先重新读一遍文件，别盖掉在编辑器里刚改的。
pub fn write_values(path: &Path, key: &str, values: &[String], cx: &mut App) -> io::Result<ConfigFile> {
    let mut file = ConfigFile::read(path)?;
    file.set(key, values);
    file.write(path)?;
    reload(cx);
    Ok(file)
}

/// 逗号隔开、各层叠加的列表键（`status-bar-hidden`、`agent-notifications-exclude`）要写回 runode 配置文件的
/// 各行：`written` 是文件里原来写的，`current` 是各层叠出来生效的名字，`values` 是要换成的。下面一层
/// （Ghostty 的配置）里有的名字，本文件删掉也还在，而读进来的配置已去重、看不出是不是两层都写了；所以要去掉
/// 生效的名字，或文件里已有清空的空行时，前面先写一行空值清掉别处加的。只加名字时不用。
///
/// shortcut: `config-file` 引入的文件排在本文件的各行后面，那里加的这一行清不掉。
pub(crate) fn clear_first(written: &[String], current: &[String], mut values: Vec<String>) -> Vec<String> {
    let names = |values: &[String]| -> Vec<String> {
        values
            .iter()
            .flat_map(|value| value.split(','))
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let kept = names(&values);
    let cleared = written.iter().any(String::is_empty);
    let dropped = names(current).iter().any(|name| !kept.contains(name));
    if cleared || dropped {
        values.insert(0, String::new());
    }
    values
}

/// 系统深浅色变了就重载，让 `theme = light:A,dark:B` 换到对应的主题。每个窗口都会
/// 收到外观变化，第一个窗口重载后外观已经对上，其余窗口直接跳过。
pub fn follow_appearance(cx: &mut App) {
    if system_is_dark(cx) != cx.global::<AppConfig>().0.dark {
        reload(cx);
    }
}

fn system_is_dark(cx: &App) -> bool {
    matches!(cx.window_appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark)
}

/// 要盯的文件：`config` 的各配置文件，再加上存着命令快捷键的 `tasks.json`。
fn watch_paths(config: &Config) -> Vec<PathBuf> {
    let mut paths = config.watch_paths();
    paths.extend(runode_paths::Dirs::from_env().tasks_file());
    paths
}

/// 所有可能的配置文件的修改时间。还不存在的文件也算在内，新建配置文件同样会触发重载。
pub(crate) fn watch_stamp(config: &Config) -> Stamps {
    seen_after(&[], config)
}

/// 读出 `config` 之后，按它要盯的文件记下看过的修改时间：读之前（按读之前的配置）取过的用那时的
/// `before`，读的时候正好有人写进来，下一轮照样看得出变了；新配置才盯上的文件（比如换了主题）只能
/// 现在取。
pub(crate) fn seen_after(before: &[(PathBuf, Option<SystemTime>)], config: &Config) -> Stamps {
    watch_paths(config)
        .into_iter()
        .map(|path| {
            let modified = match before.iter().find(|(seen, _)| *seen == path) {
                Some((_, modified)) => *modified,
                None => std::fs::metadata(&path).and_then(|m| m.modified()).ok(),
            };
            (path, modified)
        })
        .collect()
}

/// 用文本编辑器打开 runode 自己的配置文件；文件还不存在时先写一份模板。
pub fn open(cx: &App) {
    let Some(path) = runode_config::config_path() else {
        tracing::warn!("no home directory to keep the config file in");
        return;
    };
    if let Err(err) = runode_config::create_config_file(&path) {
        tracing::warn!("failed to create {}: {err}", path.display());
        return;
    }
    // `.conf` 常常没有默认程序，或者关联到别的应用：macOS 上用 `open -t` 指定文本编辑器，Windows 上
    // 用系统自带的记事本。等它退出要在别的线程里，免得卡住界面，也免得留下僵尸进程。
    let editor: Option<(&str, &[&str])> = if cfg!(target_os = "macos") {
        Some(("open", &["-t"]))
    } else if cfg!(windows) {
        Some(("notepad.exe", &[]))
    } else {
        None
    };
    match editor {
        Some((program, args)) => {
            std::thread::spawn(move || {
                if let Err(err) = std::process::Command::new(program).args(args).arg(&path).status() {
                    tracing::warn!("failed to open {}: {err}", path.display());
                }
            });
        }
        None => cx.open_with_system(&path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 列表键写回的各行：去掉生效的名字时先写一行空值，两层都写了同一个名字（读进来去了重，看不出来）也取消得
    /// 掉，去掉最后一个时只剩这一行空值；只加名字时不写；文件里已经写了清空的照样留着。
    #[test]
    fn list_keys_clear_lower_layers_when_dropping_names() {
        let lines = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
        // Ghostty 和 runode 的配置文件都写了 `ports`：取消它时要清掉 Ghostty 那一层的。
        assert_eq!(clear_first(&lines(&["ports"]), &lines(&["ports"]), Vec::new()), lines(&[""]));
        assert_eq!(
            clear_first(&lines(&["ports, branch"]), &lines(&["ports", "branch"]), lines(&["branch"])),
            lines(&["", "branch"])
        );
        assert_eq!(
            clear_first(&lines(&["codex"]), &lines(&["claude", "codex"]), lines(&["claude"])),
            lines(&["", "claude"])
        );
        // 只加名字：别处加的已经都在要写的里面，不用清。
        assert_eq!(clear_first(&[], &lines(&["ports"]), lines(&["ports, branch"])), lines(&["ports, branch"]));
        // 文件里写了清空的：加名字也留着那一行空值，不然下面一层的又露出来。
        assert_eq!(
            clear_first(&lines(&["", "ports"]), &lines(&["ports"]), lines(&["ports, branch"])),
            lines(&["", "ports, branch"])
        );
    }

    /// 读之前就盯着的文件记读之前取的时间，读的时候写进来的改动下一轮还看得出；没取过的现在取。
    #[test]
    fn files_stamped_before_reading_keep_that_stamp() {
        let config = Config::default();
        let paths = watch_paths(&config);
        assert!(!paths.is_empty());
        let before: Vec<_> = paths.iter().map(|path| (path.clone(), Some(SystemTime::UNIX_EPOCH))).collect();
        assert_eq!(seen_after(&before, &config), before);
        assert_eq!(seen_after(&before[1..], &config)[1..], before[1..]);
        assert_eq!(seen_after(&[], &config), watch_stamp(&config));
    }

    #[cfg(unix)]
    #[test]
    fn watch_dirs_cover_symlinks_and_missing_dirs() {
        let root = std::env::temp_dir().join(format!("rdr-watch-dirs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["data", "config", "dotfiles"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("data/config.conf"), "").unwrap();
        std::fs::write(root.join("dotfiles/ghostty"), "").unwrap();
        std::os::unix::fs::symlink(root.join("dotfiles/ghostty"), root.join("data/link")).unwrap();
        let roots = [root.join("config"), root.join("data")];
        let real = |dir: &str| std::fs::canonicalize(root.join(dir)).unwrap();
        let dirs = |paths: &[&str]| watch_dirs(&paths.iter().map(|path| root.join(path)).collect::<Vec<_>>(), &roots);
        // 已有的文件和还没建的文件都盯所在的目录，同一个目录只盯一次。
        assert_eq!(dirs(&["data/config.conf", "data/new.conf"]), [real("data")]);
        // 符号链接还要盯它指向的文件所在的目录。
        assert_eq!(dirs(&["data/link"]), [real("data"), real("dotfiles")]);
        // 目录还没建时在配置目录里往上找。
        assert_eq!(dirs(&["config/ghostty/config"]), [real("config")]);
        // 出了配置和数据目录不往上找。
        assert_eq!(dirs(&["elsewhere/app/config"]), Vec::<PathBuf>::new());
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// 编辑器写临时文件再改名换上、改符号链接指向的文件，都叫得醒。
    #[cfg(unix)]
    #[test]
    fn file_watch_sees_atomic_saves_and_symlink_targets() {
        let root = std::env::temp_dir().join(format!("rdr-file-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["data", "dotfiles"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("dotfiles/theme"), "a").unwrap();
        std::os::unix::fs::symlink(root.join("dotfiles/theme"), root.join("data/theme")).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut files = FileWatch::new(move || tx.send(()).unwrap()).unwrap();
        files.follow(&[root.join("data/config.conf"), root.join("data/theme")]);
        let settle = || {
            std::thread::sleep(Duration::from_millis(300));
            while rx.try_recv().is_ok() {}
        };
        let woken = || rx.recv_timeout(Duration::from_secs(10)).is_ok();
        settle();
        std::fs::write(root.join("data/config.conf.tmp"), "font-size = 14").unwrap();
        std::fs::rename(root.join("data/config.conf.tmp"), root.join("data/config.conf")).unwrap();
        assert!(woken());
        settle();
        std::fs::write(root.join("dotfiles/theme"), "b").unwrap();
        assert!(woken());
        drop(files);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
