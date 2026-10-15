//! 规则文件的校验和求值：写错的条件不编译，读不到屏幕时只比标题和进度报告。

use runode_agent::{RuleSet, Signals, Verdict};
use runode_types::agent::AgentState;

#[test]
fn empty_contains_entries_are_rejected() {
    // 空的或者只有空白的词处处都出现，等于没写条件，嵌套在里面的也一样。
    for gate in [
        r#"contains = [""]"#,
        r#"contains = ["   "]"#,
        r#"contains = ["ok", " \n "]"#,
        r#"any = [{ contains = ["ok"] }, { contains = ["  "] }]"#,
        r#"contains = ["ok"]
not = [{ contains = [" "] }]"#,
    ] {
        let text = format!("id = \"claude\"\n[[rules]]\nid = \"r\"\nstate = \"idle\"\n{gate}\n");
        assert!(RuleSet::parse(&text).is_err(), "{gate}");
    }
}

#[test]
fn rules_without_a_screen_still_compare_the_title_and_progress() {
    let set = RuleSet::parse(
        r#"
id = "claude"
[[rules]]
id = "screen"
state = "idle"
priority = 10
regex = ['^']
[[rules]]
id = "progress"
state = "blocked"
priority = 5
region = "osc_progress"
regex = ['^4;3']
"#,
    )
    .unwrap();
    let both = set.evaluate(Signals { screen: Some(""), progress: "4;3", ..Default::default() });
    assert!(matches!(both, Verdict::Matched { rule: "screen", .. }), "{both:?}");
    // 读不到屏幕时比屏幕的规则不算命中，比进度报告的照样比。
    let blind = set.evaluate(Signals { screen: None, progress: "4;3", ..Default::default() });
    assert!(matches!(blind, Verdict::Matched { state: AgentState::Blocked, rule: "progress", .. }), "{blind:?}");
    assert_eq!(set.evaluate(Signals::default()), Verdict::Unknown);
}
