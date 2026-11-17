//! 规则文件的校验和求值：写错的条件不编译，读不到屏幕时只比标题和进度报告。

use std::cell::Cell;

use runode_agent::{BackgroundShells, RuleSet, Signals, Verdict};
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
fn only_working_rules_on_a_new_engine_override_a_reported_idle() {
    let file = |state: &str, engine: &str| {
        format!(
            "id = \"claude\"\n{engine}\n[[rules]]\nid = \"r\"\nstate = \"{state}\"\noverrides_reported_idle = true\ncontains = [\"ok\"]\n"
        )
    };
    assert!(RuleSet::parse(&file("working", "min_engine_version = 4")).is_ok());
    assert!(RuleSet::parse(&file("working", "")).is_ok());
    assert!(RuleSet::parse(&file("idle", "min_engine_version = 4")).is_err());
    assert!(RuleSet::parse(&file("working", "min_engine_version = 3")).is_err());
}

#[test]
fn only_working_rules_on_a_new_engine_look_at_background_shells() {
    let file = |state: &str, engine: &str| {
        format!(
            "id = \"claude\"\n{engine}\n[[rules]]\nid = \"r\"\nstate = \"{state}\"\nbackground_shell_busy = true\ncontains = [\"ok\"]\n"
        )
    };
    assert!(RuleSet::parse(&file("working", "min_engine_version = 5")).is_ok());
    assert!(RuleSet::parse(&file("working", "")).is_ok());
    assert!(RuleSet::parse(&file("idle", "min_engine_version = 5")).is_err());
    assert!(RuleSet::parse(&file("working", "min_engine_version = 4")).is_err());
}

fn background_rules() -> RuleSet {
    RuleSet::parse(
        r#"
id = "claude"
min_engine_version = 5
[[rules]]
id = "shell"
state = "working"
priority = 10
background_shell_busy = true
contains = ["1 shell"]
[[rules]]
id = "prompt"
state = "idle"
priority = 5
contains = ["❯"]
"#,
    )
    .unwrap()
}

#[test]
fn background_shell_rules_match_unless_every_shell_is_serving() {
    let set = background_rules();
    let screen = Some("❯\n1 shell");
    let rule = |probe: &dyn Fn() -> Option<BackgroundShells>| match set.evaluate(Signals {
        screen,
        background: Some(probe),
        ..Default::default()
    }) {
        Verdict::Matched { rule, .. } => rule,
        verdict => panic!("{verdict:?}"),
    };
    // 说不清时照常命中。
    assert_eq!(rule(&|| None), "shell");
    assert!(matches!(set.evaluate(Signals { screen, ..Default::default() }), Verdict::Matched { rule: "shell", .. }));
    assert_eq!(rule(&|| Some(BackgroundShells { shells: 2, serving: 1 })), "shell");
    assert_eq!(rule(&|| Some(BackgroundShells { shells: 0, serving: 0 })), "shell");
    // 全是服务：落到下一条。
    assert_eq!(rule(&|| Some(BackgroundShells { shells: 1, serving: 1 })), "prompt");
}

#[test]
fn background_shells_are_probed_only_when_the_rest_of_the_rule_matches() {
    let set = background_rules();
    let probes = Cell::new(0);
    let probe = || {
        probes.set(probes.get() + 1);
        None
    };
    let verdict = set.evaluate(Signals { screen: Some("❯"), background: Some(&probe), ..Default::default() });
    assert!(matches!(verdict, Verdict::Matched { rule: "prompt", .. }), "{verdict:?}");
    assert_eq!(probes.get(), 0);
    set.evaluate(Signals { screen: Some("❯\n1 shell"), background: Some(&probe), ..Default::default() });
    assert_eq!(probes.get(), 1);
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
