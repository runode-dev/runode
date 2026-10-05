//! 界面这边的配置：把加载好的配置放进全局，配置文件保存后自动重载，系统深浅色变了时
//! 换到对应的主题，以及用文本编辑器打开配置文件。配置的读取和解析见 `runode_config`。

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

/// 加载配置并开始监视配置文件，保存后自动重载。
pub fn install(cx: &mut App) {
    reload(cx);
    // 模板按界面语言写，所以先加载配置定下语言。
    if let Some(path) = runode_config::config_path()
        && let Err(err) = runode_config::create_config_file(&path)
    {
        tracing::warn!("failed to create {}: {err}", path.display());
    }
    cx.spawn(async move |cx| {
        let mut seen = None;
        loop {
            cx.background_executor().timer(WATCH_INTERVAL).await;
            let mut stamp = cx.update(|cx| watch_stamp(&cx.global::<AppConfig>().0));
            // 第一次只记录，之后有变化才重载。重载可能引入新的文件（比如换了主题），
            // 所以重载后按新配置重新记录，免得下一轮又因文件列表变化再重载一次。
            if seen.as_ref().is_some_and(|seen| *seen != stamp) {
                stamp = cx.update(|cx| {
                    reload(cx);
                    watch_stamp(&cx.global::<AppConfig>().0)
                });
            }
            seen = Some(stamp);
        }
    })
    .detach();
}

/// 重新读取全部配置文件并广播给各视图。
pub fn reload(cx: &mut App) {
    let dark = system_is_dark(cx);
    let config = Config::load(dark);
    // 先换语言再广播，观察配置的菜单和视图重画时就是新语言。
    crate::i18n::set(&config.language.clone().unwrap_or_else(crate::i18n::system));
    // 宿主先换主题，视图等它在各个会话的输出流里标出位置后再跟着换。
    crate::session_host::configure(&config);
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
fn watch_stamp(config: &Config) -> Vec<(PathBuf, Option<SystemTime>)> {
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
