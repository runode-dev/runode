//! 手机端测试读的新开 agent 菜单样例 `tests/fixtures/launchers.json`：菜单里列的各家 agent（`listed`），每家
//! 按先后试的程序名（`same_kind`，不带子命令）和认出来的种类（`agent_from_name`，手机据此挑 logo）。手机端
//! 照它核对自己那份菜单，改了 `LAUNCHERS` 后设 `RUNODE_BLESS=1` 跑这个测试重新写出样例，再跑手机端的测试；
//! 不设时样例和生成的结果对不上就失败。

use std::path::PathBuf;

use runode_agent::{
    agent_from_name,
    models::{LAUNCHERS, listed, same_kind},
};
use serde_json::{Value, json};

const NOTE: &str = "由 runode_agent 的测试 launcher_fixture 按 LAUNCHERS 生成，手机端的测试据此核对新开 agent 的菜单。\
                    不要手改：改了 LAUNCHERS 后在仓库根跑 \
                    `RUNODE_BLESS=1 cargo test -p runode-agent --test launcher_fixture` 重新生成。";

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/launchers.json")
}

/// 命令开头的程序名，不带子命令。
fn program(command: &str) -> &str {
    command.split_whitespace().next().unwrap_or(command)
}

fn launchers() -> Value {
    let entries = (0..LAUNCHERS.len()).filter(|agent| listed(*agent)).map(|first| {
        let programs: Vec<&str> = std::iter::successors(Some(first), |agent| same_kind(*agent))
            .map(|agent| program(LAUNCHERS[agent].command))
            .collect();
        json!({
            "label": LAUNCHERS[first].label,
            "programs": programs,
            "kind": agent_from_name(programs[0]),
        })
    });
    json!({ "_说明": NOTE, "launchers": entries.collect::<Vec<_>>() })
}

#[test]
fn fixture_matches_the_launchers() {
    let path = fixture_path();
    if std::env::var_os("RUNODE_BLESS").is_some_and(|value| value == "1") {
        let text = serde_json::to_string_pretty(&launchers()).unwrap() + "\n";
        std::fs::write(&path, text).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        return;
    }
    let text = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    let fixture: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(fixture, launchers(), "样例和 LAUNCHERS 对不上，设 RUNODE_BLESS=1 重新生成");
}
