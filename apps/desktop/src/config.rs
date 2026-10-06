//! 界面这边的配置：把加载好的配置放进全局，配置文件保存后自动重载，系统深浅色变了时
//! 换到对应的主题，以及用文本编辑器打开配置文件。在界面里改设置见 `settings`。配置的读取和解析见 `runode_config`。

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};

use gpui::{App, Global, WindowAppearance};
use runode_config::Config;

/// 当前生效的配置。视图通过 `observe_global` 在重载后重新应用。
pub struct AppConfig(pub Arc<Config>);

impl Global for AppConfig {}

/// 两次检查配置文件是否变化的间隔。
const WATCH_INTERVAL: Duration = Duration::from_secs(1);

/// 加载配置并开始监视配置文件，保存后自动重载。启动时后台线程已经读过一遍（见
/// `host_client::take_config`），拿它对得上系统外观时直接用，不再读。
pub fn install(cx: &mut App) {
    let dark = system_is_dark(cx);
    let loaded = crate::host_client::take_config().filter(|config| config.fits_appearance(dark));
    match loaded {
        Some(mut config) => {
            config.dark = dark;
            apply(cx, config);
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

/// 上次加载配置时各配置文件的修改时间。每次加载后按新配置重新记录：重载可能引入新的文件（比如
/// 换了主题），设置窗口写回后也会自己重载，这样监视的那一轮不会因为这些再重载一次。
struct Seen(Vec<(PathBuf, Option<SystemTime>)>);

impl Global for Seen {}

/// 重新读取全部配置文件并广播给各视图。
pub fn reload(cx: &mut App) {
    let dark = system_is_dark(cx);
    apply(cx, Config::load(dark));
}

/// 让 `config` 生效并广播给各视图。
fn apply(cx: &mut App, config: Config) {
    // 先换语言再广播，观察配置的菜单和视图重画时就是新语言。
    crate::i18n::set(&config.language.clone().unwrap_or_else(crate::i18n::system));
    // 宿主先换主题，视图等它在各个会话的输出流里标出位置后再跟着换。
    crate::host_client::configure(&config);
    cx.set_global(Seen(watch_stamp(&config)));
    cx.set_global(AppConfig(Arc::new(config)));
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
pub(crate) fn watch_stamp(config: &Config) -> Vec<(PathBuf, Option<SystemTime>)> {
    config
        .watch_paths()
        .into_iter()
        .map(|path| {
            let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
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
