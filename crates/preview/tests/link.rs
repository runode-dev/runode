//! Markdown 里的链接和图片地址解析成网页、本地文件和锚点。

use std::path::Path;

use runode_preview::{LinkTarget, link_target};

#[test]
fn links_resolve_against_the_file() {
    let dir = Path::new("/repo/docs");
    assert_eq!(link_target(dir, "https://x.dev/a"), Some(LinkTarget::Web("https://x.dev/a".into())));
    assert_eq!(link_target(dir, "HTTP://x.dev"), Some(LinkTarget::Web("HTTP://x.dev".into())));
    assert_eq!(link_target(dir, "mailto:a@b.c"), Some(LinkTarget::Web("mailto:a@b.c".into())));
    // `..` 和 `.` 按字面消掉，和文件树打开的路径一样。
    assert_eq!(link_target(dir, "../README.md#usage"), Some(LinkTarget::File("/repo/README.md".into())));
    assert_eq!(link_target(dir, "./a/../b.md"), Some(LinkTarget::File("/repo/docs/b.md".into())));
    assert_eq!(link_target(dir, "../../../../x.md"), Some(LinkTarget::File("/x.md".into())));
    assert_eq!(link_target(dir, "img/a.png?raw=1"), Some(LinkTarget::File("/repo/docs/img/a.png".into())));
    assert_eq!(link_target(dir, "/etc/hosts"), Some(LinkTarget::File("/etc/hosts".into())));
    assert_eq!(link_target(dir, "#usage"), Some(LinkTarget::Anchor("usage".into())));
    assert_eq!(link_target(dir, "#"), None);
    assert_eq!(link_target(dir, "javascript:alert(1)"), None);
    assert_eq!(link_target(dir, "vscode://file/a"), None);
}

#[test]
fn local_links_and_anchors_are_percent_decoded() {
    let dir = Path::new("/repo");
    assert_eq!(link_target(dir, "my%20file.md"), Some(LinkTarget::File("/repo/my file.md".into())));
    assert_eq!(link_target(dir, "#%E4%B8%AD%E6%96%87"), Some(LinkTarget::Anchor("中文".into())));
    // 不成对的 `%`、解出来不是 UTF-8 的原样留着。
    assert_eq!(link_target(dir, "100%"), Some(LinkTarget::File("/repo/100%".into())));
    assert_eq!(link_target(dir, "a%zzb"), Some(LinkTarget::File("/repo/a%zzb".into())));
    assert_eq!(link_target(dir, "%FF"), Some(LinkTarget::File("/repo/%FF".into())));
}
