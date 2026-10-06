//! runode 自己的命令行的动态补全：会话的写法和配对过的设备，都由 runode 命令行自己列出来。
//! 命令规格和别的命令的一样是一份 JSON，和 crate 放在一起，构建脚本把它和子模块里的规格
//! 一起编进二进制；它要跟着命令行的参数解析一起改。

use warp_command_signatures::{
    CommandBuilder, CommandSignatureGenerators, Generator, GeneratorResults, GeneratorResultsCollector, Suggestion,
};

/// `runode list` 显示的会话标识有几位，补全给的也是这么长的前缀。
const SHORT_ID: usize = 8;

/// 不用列会话也能写的会话写法，排在会话标识后面。
const SELECTORS: &[(&str, &str)] = &[
    ("self", "Your own terminal"),
    ("left", "The pane to the left of yours"),
    ("right", "The pane to the right of yours"),
    ("up", "The pane above yours"),
    ("down", "The pane below yours"),
    ("next", "The next pane in your tab"),
    ("prev", "The previous pane in your tab"),
];

pub fn generators() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("runode")
        .add_generator("sessions", Generator::script(list("runode list --json"), sessions))
        .add_generator("devices", Generator::script(list("runode remote devices --json"), devices))
}

fn list(command: &str) -> CommandBuilder {
    CommandBuilder::single_command_and_ignore_stderr(command)
}

/// `runode list --json` 的输出换成会话候选：短标识，说明是 agent 和它的状态、前台程序、
/// 标题；后面跟着固定的几种写法。连不上 app 时只有固定的写法。
fn sessions(output: &str) -> GeneratorResults {
    let listing: serde_json::Value = serde_json::from_str(output).unwrap_or_default();
    let sessions = listing["sessions"].as_array().map(Vec::as_slice).unwrap_or_default();
    let ids = sessions.iter().filter(|session| session["exited"] != true).filter_map(|session| {
        let id = session["id"].as_str()?;
        let text = |key: &str| session[key].as_str().filter(|text| !text.is_empty());
        let agent = text("agent").map(|agent| match text("state") {
            Some(state) => format!("{agent} ({state})"),
            None => agent.to_owned(),
        });
        let description: Vec<&str> =
            [agent.as_deref(), text("foreground"), text("title")].into_iter().flatten().collect();
        let id = id.get(..SHORT_ID).unwrap_or(id);
        Some(match description.as_slice() {
            [] => Suggestion::new(id),
            parts => Suggestion::with_description(id, parts.join(" · ")),
        })
    });
    let selectors = SELECTORS.iter().map(|(name, description)| Suggestion::with_description(*name, *description));
    ids.chain(selectors).collect_ordered_results()
}

/// `runode remote devices --json` 的输出换成设备候选：完整标识，说明是设备的名字。
fn devices(output: &str) -> GeneratorResults {
    let devices: serde_json::Value = serde_json::from_str(output).unwrap_or_default();
    devices
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|device| {
            let id = device["device_id"].as_str()?;
            Some(match device["name"].as_str() {
                Some(name) => Suggestion::with_description(id, name),
                None => Suggestion::new(id),
            })
        })
        .collect_ordered_results()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(results: GeneratorResults) -> Vec<(String, Option<String>)> {
        results.suggestions.into_iter().map(|s| (s.exact_string, s.description)).collect()
    }

    #[test]
    fn sessions_come_before_the_fixed_selectors() {
        let output = r#"{"self":null,"layout":null,"sessions":[
            {"id":"0123456789abcdef0123456789abcdef","agent":"claude","state":"idle","foreground":"claude","title":"fix bug","exited":false},
            {"id":"fedcba9876543210fedcba9876543210","agent":null,"state":null,"foreground":null,"title":null,"exited":false},
            {"id":"aaaaaaaa76543210fedcba9876543210","exited":true}
        ]}"#;
        let listed = values(sessions(output));
        assert_eq!(listed[0], ("01234567".into(), Some("claude (idle) · claude · fix bug".into())));
        assert_eq!(listed[1], ("fedcba98".into(), None));
        // 已经结束的会话不列。
        assert_eq!(listed[2].0, "self");
        assert_eq!(listed.len(), 2 + SELECTORS.len());
        // 连不上 app 时输出为空，仍然给出固定的写法。
        assert_eq!(values(sessions("")).len(), SELECTORS.len());
    }

    #[test]
    fn devices_are_listed_by_full_id() {
        let output =
            r#"[{"device_id":"00112233445566778899aabbccddeeff","name":"iPhone","paired_at":1,"last_seen":2}]"#;
        assert_eq!(values(devices(output)), [("00112233445566778899aabbccddeeff".into(), Some("iPhone".into()))]);
        assert!(values(devices("")).is_empty());
    }
}
