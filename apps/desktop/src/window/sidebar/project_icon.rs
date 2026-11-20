//! 侧栏里项目名前的图标：用户换过的图片优先，其次是仓库里的图标文件（`repo_icon`），再次是 GitHub owner 的
//! 头像。换过的图片复制进 `runode_paths::Dirs::project_icons_dir`，文件名以项目的摘要开头，所有窗口共用一份。

use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
};

use gpui::{
    App, Context, Focusable as _, Global, Img, PathPromptOptions, PromptButton, PromptLevel, Window, img, prelude::*,
    px,
};
use sha2::{Digest, Sha256};

use crate::{
    WorkspaceId,
    host_client::hosts::HostId,
    project_host::ProjectHost,
    ui::menu::{MenuHost as _, MenuItem},
    window::{ChangeProjectIcon, ResetProjectIcon, WindowView},
};

/// 项目图标的边长。
pub(in crate::window) const PROJECT_ICON_SIZE: f32 = 16.;

/// 画出项目图标。
pub(in crate::window) fn project_icon_image(icon: PathBuf) -> Img {
    img(icon).flex_none().size(px(PROJECT_ICON_SIZE)).rounded(px(3.))
}

/// `gpui::img` 认得的图片后缀，用户换图标时只收这些。
pub(in crate::window) const IMAGE_EXTENSIONS: [&str; 7] = ["png", "jpg", "jpeg", "webp", "gif", "svg", "ico"];
/// 最多试仓库里排在前面的这么多个文件，都不是图片就算没有。
const ICON_CANDIDATES: usize = 5;
/// 认图片格式时读文件开头这么多字节。
const IMAGE_HEAD: u64 = 4096;
/// 远端仓库里的图标最多读这么多字节，再大的不要。
const REMOTE_ICON_MAX: u64 = 1 << 20;
/// 这些目录下的图片是别人的项目或测试数据，不当项目图标。
const SKIPPED_DIRS: [&str; 9] =
    ["node_modules", "vendor", "third_party", "test", "tests", "testdata", "fixtures", "example", "examples"];

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().take(8).map(|byte| format!("{byte:02x}")).collect()
}

/// 项目在 `project_icons_dir` 里的文件名前缀。
fn project_digest(host: &HostId, dir: &Path) -> String {
    hex(format!("{}\n{}", host.key().unwrap_or_default(), dir.display()).as_bytes())
}

/// 换过的图标，按 `project_digest` 找；第一次用时读一遍目录。
#[derive(Default)]
struct CustomIcons(Option<HashMap<String, PathBuf>>);

impl Global for CustomIcons {}

fn custom_icons(cx: &mut App) -> &mut HashMap<String, PathBuf> {
    cx.default_global::<CustomIcons>().0.get_or_insert_with(|| {
        let Some(dir) = runode_paths::Dirs::from_env().project_icons_dir() else { return HashMap::new() };
        let Ok(entries) = std::fs::read_dir(dir) else { return HashMap::new() };
        entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                Some((name.split_once('-')?.0.to_owned(), entry.path()))
            })
            .collect()
    })
}

/// 用户给项目换的图标。
pub(in crate::window) fn custom_icon(host: &HostId, dir: &Path, cx: &mut App) -> Option<PathBuf> {
    custom_icons(cx).get(&project_digest(host, dir)).cloned()
}

/// 把 `image` 复制成项目的图标，换掉原来的。文件名带上内容的摘要，换了图 `gpui::img` 不会用缓存里的旧图。
pub(in crate::window) fn set_custom_icon(host: &HostId, dir: &Path, image: &Path, cx: &mut App) -> io::Result<()> {
    let folder = runode_paths::Dirs::from_env().project_icons_dir().ok_or(io::ErrorKind::NotFound)?;
    let bytes = std::fs::read(image)?;
    let extension = image.extension().unwrap_or_default().to_string_lossy().to_lowercase();
    let digest = project_digest(host, dir);
    let file = folder.join(format!("{digest}-{}.{extension}", hex(&bytes)));
    std::fs::create_dir_all(&folder)?;
    std::fs::write(&file, bytes)?;
    if let Some(old) = custom_icons(cx).insert(digest, file.clone()).filter(|old| *old != file) {
        let _ = std::fs::remove_file(old);
    }
    Ok(())
}

/// 删掉换过的图标，回到默认的。
pub(in crate::window) fn reset_custom_icon(host: &HostId, dir: &Path, cx: &mut App) {
    if let Some(old) = custom_icons(cx).remove(&project_digest(host, dir)) {
        let _ = std::fs::remove_file(old);
    }
}

/// 仓库 `root` 里当项目图标的文件：按 `repo_icon_candidates` 的先后，挑前 `ICON_CANDIDATES` 个里第一个
/// 文件头确实是图片的（`is_image`）。远端的读回来存进 `remote_project_icons_dir`，交回本机上的那份。会阻塞。
pub(in crate::window) fn repo_icon(host: &ProjectHost, host_id: &HostId, root: &Path) -> Option<PathBuf> {
    let files = host.list_files(root);
    repo_icon_candidates(&files).into_iter().take(ICON_CANDIDATES).find_map(|candidate| {
        let found = root.join(candidate);
        if !host.is_remote() {
            let head = host.read_limited(&found, IMAGE_HEAD, false).ok()?.bytes;
            return is_image(&head).then_some(found);
        }
        let bytes = host.read_limited(&found, REMOTE_ICON_MAX, true).ok().filter(|read| !read.cut)?.bytes;
        if !is_image(&bytes) {
            return None;
        }
        let extension = found.extension()?.to_string_lossy().to_lowercase();
        let file = runode_paths::Dirs::from_env()
            .remote_project_icons_dir()?
            .join(format!("{}.{extension}", project_digest(host_id, root)));
        std::fs::create_dir_all(file.parent()?).ok()?;
        runode_paths::replace_file(&file, &bytes).ok()?;
        Some(file)
    })
}

/// 看文件开头的 `IMAGE_HEAD` 个字节像不像 `gpui::img` 画得出的图片：Git LFS 的指针文件、SVG 字体都不算。
fn is_image(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(IMAGE_HEAD as usize)];
    let svg = || {
        let text = String::from_utf8_lossy(head);
        text.contains("<svg") && !text.contains("<font")
    };
    head.starts_with(b"\x89PNG")
        || head.starts_with(&[0xFF, 0xD8])
        || head.starts_with(b"GIF8")
        || (head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP".as_slice()))
        || head.starts_with(&[0, 0, 1, 0])
        || svg()
}

/// 图标文件名去掉 icon、logo 前缀后剩下的是不是同一个图标的变体：尺寸、倍率（`32x32`、`@2x`）或配色
/// （`-dark`），不是 `close`、`search` 这类别的图标名。
fn icon_variant(rest: &str) -> bool {
    let rest = rest.trim_start_matches(['-', '_', '.']);
    rest.starts_with(|c: char| c.is_ascii_digit() || c == '@')
        || ["dark", "light", "white", "black", "color", "mono"].iter().any(|theme| rest.starts_with(theme))
}

/// 仓库的文件 `files`（相对仓库根）里可能是项目图标的，好的在前：文件名是 icon、logo、appicon 的最好，
/// `.appiconset`、`.iconset` 里的次之，favicon 再次，icon、logo 加上尺寸、配色的变体（`icon_variant`）
/// 最后，`logout`、`iconfont`、`icon-close` 这类别的名字不算；同一档里离仓库根近的在前，再是 SVG。
/// `SKIPPED_DIRS` 下的不算。
fn repo_icon_candidates(files: &[PathBuf]) -> Vec<&Path> {
    let mut candidates: Vec<_> = files
        .iter()
        .filter_map(|file| {
            let extension = file.extension()?.to_str()?.to_lowercase();
            if !IMAGE_EXTENSIONS.contains(&extension.as_str()) {
                return None;
            }
            let dirs = file.parent()?.components().map(|part| part.as_os_str().to_string_lossy().to_lowercase());
            let mut in_iconset = false;
            for dir in dirs {
                if SKIPPED_DIRS.contains(&dir.as_str()) {
                    return None;
                }
                in_iconset |= dir.ends_with(".appiconset") || dir.ends_with(".iconset");
            }
            let stem = file.file_stem()?.to_str()?.to_lowercase();
            let prefixed =
                ["icon", "logo", "appicon"].iter().any(|name| stem.strip_prefix(name).is_some_and(icon_variant));
            let rank = if ["icon", "logo", "appicon"].contains(&stem.replace(['-', '_'], "").as_str()) {
                0
            } else if in_iconset {
                1
            } else if stem.starts_with("favicon") {
                2
            } else if prefixed {
                3
            } else {
                return None;
            };
            Some(((rank, file.components().count(), extension != "svg"), file.as_path()))
        })
        .collect();
    candidates.sort();
    candidates.into_iter().map(|(_, file)| file).collect()
}

impl WindowView {
    /// 项目 `dir` 名前画的图标：换过的图片，其次是项目里各 workspace 找到的仓库图标、GitHub owner 头像；
    /// 都没有时为空。
    pub(in crate::window) fn project_icon(&self, host: &HostId, dir: &Path, cx: &mut App) -> Option<PathBuf> {
        let workspaces = || self.workspaces.iter().filter(|w| w.host == *host && w.project_dir() == dir);
        custom_icon(host, dir, cx)
            .or_else(|| {
                self.repo_icons.iter().find(|(h, d, _)| h == host && d == dir).and_then(|(_, _, icon)| icon.clone())
            })
            .or_else(|| workspaces().find_map(|w| w.repo.avatar.clone()))
    }

    /// 右键菜单里给 workspace `id` 所在项目换图标的几项；换过时另有恢复默认的一项。
    pub(in crate::window) fn project_icon_menu_items(&self, id: WorkspaceId, cx: &mut App) -> Vec<Option<MenuItem>> {
        let Some(workspace) = self.workspaces.iter().find(|w| w.id == id) else { return Vec::new() };
        let custom = custom_icon(&workspace.host, workspace.project_dir(), cx).is_some();
        let mut items = vec![Some(MenuItem::new(
            rust_i18n::t!("menu.change_project_icon"),
            Box::new(ChangeProjectIcon(id)),
            true,
            cx,
        ))];
        if custom {
            items.push(Some(MenuItem::new(
                rust_i18n::t!("menu.reset_project_icon"),
                Box::new(ResetProjectIcon(id)),
                true,
                cx,
            )));
        }
        items
    }

    /// 右键点了项目的组头：在 `position` 弹出给项目换图标的菜单，`id` 是项目里的一个 workspace。
    pub(in crate::window) fn open_project_menu(
        &mut self,
        id: WorkspaceId,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let items = self.project_icon_menu_items(id, cx);
        let focus = self.focus_handle(cx);
        self.open_context_menu(position, items, focus, cx);
    }

    fn icon_project(&self, id: WorkspaceId) -> Option<(HostId, PathBuf)> {
        let workspace = self.workspaces.iter().find(|w| w.id == id)?;
        Some((workspace.host.clone(), workspace.project_dir().to_path_buf()))
    }

    pub(in crate::window) fn change_project_icon(
        &mut self,
        action: &ChangeProjectIcon,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((host, dir)) = self.icon_project(action.0) else { return };
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(rust_i18n::t!("workspace.choose_icon").into_owned().into()),
        });
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(image) = paths.into_iter().next() else { return };
            let failed = cx.update(|window, cx| {
                let supported = image
                    .extension()
                    .is_some_and(|ext| IMAGE_EXTENSIONS.contains(&ext.to_string_lossy().to_lowercase().as_str()));
                let message = if supported {
                    let err = set_custom_icon(&host, &dir, &image, cx).err();
                    notify_windows(cx);
                    rust_i18n::t!("workspace.icon_failed", error = err?.to_string()).into_owned()
                } else {
                    rust_i18n::t!("workspace.icon_unsupported").into_owned()
                };
                let ok = [PromptButton::cancel(rust_i18n::t!("machine.ok"))];
                Some(crate::prompt::show(window, PromptLevel::Warning, &message, None, &ok, cx))
            });
            // 只是告诉一声，等用户点掉。
            if let Ok(Some(failed)) = failed {
                let _ = failed.await;
            }
        })
        .detach();
    }

    pub(in crate::window) fn reset_project_icon(
        &mut self,
        action: &ResetProjectIcon,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((host, dir)) = self.icon_project(action.0) else { return };
        reset_custom_icon(&host, &dir, cx);
        notify_windows(cx);
    }
}

/// 换了图标，各个窗口都重画侧栏，同一个项目可能开在几个窗口里。
fn notify_windows(cx: &mut App) {
    for window in crate::window::remote::windows(cx) {
        window.update(cx, |_, _, cx| cx.notify()).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick(files: &[&str]) -> Option<String> {
        let files: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
        repo_icon_candidates(&files).first().map(|file| file.display().to_string())
    }

    #[test]
    fn repo_icons_prefer_plain_names_near_the_root() {
        assert_eq!(
            pick(&["apps/admin/app/favicon.ico", "apps/mobile/assets/images/logo.png", "README.md"]),
            Some("apps/mobile/assets/images/logo.png".into())
        );
        assert_eq!(
            pick(&["apps/desktop/assets/icon.png", "apps/desktop/assets/icon.svg", "docs/deep/a/logo.png"]),
            Some("apps/desktop/assets/icon.svg".into())
        );
        assert_eq!(
            pick(&["App/Assets.xcassets/AppIcon.appiconset/icon_32x32@2x.png", "App/Assets.xcassets/Contents.json"]),
            Some("App/Assets.xcassets/AppIcon.appiconset/icon_32x32@2x.png".into())
        );
        assert_eq!(pick(&["public/favicon.svg", "src/main.rs"]), Some("public/favicon.svg".into()));
    }

    #[test]
    fn repo_icons_skip_other_peoples_images_and_unknown_formats() {
        assert_eq!(pick(&["node_modules/pkg/logo.png", "tests/fixtures/icon.png", "packaging/AppIcon.icns"]), None);
        assert_eq!(pick(&["src/background.png", "docs/screenshot.jpg", "src/iconfont/iconfont.svg"]), None);
        assert_eq!(pick(&["src/LogoutButton/logout.svg", "public/favicon.ico"]), Some("public/favicon.ico".into()));
        assert_eq!(pick(&["public/favicon.ico", "src/icon-close.svg"]), Some("public/favicon.ico".into()));
        assert_eq!(pick(&["assets/logo-dark.png", "src/main.rs"]), Some("assets/logo-dark.png".into()));
        assert_eq!(pick(&["assets/icon@2x.png"]), Some("assets/icon@2x.png".into()));
        assert_eq!(pick(&["images/icon-close.png", "assets/icons/icon_search.svg"]), None);
    }

    #[test]
    fn only_real_images_count_as_icons() {
        assert!(is_image(b"\x89PNG\r\n\x1a\n...."));
        assert!(is_image(br#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg"></svg>"#));
        assert!(!is_image(b"version https://git-lfs.github.com/spec/v1\noid sha256:abc\n"));
        assert!(!is_image(
            br#"<svg xmlns="http://www.w3.org/2000/svg"><defs><font id="iconfont"></font></defs></svg>"#
        ));
    }
}
