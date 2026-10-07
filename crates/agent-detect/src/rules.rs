//! 识别规则：规则文件的格式、校验、编译和求值。
//!
//! 一份规则文件管一种 agent：
//!
//! ```toml
//! id = "claude"              # agent 的短名，见 `AgentKind::label`
//! aliases = ["claude-code"]  # 也认这些名字
//! version = "2026.09.11.1"   # 以下三项只作记录
//! min_engine_version = 2
//! updated_at = "2026-09-11T00:00:00Z"
//!
//! [[rules]]
//! id = "live_prompt_box"
//! state = "idle"             # idle、working、blocked 或 unknown
//! priority = 950             # 几条都命中时取最大的，一样大取靠前的
//! region = "prompt_box_body" # 在哪段文字里找，见 `Region`
//! visible_idle = true        # 命中就说明界面上确实摆着这个状态，不必再等确认
//! line_regex = ['^\s*❯']
//! not = [{ contains = ["esc to cancel"] }]
//! ```
//!
//! 一条规则的条件全部满足才算命中：`contains` 里每个词都出现（不分大小写），`regex` 每个都
//! 匹配整段，`line_regex` 每个都至少匹配其中一行，`all` 里的子条件全满足，`any` 里的至少一个
//! 满足，`not` 里的一个都不满足。子条件的写法和规则本身一样，可以再嵌套。
//!
//! `skip_state_update = true` 的规则（状态必须是 unknown）命中时，这次不改状态：屏幕上是
//! agent 自己的历史记录、菜单这类看不出当前状态的界面。不跳过的 unknown 规则命中时当作
//! 没有规则命中。

use regex::Regex;
use runode_shared_types::agent::{AgentKind, AgentState};
use serde::Deserialize;

use crate::{identify, region::Region};

/// 支持到第几版规则格式；规则文件的 `min_engine_version` 比它大时不用。
pub const ENGINE_VERSION: u32 = 3;
/// `top_non_empty_lines` 从第几版格式开始有。
const TOP_LINES_ENGINE_VERSION: u32 = 3;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileSpec {
    id: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    version: Option<String>,
    min_engine_version: Option<u32>,
    #[serde(default)]
    #[allow(dead_code)]
    updated_at: Option<String>,
    #[serde(default)]
    rules: Vec<RuleSpec>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleSpec {
    id: String,
    state: Option<StateSpec>,
    #[serde(default)]
    priority: i32,
    #[serde(default = "whole_recent")]
    region: String,
    #[serde(default)]
    visible_idle: bool,
    #[serde(default)]
    visible_blocker: bool,
    #[serde(default)]
    visible_working: bool,
    #[serde(default)]
    skip_state_update: bool,
    #[serde(default)]
    all: Vec<GateSpec>,
    #[serde(default)]
    any: Vec<GateSpec>,
    #[serde(default, rename = "not")]
    none: Vec<GateSpec>,
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GateSpec {
    #[serde(default)]
    all: Vec<GateSpec>,
    #[serde(default)]
    any: Vec<GateSpec>,
    #[serde(default, rename = "not")]
    none: Vec<GateSpec>,
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StateSpec {
    Idle,
    Working,
    Blocked,
    Unknown,
}

fn whole_recent() -> String {
    "whole_recent".into()
}

/// 编译好的一份规则文件。
#[derive(Debug)]
pub struct RuleSet {
    id: String,
    aliases: Vec<String>,
    /// 按优先级从高到低排好，优先级一样的保持文件里的先后：第一条命中的就是结果。
    rules: Vec<Rule>,
}

#[derive(Debug)]
struct Rule {
    id: String,
    /// `None` 是 unknown。
    state: Option<AgentState>,
    priority: i32,
    region: Region,
    /// 命中就说明界面上确实摆着 `state` 这个状态。
    visible: bool,
    skip: bool,
    gate: Gate,
}

#[derive(Debug)]
struct Gate {
    /// 已经转成小写。
    contains: Vec<String>,
    regex: Vec<Regex>,
    line_regex: Vec<Regex>,
    all: Vec<Gate>,
    any: Vec<Gate>,
    none: Vec<Gate>,
}

/// 求值用的输入。
#[derive(Clone, Copy, Debug, Default)]
pub struct Signals<'a> {
    /// 屏幕底部的文字：一行一行，行尾空白去掉，用 `\n` 连接。
    pub screen: &'a str,
    /// 程序用 OSC 0/2 设的标题，没有时为空。
    pub title: &'a str,
    /// 程序最近一次 OSC 9 报告里 `9;` 后面的部分，没有时为空。
    pub progress: &'a str,
}

/// 一次求值的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict<'r> {
    /// 有规则命中，给出了状态。
    Matched {
        state: AgentState,
        /// 界面上确实摆着这个状态，见规则的 `visible_*`。
        visible: bool,
        rule: &'r str,
    },
    /// 命中的规则要求这次不改状态。
    Hold { rule: &'r str },
    /// 没有规则命中，或者命中的规则也说不准。
    Unknown,
}

impl RuleSet {
    /// 解析、校验并编译一份规则文件。
    pub fn parse(text: &str) -> Result<Self, String> {
        let spec: FileSpec = toml::from_str(text).map_err(|err| err.to_string())?;
        if let Some(min) = spec.min_engine_version
            && min > ENGINE_VERSION
        {
            return Err(format!("needs rule engine version {min}, this one is {ENGINE_VERSION}"));
        }
        if spec.rules.is_empty() {
            return Err("no rules".into());
        }
        let mut rules = Vec::with_capacity(spec.rules.len());
        for rule in spec.rules {
            let id = rule.id.clone();
            rules.push(compile_rule(rule, spec.min_engine_version).map_err(|err| format!("rule {id}: {err}"))?);
        }
        // 稳定排序：优先级一样的保持原来的先后。
        rules.sort_by_key(|rule| std::cmp::Reverse(rule.priority));
        Ok(Self { id: spec.id, aliases: spec.aliases, rules })
    }

    /// 文件里的 `id` 或某个别名是不是指 `kind`。
    pub fn is_for(&self, kind: AgentKind) -> bool {
        std::iter::once(&self.id)
            .chain(&self.aliases)
            .any(|name| name == kind.label() || identify::agent_from_name(name) == Some(kind))
    }

    /// 按 `signals` 求值。
    pub fn evaluate<'r>(&'r self, signals: Signals<'_>) -> Verdict<'r> {
        // 每块区域只转一次小写，供 `contains` 用；同一块区域多条规则共用。
        let mut lowered: Vec<(Region, String)> = Vec::new();
        for rule in &self.rules {
            let text = rule.region.slice(signals.screen, signals.title, signals.progress);
            let lower = match lowered.iter().position(|(region, _)| *region == rule.region) {
                Some(i) => &lowered[i].1,
                None => {
                    lowered.push((rule.region, text.to_lowercase()));
                    &lowered[lowered.len() - 1].1
                }
            };
            if !rule.gate.matches(text, lower) {
                continue;
            }
            if rule.skip {
                return Verdict::Hold { rule: &rule.id };
            }
            return match rule.state {
                Some(state) => Verdict::Matched { state, visible: rule.visible, rule: &rule.id },
                None => Verdict::Unknown,
            };
        }
        Verdict::Unknown
    }
}

fn compile_rule(rule: RuleSpec, min_engine: Option<u32>) -> Result<Rule, String> {
    if rule.id.trim().is_empty() {
        return Err("empty rule id".into());
    }
    let region = Region::parse(&rule.region).ok_or_else(|| format!("unknown region {:?}", rule.region))?;
    if matches!(region, Region::TopNonEmptyLines(_)) && min_engine.is_some_and(|v| v < TOP_LINES_ENGINE_VERSION) {
        return Err(format!("top_non_empty_lines needs min_engine_version {TOP_LINES_ENGINE_VERSION}"));
    }
    let state = match rule.state {
        Some(StateSpec::Idle) => Some(AgentState::Idle),
        Some(StateSpec::Working) => Some(AgentState::Working),
        Some(StateSpec::Blocked) => Some(AgentState::Blocked),
        Some(StateSpec::Unknown) | None => None,
    };
    let any_visible = rule.visible_idle || rule.visible_blocker || rule.visible_working;
    if rule.skip_state_update && (rule.state != Some(StateSpec::Unknown) || any_visible) {
        return Err("skip_state_update needs state = \"unknown\" and no visible_* flag".into());
    }
    let visible = match state {
        Some(AgentState::Idle) => rule.visible_idle,
        Some(AgentState::Working) => rule.visible_working,
        Some(AgentState::Blocked) => rule.visible_blocker,
        None => false,
    };
    let gate = GateSpec {
        all: rule.all,
        any: rule.any,
        none: rule.none,
        contains: rule.contains,
        regex: rule.regex,
        line_regex: rule.line_regex,
    };
    Ok(Rule {
        id: rule.id,
        state,
        priority: rule.priority,
        region,
        visible,
        skip: rule.skip_state_update,
        gate: compile_gate(gate, false)?,
    })
}

/// 编译一个条件。直接写在 `not` 里的条件（`negated`）可以只有 `not`，其余的至少要有一个
/// 正面条件，免得写出什么都能命中的规则。
fn compile_gate(spec: GateSpec, negated: bool) -> Result<Gate, String> {
    let direct = spec.contains.len() + spec.regex.len() + spec.line_regex.len();
    let positive = direct > 0 || !spec.all.is_empty() || !spec.any.is_empty();
    if !positive && !(negated && !spec.none.is_empty()) {
        return Err("a condition needs contains, regex, line_regex, all or any".into());
    }
    let regex = |patterns: Vec<String>| -> Result<Vec<Regex>, String> {
        patterns
            .into_iter()
            .map(|pattern| Regex::new(&pattern).map_err(|err| format!("bad regex {pattern:?}: {err}")))
            .collect()
    };
    let nested = |gates: Vec<GateSpec>, negated: bool| -> Result<Vec<Gate>, String> {
        gates.into_iter().map(|gate| compile_gate(gate, negated)).collect()
    };
    Ok(Gate {
        contains: spec.contains.iter().map(|needle| needle.to_lowercase()).collect(),
        regex: regex(spec.regex)?,
        line_regex: regex(spec.line_regex)?,
        all: nested(spec.all, false)?,
        any: nested(spec.any, false)?,
        none: nested(spec.none, true)?,
    })
}

impl Gate {
    fn matches(&self, text: &str, lower: &str) -> bool {
        self.contains.iter().all(|needle| lower.contains(needle.as_str()))
            && self.regex.iter().all(|regex| regex.is_match(text))
            && self.line_regex.iter().all(|regex| text.lines().any(|line| regex.is_match(line)))
            && self.all.iter().all(|gate| gate.matches(text, lower))
            && (self.any.is_empty() || self.any.iter().any(|gate| gate.matches(text, lower)))
            && !self.none.iter().any(|gate| gate.matches(text, lower))
    }
}
