//! 按名字读出一个主题的配色。

use runode_config::{Config, theme_config};

#[test]
fn theme_config_reads_a_bundled_theme() {
    let theme = theme_config("Adwaita Dark").unwrap();
    assert_ne!(theme.background, Config::default().background);
    assert!(theme_config("no such theme").is_none());
}
