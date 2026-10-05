//! 各 agent 用哪份规则：内置的，或者用户放在规则目录里的同名文件。

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use runode_shared_types::agent::AgentKind;

use crate::rules::RuleSet;

/// 编进二进制的规则，按 `AgentKind::label` 找。
const BUILTIN: &[(&str, &str)] = &[
    ("amp", include_str!("../rules/amp.toml")),
    ("agy", include_str!("../rules/antigravity.toml")),
    ("claude", include_str!("../rules/claude.toml")),
    ("cline", include_str!("../rules/cline.toml")),
    ("codex", include_str!("../rules/codex.toml")),
    ("cursor", include_str!("../rules/cursor.toml")),
    ("devin", include_str!("../rules/devin.toml")),
    ("droid", include_str!("../rules/droid.toml")),
    ("gemini", include_str!("../rules/gemini.toml")),
    ("grok", include_str!("../rules/grok.toml")),
    ("hermes", include_str!("../rules/hermes.toml")),
    ("kilo", include_str!("../rules/kilo.toml")),
    ("kimi", include_str!("../rules/kimi.toml")),
    ("kiro", include_str!("../rules/kiro.toml")),
    ("letta", include_str!("../rules/letta.toml")),
    ("maki", include_str!("../rules/maki.toml")),
    ("muse", include_str!("../rules/muse.toml")),
    ("opencode", include_str!("../rules/opencode.toml")),
    ("pi", include_str!("../rules/pi.toml")),
    ("qodercli", include_str!("../rules/qodercli.toml")),
    ("qwen", include_str!("../rules/qwen.toml")),
    ("copilot", include_str!("../rules/github-copilot.toml")),
];

/// 全部 agent 的规则。每种 agent 第一次用到时才读文件、编译正则，之后一直用这一份；改了
/// 用户规则要重启才生效。
#[derive(Debug)]
pub struct RuleBook {
    /// 用户规则的目录，见 `runode_paths::Dirs::agent_detection_dir`。
    user_dir: Option<PathBuf>,
    /// 按 `AgentKind::ALL` 的顺序。`None` 是这种 agent 没有规则。
    sets: [OnceLock<Option<Arc<RuleSet>>>; AgentKind::ALL.len()],
    /// 用户规则用不了时的说明，由 `take_warnings` 取走记日志。
    warnings: Mutex<Vec<String>>,
}

impl RuleBook {
    /// 用户规则放在 `user_dir` 里；为 `None` 时只用内置的。
    pub fn new(user_dir: Option<&Path>) -> Self {
        Self {
            user_dir: user_dir.map(Path::to_path_buf),
            sets: std::array::from_fn(|_| OnceLock::new()),
            warnings: Mutex::new(Vec::new()),
        }
    }

    /// `kind` 的规则；用户规则优先，读不了、写错了或者 `id` 对不上时用内置的。
    pub fn rules(&self, kind: AgentKind) -> Option<Arc<RuleSet>> {
        let slot = AgentKind::ALL.iter().position(|&k| k == kind)?;
        self.sets[slot].get_or_init(|| self.load(kind)).clone()
    }

    /// 取走攒下的警告。
    pub fn take_warnings(&self) -> Vec<String> {
        std::mem::take(&mut *self.warnings.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn load(&self, kind: AgentKind) -> Option<Arc<RuleSet>> {
        if let Some(path) = self.user_dir.as_ref().map(|dir| dir.join(format!("{}.toml", kind.label())))
            && path.exists()
        {
            let loaded = std::fs::read_to_string(&path).map_err(|err| err.to_string()).and_then(|text| {
                let set = RuleSet::parse(&text)?;
                if set.is_for(kind) { Ok(set) } else { Err(format!("its id is not {}", kind.label())) }
            });
            match loaded {
                Ok(set) => return Some(Arc::new(set)),
                Err(err) => self
                    .warnings
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(format!("ignored agent rules {}: {err}", path.display())),
            }
        }
        builtin(kind).map(|text| {
            Arc::new(RuleSet::parse(text).unwrap_or_else(|err| panic!("built-in {} rules are invalid: {err}", kind.label())))
        })
    }
}

fn builtin(kind: AgentKind) -> Option<&'static str> {
    BUILTIN.iter().find(|(label, _)| *label == kind.label()).map(|(_, text)| *text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_rule_file_compiles_and_names_its_agent() {
        assert_eq!(BUILTIN.len(), 22);
        for (label, text) in BUILTIN {
            let kind = AgentKind::ALL.into_iter().find(|kind| kind.label() == *label).unwrap();
            let set = RuleSet::parse(text).unwrap_or_else(|err| panic!("{label}: {err}"));
            assert!(set.is_for(kind), "{label}");
        }
        // 这两种没有内置规则，只按进程认出来。
        let book = RuleBook::new(None);
        assert!(book.rules(AgentKind::Omp).is_none());
        assert!(book.rules(AgentKind::Mastracode).is_none());
        assert!(book.rules(AgentKind::Other).is_none());
    }
}
