//! 前端交来的任务和定时能不能用：本机任何进程和配对的手机都能发这些消息，交来的内容查过才存、才往 shell
//! 里打。

use runode_git::Repo;
use runode_protocol::todo::{TodoSpec, When};

/// 提示词最长这么多字节：它要放进环境变量，系统给参数和环境变量的空间有限。
const MAX_PROMPT: usize = 64 * 1024;
/// 新交来的任务和定时的标题最长这么多字节（`check_title`）：看板每张卡片都带着它，列看板（`ListTodos`）
/// 一次回所有未完成的任务。
const MAX_TITLE: usize = 1024;

/// 前端给的任务内容能不能用：本机任何进程和配对的手机都能发这些消息，见模块文档。
pub(super) fn check_spec(spec: &TodoSpec) -> Result<(), String> {
    check_text(spec)?;
    if !spec.dir.is_absolute() || !spec.dir.is_dir() {
        return Err(format!("{} is not a directory", spec.dir.display()));
    }
    if spec.worktree && Repo::open(&spec.dir).is_none() {
        return Err(format!("{} is not in a git repository, so it cannot have a worktree", spec.dir.display()));
    }
    Ok(())
}

/// 前端新交来的标题不能太长。只查新交来的：开始任务时重查（`start_todo`）不查它。
pub(super) fn check_title(spec: &TodoSpec) -> Result<(), String> {
    if spec.title.len() > MAX_TITLE {
        return Err(format!("the title is longer than {} KiB; put the details in the prompt", MAX_TITLE / 1024));
    }
    Ok(())
}

/// `check_spec` 里不看目录的部分：标题、提示词、agent、skill 和附件。附件只查是不是绝对路径，文件可以
/// 后来才有。
pub(super) fn check_text(spec: &TodoSpec) -> Result<(), String> {
    if spec.title.trim().is_empty() {
        return Err("a todo needs a title".into());
    }
    if spec.time_limit == Some(0) {
        return Err("the time limit must be at least a minute".into());
    }
    let one_line = |name: &str| !name.trim().is_empty() && !name.chars().any(char::is_control);
    if !spec.skills.iter().all(|skill| one_line(skill)) {
        return Err("each skill must be a one-line name".into());
    }
    // 名字要原样拼进 shell 命令，只收不用加引号的。
    if spec.subagent.as_deref().is_some_and(|name| !runode_agent::models::subagent_name(name)) {
        return Err("the subagent name may only contain letters, digits and -_.:".into());
    }
    if let Some(path) = spec.attachments.iter().find(|path| !path.is_absolute()) {
        return Err(format!("the attachment {} is not an absolute path", path.display()));
    }
    let prompt = spec.instructions();
    if prompt.len() > MAX_PROMPT {
        return Err(format!("the prompt is longer than {} KiB", MAX_PROMPT / 1024));
    }
    if prompt.contains('\0') {
        return Err("the prompt contains a NUL character".into());
    }
    if spec.agent.trim().is_empty() || spec.agent.chars().any(char::is_control) {
        return Err("the agent must be a one-line command".into());
    }
    Ok(())
}

pub(super) fn check_when(when: &When, now: u64) -> Result<(), String> {
    match *when {
        When::Once { at } if at <= now => Err("that time has passed".into()),
        When::Daily { hour, minute, days } if hour >= 24 || minute >= 60 || days >= 1 << 7 => {
            Err("not a valid time of day".into())
        }
        When::Every { minutes: 0 } => Err("the interval must be at least a minute".into()),
        When::Cron { ref expr } if !runode_todo::valid_cron(expr) => {
            Err(format!("{expr:?} is not a cron expression: minute hour day month weekday"))
        }
        When::Cron { ref expr } if runode_todo::next_fire(when, now).is_none() => {
            Err(format!("{expr:?} never comes"))
        }
        When::Unknown => Err("unknown kind of schedule".into()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_the_spec_and_the_schedule() {
        let dir = std::env::temp_dir();
        let spec = |title: &str, agent: &str| TodoSpec {
            title: title.into(),
            prompt: String::new(),
            dir: dir.clone(),
            agent: agent.into(),
            worktree: false,
            ..TodoSpec::default()
        };
        assert!(check_spec(&spec("t", "claude")).is_ok());
        assert!(check_spec(&spec("  ", "claude")).is_err());
        assert!(check_spec(&spec("t", "claude\nrm -rf ~")).is_err());
        assert!(check_spec(&TodoSpec { dir: "relative".into(), ..spec("t", "claude") }).is_err());
        assert!(check_spec(&TodoSpec { prompt: "x".repeat(MAX_PROMPT + 1), ..spec("t", "claude") }).is_err());
        let named = |skills: &[&str], subagent: Option<&str>| TodoSpec {
            skills: skills.iter().map(|skill| (*skill).to_owned()).collect(),
            subagent: subagent.map(str::to_owned),
            ..spec("t", "claude")
        };
        assert!(check_spec(&named(&["review"], Some("reviewer"))).is_ok());
        assert!(check_spec(&named(&["review\nrm -rf ~"], None)).is_err());
        assert!(check_spec(&named(&[" "], None)).is_err());
        assert!(check_spec(&named(&[], Some("a\nb"))).is_err());
        assert!(check_spec(&named(&[], Some("x'; touch /tmp/pwn; #"))).is_err());
        assert!(check_spec(&TodoSpec { attachments: vec!["notes.md".into()], ..spec("t", "claude") }).is_err());
        // 附上的文件列进提示词，算进长度。
        let long = TodoSpec { attachments: vec![format!("/{}", "x".repeat(MAX_PROMPT)).into()], ..spec("t", "claude") };
        assert!(check_spec(&long).is_err());
        assert!(check_title(&spec(&"x".repeat(MAX_TITLE), "claude")).is_ok());
        assert!(check_title(&spec(&"x".repeat(MAX_TITLE + 1), "claude")).is_err());
        // 开始任务时的重查不管标题长度。
        assert!(check_spec(&spec(&"x".repeat(MAX_TITLE + 1), "claude")).is_ok());

        assert!(check_when(&When::Once { at: 101 }, 100).is_ok());
        assert!(check_when(&When::Once { at: 100 }, 100).is_err());
        assert!(check_when(&When::Daily { hour: 23, minute: 59, days: 0b111_1111 }, 0).is_ok());
        assert!(check_when(&When::Daily { hour: 24, minute: 0, days: 0 }, 0).is_err());
        assert!(check_when(&When::Daily { hour: 9, minute: 0, days: 0b1000_0000 }, 0).is_err());
        assert!(check_when(&When::Every { minutes: 0 }, 0).is_err());
        assert!(check_when(&When::Unknown, 0).is_err());
        let cron = |expr: &str| When::Cron { expr: expr.into() };
        assert!(check_when(&cron("30 9 * * 1-5"), 1_760_000_000).is_ok());
        assert!(check_when(&cron("30 9 * *"), 1_760_000_000).unwrap_err().contains("not a cron expression"));
        assert!(check_when(&cron("0 0 31 2 *"), 1_760_000_000).unwrap_err().contains("never comes"));
    }
}
