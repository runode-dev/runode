//! 系统声音：agent 提醒的提示音和设置窗口里试听都按名字播放系统自带的声音，不打包音频文件。

/// 按名字播放系统声音；正在放着的先停下，从头放。
#[cfg(target_os = "macos")]
pub fn play(name: &str) {
    use objc2_app_kit::NSSound;
    use objc2_foundation::NSString;

    match NSSound::soundNamed(&NSString::from_str(name)) {
        Some(sound) => {
            sound.stop();
            sound.play();
        }
        None => tracing::warn!("no system sound named {name}"),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn play(_: &str) {}

/// 能按名字播放的声音：系统自带的和用户装在 ~/Library/Sounds 里的，按名字排序。
pub fn names() -> Vec<String> {
    let home = runode_paths::Dirs::from_env().home.map(|home| home.join("Library/Sounds"));
    let dirs = [Some(std::path::PathBuf::from("/System/Library/Sounds")), home];
    let mut names: Vec<String> = dirs
        .into_iter()
        .flatten()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .flatten()
        .filter_map(|entry| Some(entry.path().file_stem()?.to_str()?.to_owned()))
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    names.dedup();
    names
}
