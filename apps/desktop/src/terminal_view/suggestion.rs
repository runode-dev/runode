//! 按命令历史给出的灰字建议：重读输入、重查，判断现在能不能画，以及接受整条或一个词。

use std::time::Instant;

use runode_terminal::{PromptInput, history, session::Session};

use super::{ECHO_WAIT, Suggestion, TerminalView};

impl TerminalView {
    /// 屏幕上的输入或命令历史变了时重读输入，重查建议和高亮（`refresh_highlight`）；都没变就
    /// 沿用上次的结果。在绘制前调用。
    pub(super) fn refresh_input(&mut self) {
        let generation = self.config.command_suggestions.then(|| history::shared().generation());
        if !self.input_changed && generation.is_none_or(|generation| generation == self.history_generation) {
            return;
        }
        self.input_changed = false;
        if let Some(generation) = generation {
            self.history_generation = generation;
        }
        let wanted = self.config.command_suggestions || self.config.command_highlighting;
        // 只在正看着时读：冻结着的屏幕上的输入接不上 shell 了。
        let input = if wanted { self.screen.live().and_then(Session::prompt_input) } else { None };
        self.refresh_suggestion(input.as_ref());
        self.refresh_highlight(input.as_ref());
    }

    fn refresh_suggestion(&mut self, input: Option<&PromptInput>) {
        if !self.config.command_suggestions {
            self.suggestion = None;
            return;
        }
        // 光标后面还有字（包括插件自己画的建议）时不给建议，免得叠在一起。
        let Some(input) = input.filter(|input| input.at_end) else {
            self.suggestion = None;
            return;
        };
        let cwd = self.screen.live().and_then(Session::prompt_cwd);
        let rest = self.suggester.suggest(&history::shared(), input.before_cursor(), cwd.as_deref());
        self.suggestion = rest.map(|rest| Suggestion { rest, read_at: Instant::now() });
    }

    /// 现在该画出来的建议：开着输入法组字、有选区、视口没在底部、开着补全菜单时都不画。
    pub(super) fn visible_suggestion(&self) -> Option<&Suggestion> {
        self.suggestion.as_ref().filter(|_| {
            self.config.command_suggestions
                && self.completion.is_none()
                && self.marked_text.is_none()
                && self.screen.live().is_some_and(|session| !session.has_selection() && session.viewport_at_bottom())
        })
    }

    /// 接受显示着的建议：整条（`word` 为假）或下一个词，当作普通文字发给 shell，不走粘贴。
    /// 没有建议，或者刚发出的输入还没回显、建议可能是按旧屏幕算的时候不接受，返回 false。
    pub(super) fn accept_suggestion(&mut self, word: bool) -> bool {
        // 有了新输出还没重画时，先按现在的屏幕重查一次，不接受已经过时的建议。
        if self.input_changed {
            self.refresh_input();
        }
        // 程序把光标藏起来时建议也不画，同样不接受；没在看时也不接受。
        if self.screen.live_mut().is_none_or(|session| session.frame().cursor.is_none()) {
            return false;
        }
        let Some(suggestion) = self.visible_suggestion() else {
            return false;
        };
        let last_input = self.screen.live().and_then(Session::last_input);
        if last_input.is_some_and(|at| at > suggestion.read_at && at.elapsed() < ECHO_WAIT) {
            return false;
        }
        let text = if word { history::next_word(&suggestion.rest) } else { suggestion.rest.as_str() };
        let text = text.to_owned();
        match self.screen.live_mut() {
            Some(session) => {
                session.send_text(text.as_bytes());
                true
            }
            None => false,
        }
    }
}
