//! 进行中卡片上的终端摘录：任务会话屏幕底部最后几行有字的输出（`runode_agent::prompt::excerpt`）。
//!
//! 只在看板开着任务页、页上有进行中的卡片时轮询（`Kanban::follow_excerpts`），每 `EXCERPT_INTERVAL` 在后台线程
//! 上把这些会话的屏幕各读一次，摘录变了才重画看板；没有这样的卡片、换到别的页或看板收起时停掉。

use std::{collections::HashMap, time::Duration};

use gpui::{Context, Task};
use runode_protocol::SessionId;

use super::{Kanban, store};

/// 隔多久读一次屏幕。
const EXCERPT_INTERVAL: Duration = Duration::from_secs(3);
/// 读屏幕底部多少行：agent 的输入框和状态栏占掉几行后还要剩得下摘录。
const SCREEN_LINES: u32 = 20;
/// 卡片上摘几行。
const EXCERPT_LINES: usize = 2;

/// 在读的会话和读到的摘录。
#[derive(Default)]
pub(super) struct Excerpts {
    /// 要读的会话，按卡片的先后。
    sessions: Vec<SessionId>,
    /// 读到的摘录；没读到、读到的是空的不在里面。
    lines: HashMap<SessionId, Vec<String>>,
    /// 轮询，丢掉就停。
    poll: Option<Task<()>>,
}

impl Excerpts {
    /// 会话 `id` 的摘录；没有时为空。
    pub(super) fn get(&self, id: SessionId) -> Option<&[String]> {
        self.lines.get(&id).map(Vec::as_slice)
    }

    /// 换上要读的会话，不在里面的摘录忘掉。返回要不要马上重读：多了新的会话。
    fn follow(&mut self, sessions: Vec<SessionId>) -> bool {
        let added = sessions.iter().any(|id| !self.sessions.contains(id));
        self.lines.retain(|id, _| sessions.contains(id));
        self.sessions = sessions;
        added
    }

    /// 记下一轮读到的屏幕摘录，读不到的（`None`）和空的去掉，读的时候已经不跟的会话不记。返回摘录变了没有。
    fn note(&mut self, read: Vec<(SessionId, Option<Vec<String>>)>) -> bool {
        let mut changed = false;
        for (id, lines) in read {
            if !self.sessions.contains(&id) {
                continue;
            }
            match lines.filter(|lines| !lines.is_empty()) {
                Some(lines) => {
                    if self.lines.get(&id) != Some(&lines) {
                        self.lines.insert(id, lines);
                        changed = true;
                    }
                }
                None => changed |= self.lines.remove(&id).is_some(),
            }
        }
        changed
    }
}

impl Kanban {
    /// 要读摘录的会话换成 `sessions`（任务页上进行中的卡片的会话）：为空时停掉轮询，多了新的会话时从头轮询，
    /// 新卡片马上读到。画的时候调。
    pub(super) fn follow_excerpts(&mut self, sessions: Vec<SessionId>, cx: &mut Context<Self>) {
        let added = self.excerpts.follow(sessions);
        if self.excerpts.sessions.is_empty() {
            self.excerpts.poll = None;
        } else if added || self.excerpts.poll.is_none() {
            self.excerpts.poll = Some(poll_excerpts(cx));
        }
    }
}

/// 每 `EXCERPT_INTERVAL` 读一轮 `Excerpts::sessions` 的屏幕，摘录变了就重画看板；看板没了就停。
fn poll_excerpts(cx: &mut Context<Kanban>) -> Task<()> {
    let read_screen = store::read_screen(cx);
    cx.spawn(async move |this, cx| {
        loop {
            let Ok(sessions) = this.update(cx, |this, _| this.excerpts.sessions.clone()) else { return };
            let read = cx
                .background_executor()
                .spawn(async move {
                    sessions
                        .into_iter()
                        .map(|id| {
                            let lines = read_screen(id, SCREEN_LINES)
                                .ok()
                                .map(|screen| runode_agent::prompt::excerpt(&screen, EXCERPT_LINES));
                            (id, lines)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let noted = this.update(cx, |this, cx| {
                if this.excerpts.note(read) {
                    cx.notify();
                }
            });
            if noted.is_err() {
                return;
            }
            cx.background_executor().timer(EXCERPT_INTERVAL).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &[&str]) -> Option<Vec<String>> {
        Some(text.iter().map(|line| (*line).to_owned()).collect())
    }

    /// 只在摘录变了时算变了；读不到、读到空的去掉摘录。
    #[test]
    fn notes_only_changes() {
        let mut excerpts = Excerpts::default();
        assert!(excerpts.follow(vec![SessionId(1), SessionId(2)]));
        assert!(excerpts.note(vec![(SessionId(1), lines(&["a", "b"])), (SessionId(2), None)]));
        assert!(!excerpts.note(vec![(SessionId(1), lines(&["a", "b"])), (SessionId(2), None)]));
        assert_eq!(excerpts.get(SessionId(1)), Some(["a".to_owned(), "b".to_owned()].as_slice()));
        assert!(excerpts.note(vec![(SessionId(1), lines(&[]))]));
        assert_eq!(excerpts.get(SessionId(1)), None);
        assert!(!excerpts.note(vec![(SessionId(1), None)]));
    }

    /// 不跟的会话的摘录忘掉，读的时候已经不跟的不记；只有多了会话才要马上重读。
    #[test]
    fn follows_the_shown_sessions() {
        let mut excerpts = Excerpts::default();
        excerpts.follow(vec![SessionId(1), SessionId(2)]);
        excerpts.note(vec![(SessionId(1), lines(&["a"])), (SessionId(2), lines(&["b"]))]);
        assert!(!excerpts.follow(vec![SessionId(2)]));
        assert_eq!(excerpts.get(SessionId(1)), None);
        assert!(!excerpts.note(vec![(SessionId(1), lines(&["a"]))]));
        assert_eq!(excerpts.get(SessionId(1)), None);
        assert!(excerpts.follow(vec![SessionId(2), SessionId(3)]));
        assert_eq!(excerpts.get(SessionId(2)), Some(["b".to_owned()].as_slice()));
    }
}
