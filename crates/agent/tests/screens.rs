//! 内置规则对各 agent 典型界面的判断。屏幕文字的格式同 `Signals::screen`：行尾空白去掉，
//! 一行一个 `\n`。

use runode_agent::{RuleBook, Signals, Verdict};
use runode_types::agent::{AgentKind, AgentState};

fn verdict(kind: AgentKind, screen: &str, title: &str, progress: &str) -> &'static str {
    let book = RuleBook::new(None);
    let rules = book.rules(kind).unwrap_or_else(|| panic!("no rules for {kind:?}"));
    match rules.evaluate(Signals { screen: Some(screen), title, progress }) {
        Verdict::Matched { state: AgentState::Working, .. } => "working",
        Verdict::Matched { state: AgentState::Idle, .. } => "idle",
        Verdict::Matched { state: AgentState::Blocked, visible, rule } => {
            assert!(visible, "{kind:?} blocker {rule} should be visible");
            "blocked"
        }
        Verdict::Hold { .. } => "hold",
        Verdict::Unknown => "unknown",
    }
}

fn screen(kind: AgentKind, screen: &str) -> &'static str {
    verdict(kind, screen, "", "")
}

fn title(kind: AgentKind, title: &str) -> &'static str {
    verdict(kind, "", title, "")
}

fn progress(kind: AgentKind, progress: &str) -> &'static str {
    verdict(kind, "", "", progress)
}

#[test]
fn claude() {
    use AgentKind::Claude;
    assert_eq!(title(Claude, "⠂ 修复登录"), "working");
    assert_eq!(title(Claude, "◐ 修复登录"), "working");
    assert_eq!(title(Claude, "✳ 修复登录"), "idle");
    assert_eq!(progress(Claude, "4;0;"), "idle");
    assert_eq!(
        screen(
            Claude,
            "✻ Thinking… (12s · ↓ 1.2k tokens · esc to interrupt)\n\n────────\n❯\n────────\n  ⏵⏵ accept edits on\n"
        ),
        "working"
    );
    assert_eq!(screen(Claude, "● 改好了。\n\n────────\n❯\n────────\n  ? for shortcuts\n"), "idle");
    // 等后台 agent 的那行被提示挤开，底下 agent 清单里还在跑的那行照样算干活。
    let footer = "────────\n❯\n────────\n  ⏵⏵ auto mode on (shift+tab to cycle)\n\n  ⏺ main\n";
    let waiting =
        "✻ Waiting for 1 background agent to finish\n\n✦ Heads up · 上下文不够\n  1: Learn more   0: Dismiss\n\n";
    assert_eq!(
        screen(
            Claude,
            &format!("{waiting}{footer}  ◯ implementer  正在运行 typecheck      35m 1s · ↓ 129.4k tokens\n")
        ),
        "working"
    );
    assert_eq!(
        screen(Claude, &format!("● 改好了。\n\n{footer}  ◯ reviewer  在审查改动   30s · ↓ 33.7k tokens\n")),
        "working"
    );
    assert_eq!(screen(Claude, &format!("● 改好了。\n\n{footer}")), "idle");
    // 后台 shell 还在跑：Claude 停在输入框等它结束，还没干完。
    let prompt_box = "● 正在等 CI。\n\n────────\n❯\n────────\n";
    assert_eq!(screen(Claude, &format!("{prompt_box}  ⏵⏵ bypass permissions on · PR #6 · 1 shell\n")), "working");
    assert_eq!(screen(Claude, &format!("{prompt_box}  ? for shortcuts · 2 shells · PR #6\n")), "working");
    assert_eq!(screen(Claude, &format!("{prompt_box}  1 shell\n")), "working");
    assert_eq!(screen(Claude, &format!("{prompt_box}  ⏵⏵ bypass permissions on · PR #6\n")), "idle");
    // 输入框里打的字不算。
    assert_eq!(screen(Claude, "● 好了。\n\n────────\n❯ 1 shell\n────────\n  ? for shortcuts\n"), "idle");
    let running = "  ◯ reviewer  在审查改动   30s · ↓ 33.7k tokens\n";
    // 子 agent 在跑时主线程弹出权限提示，仍是等回答。
    assert_eq!(
        screen(
            Claude,
            &format!(
                "────────\n Bash command\n   rm -rf build\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n\n Esc to cancel · Tab to amend\n\n  ⏺ main\n{running}"
            )
        ),
        "blocked"
    );
    // 输入框里形如清单行的文字不算。
    assert_eq!(
        screen(Claude, &format!("● 改好了。\n\n────────\n❯ 看看这行\n{running}────────\n  ? for shortcuts\n")),
        "idle"
    );
    // 起了名字的会话：选单的提示行下面是名字分隔线和任务面板。
    assert_eq!(
        screen(
            Claude,
            " Which one?\n ❯ 1. A\n   2. B\n\n Enter to select · ↑/↓ to navigate · Esc to cancel\n\n──────── my-session ─\n  2 tasks (1 done, 1 open)\n  ✔ 写测试\n  ◻ 提交\n"
        ),
        "blocked"
    );
    assert_eq!(
        screen(
            Claude,
            " Bash command\n\n   rm -rf build\n   Remove the build directory\n\n Do you want to proceed?\n ❯ 1. Yes\n   2. Yes, and don't ask again for rm commands\n   3. No, and tell Claude what to do differently (esc)\n"
        ),
        "blocked"
    );
    assert_eq!(
        screen(
            Claude,
            "────────\n Edit file\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n\n Esc to cancel · Tab to amend\n"
        ),
        "blocked"
    );
    // 窄屏下 Claude 自己把底部的提示折成两行，提问框照样认得出。
    assert_eq!(
        screen(
            Claude,
            "────────\n ☐ 午饭\n\n午饭吃什么？\n\n❯ 1. 面条\n  2. 米饭\n────────\n  3. Chat about this\n\nEnter to select · ↑/↓ to navigate · Esc to\ncancel\n"
        ),
        "blocked"
    );
    // 正在看历史记录，看不出当前状态。
    assert_eq!(screen(Claude, "● earlier\n\n  Showing detailed transcript · ctrl+o to toggle\n"), "hold");
    // 光标上一行的提示词里提到「do you want to」，但输入框在：不算等用户。
    assert_eq!(screen(Claude, "> do you want to say yes?\n● ok\n────────\n❯\n────────\n"), "idle");
}

#[test]
fn codex() {
    use AgentKind::Codex;
    assert_eq!(title(Codex, "⠋ runode"), "working");
    assert_eq!(title(Codex, "runode"), "idle");
    assert_eq!(title(Codex, "Action Required | runode"), "blocked");
    assert_eq!(
        screen(
            Codex,
            "  Update available · 0.158.0 -> 0.160.0\n\n  › 1. Update now\n    2. Skip\n\n  enter continue · esc skip\n"
        ),
        "blocked"
    );
    assert_eq!(
        verdict(Codex, "• Working (5s • esc to interrupt)\n\n› Ask Codex to do anything\n", "runode", ""),
        "working"
    );
    assert_eq!(
        verdict(
            Codex,
            "• Ran ls\n\n  Allow command?\n  › 1. Yes  2. No\n  Press enter to confirm or esc to cancel\n",
            "runode",
            ""
        ),
        "blocked"
    );
    assert_eq!(
        screen(
            Codex,
            "> You are in /work/runode\n\n  Do you trust the contents of this directory? Working with untrusted contents\n  › 1. Yes, continue\n"
        ),
        "blocked"
    );
    // 计时停了、后面又有回答：不再算工作中。
    assert_eq!(screen(Codex, "• Working (5s • esc to interrupt)\n• 改好了\n\n›\n"), "unknown");
}

#[test]
fn pi() {
    use AgentKind::Pi;
    assert_eq!(screen(Pi, "⠋ Working...\n"), "working");
    assert_eq!(screen(Pi, "── ⠙ Working ────────\n"), "working");
    assert_eq!(screen(Pi, "π - runode\n> \n"), "unknown");
    // 回答正文或命令输出里提到这几个字不算在干活：不论在屏幕底部还是早就滚到上面。
    assert_eq!(screen(Pi, "The build is still working... let me check.\n\n> \n"), "unknown");
    let scrolled = format!("$ make\nstill working...\n{}> \n", "done\n".repeat(20));
    assert_eq!(screen(Pi, &scrolled), "unknown");
}

#[test]
fn gemini_opencode_kilo() {
    use AgentKind::*;
    assert_eq!(screen(Gemini, "╭────\n│ Apply this change?\n│ ● 1. Yes, allow once\n╰────\n"), "blocked");
    assert_eq!(screen(Gemini, "⠏ Reticulating splines (esc to cancel, 3s)\n"), "working");
    assert_eq!(screen(OpenCode, "△ Permission required\n  bash: rm -rf build\n"), "blocked");
    assert_eq!(screen(OpenCode, "  ■■■■⬝⬝⬝⬝  esc to interrupt\n"), "working");
    assert_eq!(screen(Kilo, "△ Permission required\n"), "blocked");
    assert_eq!(screen(Kilo, "⬝⬝⬝ esc interrupt\n"), "working");
}

#[test]
fn amp_antigravity_cline_cursor() {
    use AgentKind::*;
    assert_eq!(title(Amp, "⠋ amp"), "working");
    assert_eq!(title(Amp, "runode - amp - 修 bug"), "idle");
    assert_eq!(screen(Amp, "Run this command?\n  rm -rf build\n"), "blocked");
    assert_eq!(
        screen(Antigravity, "Allow running rm?\n↑/↓ Navigate · tab Amend · f full diff\nesc to cancel\n"),
        "blocked"
    );
    assert_eq!(screen(Antigravity, "⠋ Thinking about the plan\n╭───\n│ >\n╰───\n"), "working");
    assert_eq!(screen(Cline, "─────\n❯\n─────\n(Tab) Plan/Act · Shift+Tab auto-approve\n"), "idle");
    assert_eq!(screen(Cline, "Cline needs permission\nApprove tool call?\n[y] Approve  [n] Deny\n"), "blocked");
    assert_eq!(screen(Cline, "⠋ Thinking\n"), "working");
    assert_eq!(screen(Cursor, "⬢ Generating.\n  ctrl+c to stop\n"), "working");
    assert_eq!(screen(Cursor, "Run this command?\n  Run (once) (y)\n  Skip (esc or n)\n"), "blocked");
}

#[test]
fn devin_droid_copilot_grok() {
    use AgentKind::*;
    assert_eq!(screen(Devin, "❭\ncontext: 12%\n"), "idle");
    assert_eq!(screen(Devin, "Running tools · esc to interrupt\n"), "working");
    assert_eq!(screen(Devin, "Approve once\nSelect · Confirm · Esc cancel\n"), "blocked");
    assert_eq!(screen(Droid, "⠋ Thinking  (Press ESC to stop)\n"), "working");
    assert_eq!(
        screen(Droid, "> Yes, allow\n  No, cancel\nUse ↑↓ to navigate, Enter to select, Esc to cancel\n"),
        "blocked"
    );
    assert_eq!(screen(GithubCopilot, "◎ Thinking (Esc to cancel)\n"), "working");
    assert_eq!(
        screen(GithubCopilot, "❯ 1. Yes\n  2. No\n↑↓ to navigate · Enter to select · Esc to cancel\n"),
        "blocked"
    );
    assert_eq!(title(Grok, "grok"), "idle");
    assert_eq!(title(Grok, "⠋ Running tests - grok"), "working");
    assert_eq!(title(Grok, "⚠ Action Required - grok"), "blocked");
    assert_eq!(progress(Grok, "4;1;-1"), "working");
    assert_eq!(progress(Grok, "4;0;0"), "idle");
}

#[test]
fn hermes_kimi_kiro_letta() {
    use AgentKind::*;
    assert_eq!(title(Hermes, "⏳ hermes"), "working");
    assert_eq!(title(Hermes, "⚠️ hermes"), "blocked");
    assert_eq!(title(Hermes, "✓ hermes"), "idle");
    assert_eq!(screen(Kimi, "🌕\n"), "working");
    assert_eq!(screen(Kimi, "Run this command?\n  ▶ Approve once\n    Reject\n  ↑↓ choose · ↵ confirm\n"), "blocked");
    assert_eq!(screen(Kiro, "> Ask a question or describe a task\n"), "idle");
    assert_eq!(screen(Kiro, "Kiro is working · type to steer · ctrl+s to queue\n"), "working");
    assert_eq!(progress(Kiro, "4;3"), "working");
    assert_eq!(progress(Letta, "4;3;"), "blocked");
    assert_eq!(title(Letta, "⠋ letta"), "working");
    assert_eq!(screen(Letta, "› \n"), "idle");
    assert_eq!(screen(Letta, "› 帮我改一下\n"), "unknown");
}

#[test]
fn maki_muse_qoder_qwen() {
    use AgentKind::*;
    assert_eq!(screen(Maki, "❯ \n ⠋ [BUILD] sonnet\n"), "working");
    assert_eq!(screen(Maki, "❯ \n [BUILD] sonnet\n"), "idle");
    // 窄屏下状态栏盖掉了模式标签，看输入框；空输入框行尾的空格已经去掉了。
    assert_eq!(screen(Maki, "❯\n─────\n ctx 12% · sonnet\n"), "idle");
    assert_eq!(screen(Muse, "◆ Working (3s · esc to interrupt)\n"), "working");
    assert_eq!(screen(Muse, "⟩\n"), "idle");
    assert_eq!(screen(Muse, "Settings\nenter confirm · esc go back\n"), "hold");
    assert_eq!(screen(Qodercli, "⠋ Thinking (esc to cancel, 3s)\n"), "working");
    assert_eq!(screen(Qodercli, "Waiting for user confirmation\n  Allow  Reject\n"), "blocked");
    assert_eq!(title(Qwen, "✳ Qwen"), "blocked");
    assert_eq!(title(Qwen, "◐ Qwen"), "working");
    assert_eq!(screen(Qwen, "> Type your message or @path/to/file\n"), "idle");
}
