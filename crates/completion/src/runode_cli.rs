//! runode 自己的命令行的动态补全：会话的写法和配对过的设备，都由 runode 命令行自己列出来。
//! 命令规格和别的命令的一样是一份 JSON，和 crate 放在一起，构建脚本把它和子模块里的规格
//! 一起编进二进制；它要跟着命令行的参数解析一起改。

use warp_command_signatures::{
    CommandBuilder, CommandSignatureGenerators, Generator, GeneratorResults, GeneratorResultsCollector, Suggestion,
};

/// `runode list` 显示的会话标识有几位，补全给的也是这么长的前缀。
const SHORT_ID: usize = 8;

/// 连不上 app、列不出会话时照样给的写法。
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
        .add_generator("sessions", Generator::script(runode("list --json"), sessions))
        .add_generator("devices", Generator::script(runode("remote devices --json"), devices))
}

/// 跑 runode 的命令行。环境里有 `RUNODE_BIN` 时用它指的那个可执行文件：PATH 里可能还有另一个
/// 版本的 runode，开发版和正式版连的宿主不是同一个。
fn runode(args: &str) -> CommandBuilder {
    CommandBuilder::single_command_and_ignore_stderr(format!("\"${{RUNODE_BIN:-runode}}\" {args}"))
}

/// `runode list --json` 里的一个会话。
struct Session<'a> {
    id: &'a str,
    title: Option<&'a str>,
    agent: Option<&'a str>,
    state: Option<&'a str>,
    foreground: Option<&'a str>,
    cwd: Option<&'a str>,
    /// 在窗口里时是 `(窗口, 工作区, 标签, 分屏)` 的序号。
    place: Option<[u64; 4]>,
    /// 相对于命令行所在终端的位置：`self`、`left` 等。
    rel: Vec<&'a str>,
}

impl<'a> Session<'a> {
    fn parse(value: &'a serde_json::Value) -> Option<Self> {
        let text = |key: &str| value[key].as_str().filter(|text| !text.is_empty());
        let place = &value["place"];
        let index = |key: &str| place[key].as_u64();
        Some(Self {
            id: text("id")?,
            title: text("title"),
            agent: text("agent"),
            state: text("state"),
            foreground: text("foreground"),
            cwd: text("cwd"),
            place: (|| Some([index("window")?, index("workspace")?, index("tab")?, index("pane")?]))(),
            rel: value["rel"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .filter_map(|r| r.as_str())
                .collect(),
        })
    }

    /// 候选的说明：agent 和它的状态、前台程序、标题。
    fn describe(&self) -> Option<String> {
        let agent = self.agent.map(|agent| match self.state {
            Some(state) => format!("{agent} ({state})"),
            None => agent.to_owned(),
        });
        let parts: Vec<&str> = [agent.as_deref(), self.foreground, self.title].into_iter().flatten().collect();
        (!parts.is_empty()).then(|| parts.join(" · "))
    }

    fn suggest(&self, value: impl Into<String>) -> Suggestion {
        match self.describe() {
            Some(description) => Suggestion::with_description(value, description),
            None => Suggestion::new(value),
        }
    }

    /// 目录的最后一段，`cwd:NAME` 按它比。
    fn dir_name(&self) -> Option<&'a str> {
        self.cwd?.trim_end_matches('/').rsplit('/').next().filter(|name| !name.is_empty())
    }
}

/// `runode list --json` 的输出换成会话的各种写法，每种只给正好对上一个会话的（对上几个时命令行
/// 会报错）：相对于自己的位置（`self`、`left`、`next` 等）、短标识、按序号的位置（离自己越近
/// 写得越短）、`agent:`、`cwd:`、`title:`。说明是对上的那个会话的 agent、前台程序和标题。
/// 生成器要带着所在终端的 `RUNODE_SESSION` 跑，命令行才知道自己是哪个；连不上 app 时只给
/// 固定的几种写法。
fn sessions(output: &str) -> GeneratorResults {
    let Ok(listing) = serde_json::from_str::<serde_json::Value>(output) else {
        return SELECTORS
            .iter()
            .map(|(name, about)| Suggestion::with_description(*name, *about))
            .collect_ordered_results();
    };
    let all = listing["sessions"].as_array().map(Vec::as_slice).unwrap_or_default();
    let live: Vec<Session> =
        all.iter().filter(|session| session["exited"] != true).filter_map(Session::parse).collect();
    let own = listing["self"].as_str().and_then(|own| live.iter().find(|session| session.id == own));
    let own_place = own.and_then(|own| own.place);
    let mut out = Vec::new();

    // 相对于自己的位置，按 `self`、上下左右、前后的顺序。
    for rel in ["self", "left", "right", "up", "down"] {
        out.extend(live.iter().find(|session| session.rel.contains(&rel)).map(|session| session.suggest(rel)));
    }
    if let Some([w, k, t, p]) = own_place {
        let mut tab: Vec<&Session> = live
            .iter()
            .filter(|session| session.place.is_some_and(|[w2, k2, t2, _]| (w2, k2, t2) == (w, k, t)))
            .collect();
        tab.sort_by_key(|session| session.place.map(|place| place[3]));
        if let Some(i) = tab.iter().position(|session| session.place.is_some_and(|place| place[3] == p))
            && tab.len() > 1
        {
            out.push(tab[(i + 1) % tab.len()].suggest("next"));
            out.push(tab[(i + tab.len() - 1) % tab.len()].suggest("prev"));
        }
    }

    out.extend(live.iter().map(|session| session.suggest(session.id.get(..SHORT_ID).unwrap_or(session.id))));

    for session in &live {
        let Some([w, k, t, p]) = session.place else {
            continue;
        };
        let written = match own_place {
            Some([w2, k2, t2, _]) if (w2, k2, t2) == (w, k, t) => format!("pane:{p}"),
            Some([w2, k2, ..]) if (w2, k2) == (w, k) => format!("tab:{t}.{p}"),
            Some([w2, ..]) if w2 == w => format!("ws:{k}/tab:{t}.{p}"),
            _ => format!("win:{w}/ws:{k}/tab:{t}.{p}"),
        };
        out.push(session.suggest(written));
    }

    let unique = |matches: &dyn Fn(&Session) -> bool| -> Option<&Session> {
        let mut found = live.iter().filter(|session| matches(session));
        let one = found.next()?;
        found.next().is_none().then_some(one)
    };
    for session in &live {
        let Some(agent) = session.agent else {
            continue;
        };
        if unique(&|other| other.agent == Some(agent)).is_some() {
            out.push(session.suggest(format!("agent:{agent}")));
        } else if let Some(state) = session.state
            && unique(&|other| other.agent == Some(agent) && other.state == Some(state)).is_some()
        {
            out.push(session.suggest(format!("agent:{agent}:{state}")));
        }
    }
    for session in &live {
        let Some(name) = session.dir_name() else {
            continue;
        };
        if unique(&|other| other.dir_name() == Some(name)).is_some() {
            out.push(session.suggest(format!("cwd:{name}")));
        }
    }
    for session in &live {
        let Some(title) = session.title else {
            continue;
        };
        let lower = title.to_lowercase();
        let contains = |other: &Session| other.title.is_some_and(|t| t.to_lowercase().contains(&lower));
        if unique(&contains).is_some() {
            out.push(session.suggest(format!("title:{title}")));
        }
    }
    out.into_iter().collect_ordered_results()
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

    /// 一个会话在 `runode list --json` 里的样子。
    fn session(id: &str, agent: Option<(&str, &str)>, title: &str, cwd: &str, place: [u32; 4], rel: &[&str]) -> String {
        let [w, k, t, p] = place;
        serde_json::json!({
            "id": id.repeat(32 / id.len()),
            "title": title,
            "agent": agent.map(|a| a.0),
            "state": agent.map(|a| a.1),
            "foreground": agent.map(|a| a.0),
            "cwd": cwd,
            "exited": false,
            "place": {"window": w, "workspace": k, "tab": t, "pane": p},
            "rel": rel,
        })
        .to_string()
    }

    #[test]
    fn sessions_are_written_every_way_that_picks_exactly_one() {
        let sessions_json = [
            session("a", None, "zsh", "/src/runode", [1, 1, 1, 1], &["self"]),
            session("b", Some(("claude", "idle")), "fix bug", "/src/runode", [1, 1, 1, 2], &["right"]),
            session("c", Some(("claude", "working")), "docs", "/src/site", [1, 1, 2, 1], &[]),
            session("d", Some(("codex", "idle")), "fix bug again", "/tmp", [2, 1, 1, 1], &[]),
        ];
        let exited = r#"{"id":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee","title":"gone","exited":true}"#;
        let output = format!(r#"{{"self":"{}","sessions":[{},{exited}]}}"#, "a".repeat(32), sessions_json.join(","));
        let listed = values(sessions(&output));
        let names: Vec<&str> = listed.iter().map(|(value, _)| value.as_str()).collect();
        assert_eq!(
            names,
            [
                "self",
                "right",
                "next",
                "prev", //
                "aaaaaaaa",
                "bbbbbbbb",
                "cccccccc",
                "dddddddd", //
                "pane:1",
                "pane:2",
                "tab:2.1",
                "win:2/ws:1/tab:1.1", //
                "agent:claude:idle",
                "agent:claude:working",
                "agent:codex", //
                "cwd:site",
                "cwd:tmp", //
                "title:zsh",
                "title:docs",
                "title:fix bug again",
            ]
        );
        // 说明是对上的那个会话；同一个标签里只有两个分屏时前后都是另一个。
        let about = |name: &str| listed.iter().find(|(value, _)| value == name).unwrap().1.clone();
        assert_eq!(about("right"), Some("claude (idle) · claude · fix bug".into()));
        assert_eq!(about("next"), about("prev"));
        assert_eq!(about("self"), Some("zsh".into()));
    }

    #[test]
    fn without_the_app_only_the_fixed_selectors_are_given() {
        assert_eq!(values(sessions("")).len(), SELECTORS.len());
        // 不在 runode 的终端里（不知道自己是哪个）时没有相对的写法，位置写全。
        let output = format!(r#"{{"self":null,"sessions":[{}]}}"#, session("a", None, "", "", [1, 2, 3, 4], &[]));
        let names: Vec<String> = values(sessions(&output)).into_iter().map(|(value, _)| value).collect();
        assert_eq!(names, ["aaaaaaaa", "win:1/ws:2/tab:3.4"]);
    }

    #[test]
    fn devices_are_listed_by_full_id() {
        let output =
            r#"[{"device_id":"00112233445566778899aabbccddeeff","name":"iPhone","paired_at":1,"last_seen":2}]"#;
        assert_eq!(values(devices(output)), [("00112233445566778899aabbccddeeff".into(), Some("iPhone".into()))]);
        assert!(values(devices("")).is_empty());
    }
}
