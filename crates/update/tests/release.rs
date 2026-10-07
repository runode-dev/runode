//! 版本清单的读法和版本号的比较。

use runode_update::{Error, Release, arch, is_newer};

const MANIFEST: &str = r#"{
    "version": "0.2.0",
    "page": "https://github.com/runode-dev/runode/releases/tag/v0.2.0",
    "archives": {
        "arm64": "https://github.com/runode-dev/runode/releases/download/v0.2.0/Runode-0.2.0-arm64.zip"
    }
}"#;

#[test]
fn the_manifest_gives_the_archive_for_this_arch() {
    let release = Release::parse(MANIFEST.as_bytes(), "arm64").unwrap();
    assert_eq!(release.version, "0.2.0");
    assert_eq!(release.page, "https://github.com/runode-dev/runode/releases/tag/v0.2.0");
    assert_eq!(
        release.archive.as_deref(),
        Some("https://github.com/runode-dev/runode/releases/download/v0.2.0/Runode-0.2.0-arm64.zip")
    );
}

/// 清单里没有这个架构时只能去网页下载。
#[test]
fn an_arch_missing_from_the_manifest_has_no_archive() {
    let release = Release::parse(MANIFEST.as_bytes(), "x86_64").unwrap();
    assert_eq!(release.archive, None);
    let bare = Release::parse(br#"{"version": "0.2.0", "page": "https://example.com"}"#, "arm64").unwrap();
    assert_eq!(bare.archive, None);
}

#[test]
fn a_broken_manifest_is_an_error() {
    assert!(matches!(Release::parse(b"<html>", "arm64"), Err(Error::Manifest(_))));
    assert!(matches!(Release::parse(br#"{"version": "v0.2", "page": ""}"#, "arm64"), Err(Error::Manifest(_))));
}

#[test]
fn versions_compare_part_by_part() {
    assert!(is_newer("0.2.0", "0.1.0"));
    assert!(is_newer("0.10.0", "0.9.3"));
    assert!(is_newer("1.0", "0.99.99"));
    assert!(is_newer("0.1.1", "0.1"));
    assert!(!is_newer("0.1.0", "0.1.0"));
    assert!(!is_newer("0.2", "0.2.0"));
    assert!(!is_newer("0.1.0", "0.2.0"));
}

/// 读不懂的版本号一律不算新，免得把装着的版本换成来路不明的。
#[test]
fn unreadable_versions_are_never_newer() {
    assert!(!is_newer("0.2.0-beta", "0.1.0"));
    assert!(!is_newer("", "0.1.0"));
    assert!(!is_newer("0.2.0", "dev"));
}

#[test]
fn the_arch_uses_the_lipo_names() {
    let expected = if cfg!(target_arch = "aarch64") { "arm64" } else { std::env::consts::ARCH };
    assert_eq!(arch(), expected);
}
