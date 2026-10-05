//! 默认配置换成的终端设置和 `TermSettings` 的默认值一致。

use runode_config::Config;
use runode_shared_types::settings::TermSettings;

#[test]
fn default_config_gives_the_default_term_settings() {
    assert_eq!(Config::default().term_settings(), TermSettings::default());
}
