//! 界面这边的配置：把加载好的配置放进全局，配置文件保存后自动重载，系统深浅色变了时
//! 换到对应的主题，以及用文本编辑器打开配置文件。在界面里改设置见 `settings`。配置的读取和解析见 `runode_config`。

use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use gpui::{App, Global, WindowAppearance};
use runode_config::{Config, ConfigFile};

/// 当前生效的配置。视图通过 `observe_global` 在重载后重新应用。
pub struct AppConfig(pub Arc<Config>);

impl Global for AppConfig {}

/// 两次检查配置文件是否变化的间隔。
const WATCH_INTERVAL: Duration = Duration::from_secs(1);

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
    // 模板按界面语言写，所以先加载配置定下语言。
    if let Some(path) = runode_config::config_path()
        && let Err(err) = runode_config::create_config_file(&path)
    {
        tracing::warn!("failed to create {}: {err}", path.display());
    }
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(WATCH_INTERVAL).await;
            cx.update(|cx| {
                let stamp = watch_stamp(&cx.global::<AppConfig>().0);
                if cx.try_global::<Seen>().is_some_and(|seen| seen.0 != stamp) {
                    reload(cx);
                }
            });
        }
    })
    .detach();
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
    crate::i18n::set(&config.language.clone().unwrap_or_else(crate::i18n::system));
    // 宿主先换主题，视图等它在各个会话的输出流里标出位置后再跟着换。
    crate::host_client::configure(&config);
    cx.set_global(Seen(seen));
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

/// 所有可能的配置文件的修改时间。还不存在的文件也算在内，新建配置文件同样会触发重载。
pub(crate) fn watch_stamp(config: &Config) -> Stamps {
    seen_after(&[], config)
}

/// 读出 `config` 之后，按它要盯的文件记下看过的修改时间：读之前（按读之前的配置）取过的用那时的
/// `before`，读的时候正好有人写进来，下一轮照样看得出变了；新配置才盯上的文件（比如换了主题）只能
/// 现在取。
pub(crate) fn seen_after(before: &[(PathBuf, Option<SystemTime>)], config: &Config) -> Stamps {
    config
        .watch_paths()
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
    // `.conf` 常常没有默认程序，或者关联到别的应用；macOS 上用 `open -t` 指定文本编辑器。
    // 等它退出要在别的线程里，免得卡住界面，也免得留下僵尸进程。
    if cfg!(target_os = "macos") {
        std::thread::spawn(move || {
            if let Err(err) = std::process::Command::new("open").arg("-t").arg(&path).status() {
                tracing::warn!("failed to open {}: {err}", path.display());
            }
        });
    } else {
        cx.open_with_system(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 读之前就盯着的文件记读之前取的时间，读的时候写进来的改动下一轮还看得出；没取过的现在取。
    #[test]
    fn files_stamped_before_reading_keep_that_stamp() {
        let config = Config::default();
        let paths = config.watch_paths();
        assert!(!paths.is_empty());
        let before: Vec<_> = paths.iter().map(|path| (path.clone(), Some(SystemTime::UNIX_EPOCH))).collect();
        assert_eq!(seen_after(&before, &config), before);
        assert_eq!(seen_after(&before[1..], &config)[1..], before[1..]);
        assert_eq!(seen_after(&[], &config), watch_stamp(&config));
    }
}
