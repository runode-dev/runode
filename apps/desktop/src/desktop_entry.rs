//! 从 AppImage 跑时把 Runode 登记进桌面环境的应用菜单：本用户的 applications 目录里放一份 .desktop 文件，指向
//! 这个 AppImage，图标放进本用户的图标主题。每次启动对一遍，AppImage 挪了地方菜单项跟着改。deb、rpm 装的由包
//! 自己带着这两样，不经这里。窗口的 `app_id` 和菜单项的文件名都是 `runode_update::BUNDLE_ID`，任务栏靠它把窗口
//! 对上菜单项的图标。

use std::{
    io,
    path::{Path, PathBuf},
};

/// 菜单项的模板，deb、rpm 和 AppImage 里带的也是它。
const TEMPLATE: &str = include_str!("../assets/linux/dev.runode.app.desktop");
const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");
/// 标明菜单项是 runode 写的：同名文件里没有这一行时是用户自己的，不覆盖。
const MANAGED: &str = "X-Runode-AppImage=true";

/// 启动时在后台登记一遍；不是从 AppImage 跑的不登记。
pub fn refresh() {
    let Some(image) = runode_paths::appimage() else { return };
    let spawned = std::thread::Builder::new().name("desktop-entry".into()).spawn(move || {
        if let Err(err) = install(&image) {
            tracing::warn!("cannot add Runode to the application menu: {err}");
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("cannot add Runode to the application menu: {err}");
    }
}

/// `runode --desktop-entry`：安装脚本装好 AppImage 后登记菜单项，返回进程的退出码。
pub fn run() -> i32 {
    let installed = runode_paths::appimage()
        .ok_or_else(|| io::Error::other("runode is not running from an AppImage"))
        .and_then(|image| install(&image));
    match installed {
        Ok(()) => 0,
        Err(err) => {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr(), "cannot add Runode to the application menu: {err}");
            1
        }
    }
}

fn install(image: &Path) -> io::Result<()> {
    let dirs = runode_paths::Dirs::from_env();
    let no_home = || io::Error::new(io::ErrorKind::NotFound, "no home directory");
    let entry = dirs.applications_dir().ok_or_else(no_home)?.join(format!("{}.desktop", runode_update::BUNDLE_ID));
    let contents = entry_for(image).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, format!("{} cannot go in a desktop entry", image.display()))
    })?;
    match std::fs::read_to_string(&entry) {
        Ok(old) if old == contents => {}
        Ok(old) if !old.lines().any(|line| line == MANAGED) => {
            tracing::info!("{} is not ours, leaving it alone", entry.display());
            return Ok(());
        }
        Ok(_) => write(&entry, contents.as_bytes())?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => write(&entry, contents.as_bytes())?,
        // 读不出来（不是 UTF-8、没有权限）的不是 runode 写的，不覆盖。
        Err(err) => return Err(io::Error::new(err.kind(), format!("{}: {err}", entry.display()))),
    }
    let icon = icon_file(&dirs).ok_or_else(no_home)?;
    if std::fs::read(&icon).ok().as_deref() != Some(ICON_PNG) {
        write(&icon, ICON_PNG)?;
    }
    Ok(())
}

fn icon_file(dirs: &runode_paths::Dirs) -> Option<PathBuf> {
    Some(dirs.icons_dir()?.join("hicolor/256x256/apps").join(format!("{}.png", runode_update::BUNDLE_ID)))
}

fn write(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    runode_paths::replace_file(path, contents)
}

/// 指向 AppImage `image` 的菜单项：模板的 `Exec` 换成它，加上 `TryExec`（文件没了时菜单不显示这一项）和
/// `MANAGED`。路径里有控制字符、不是 UTF-8 时写不出来。
fn entry_for(image: &Path) -> Option<String> {
    let path = image.to_str()?;
    let exec = exec_arg(path)?;
    let mut out = String::with_capacity(TEMPLATE.len() + 2 * path.len() + 64);
    let mut replaced = false;
    for line in TEMPLATE.lines() {
        if line.starts_with("Exec=") {
            out.push_str(&format!("TryExec={}\nExec={exec}\n", path.replace('\\', "\\\\")));
            replaced = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str(MANAGED);
    out.push('\n');
    replaced.then_some(out)
}

/// 按 Desktop Entry 规范把 `arg` 写成 `Exec` 里的一个参数：用双引号括起，引号里的 `"`、`` ` ``、`$`、`\` 前加
/// 反斜杠，字符串值自己的转义再把每个反斜杠写成两个；`%` 写成 `%%`。有控制字符时写不出来。
fn exec_arg(arg: &str) -> Option<String> {
    if arg.chars().any(char::is_control) {
        return None;
    }
    let mut out = String::from("\"");
    for c in arg.chars() {
        match c {
            '"' | '`' | '$' => {
                out.push_str("\\\\");
                out.push(c);
            }
            '\\' => out.push_str("\\\\\\\\"),
            '%' => out.push_str("%%"),
            c => out.push(c),
        }
    }
    out.push('"');
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_runs_the_appimage() {
        let entry = entry_for(Path::new("/home/me/Apps/Runode 1.AppImage")).unwrap();
        assert!(entry.contains("\nTryExec=/home/me/Apps/Runode 1.AppImage\n"));
        assert!(entry.contains("\nExec=\"/home/me/Apps/Runode 1.AppImage\"\n"));
        assert!(!entry.contains("Exec=runode"));
        assert!(entry.contains(&format!("\nStartupWMClass={}\n", runode_update::BUNDLE_ID)));
        assert!(entry.ends_with(&format!("{MANAGED}\n")));
    }

    /// 引号、反引号、`$`、反斜杠和 `%` 按规范转义；换行写不进一行。
    #[test]
    fn exec_quotes_reserved_characters() {
        assert_eq!(exec_arg("/a/b").as_deref(), Some("\"/a/b\""));
        assert_eq!(exec_arg("/a\"$`b").as_deref(), Some(r#""/a\\"\\$\\`b""#));
        assert_eq!(exec_arg(r"/a\b").as_deref(), Some(r#""/a\\\\b""#));
        assert_eq!(exec_arg("/100%").as_deref(), Some("\"/100%%\""));
        assert_eq!(exec_arg("/a\nb"), None);
    }
}
