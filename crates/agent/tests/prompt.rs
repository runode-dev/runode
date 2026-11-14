//! 从屏幕底部认出 agent 带编号选项的提问，取底部几行输出，去掉选项上的光标记号。

use runode_agent::prompt::{MAX_QUESTION_LINES, Prompt, excerpt, parse};

fn texts(prompt: &Prompt) -> Vec<&str> {
    prompt.options.iter().map(|option| option.text.as_str()).collect()
}

fn numbers(prompt: &Prompt) -> Vec<u8> {
    prompt.options.iter().map(|option| option.number).collect()
}

fn selected(prompt: &Prompt) -> Vec<bool> {
    prompt.options.iter().map(|option| option.selected).collect()
}

/// Claude Code 问能不能跑命令：问题往上取到标题，带出命令本身；末尾的 `(esc)` 去掉，带 `❯` 的是默认项。
#[test]
fn claude_permission() {
    let screen = "\
 Bash command

   rm -rf build
   Remove the build directory

 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and don't ask again for rm commands
   3. No, and tell Claude what to do differently (esc)
";
    let prompt = parse(screen).unwrap();
    assert_eq!(
        prompt.question,
        ["Bash command", "rm -rf build", "Remove the build directory", "Do you want to proceed?"]
    );
    assert_eq!(
        texts(&prompt),
        ["Yes", "Yes, and don't ask again for rm commands", "No, and tell Claude what to do differently"]
    );
    assert_eq!(numbers(&prompt), [1, 2, 3]);
    assert_eq!(selected(&prompt), [true, false, false]);
}

/// 去掉转义序列以后的 Claude 会话屏幕：问题遇到上面的分隔线就停。
#[test]
fn claude_demo_screen() {
    let screen = [
        "● 先跑一遍协议的测试，确认帧格式没被改坏。",
        "",
        "● Read(crates/protocol/src/remote.rs)",
        "  ⎿  Read 412 lines",
        "",
        "────────────────────────────────────────────────────────────",
        " Bash command",
        "",
        "   cargo test -p runode-protocol",
        "   Run the protocol crate's tests",
        "",
        " Do you want to proceed?",
        " ❯ 1. Yes",
        "   2. Yes, and don't ask again for cargo test",
        "   3. No, and tell Claude what to do differently (esc)",
    ]
    .join("\n");
    let prompt = parse(&screen).unwrap();
    assert_eq!(
        prompt.question,
        ["Bash command", "cargo test -p runode-protocol", "Run the protocol crate's tests", "Do you want to proceed?"]
    );
    assert_eq!(texts(&prompt).len(), 3);
    assert!(prompt.options[0].selected);
}

/// 问题多于 `MAX_QUESTION_LINES` 行时取最靠近选项的几行。
#[test]
fn question_keeps_the_lines_nearest_the_options() {
    let screen = "a\nb\nc\nd\ne\nf\n❯ 1. Yes\n  2. No\n";
    let prompt = parse(screen).unwrap();
    assert_eq!(prompt.question.len(), MAX_QUESTION_LINES);
    assert_eq!(prompt.question.last().map(String::as_str), Some("f"));
}

/// 选项下面隔着空行的按键提示不影响。
#[test]
fn claude_with_key_hint() {
    let prompt =
        parse("────────\n Edit file\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n\n Esc to cancel · Tab to amend\n")
            .unwrap();
    assert_eq!(prompt.question, ["Edit file", "Do you want to proceed?"]);
    assert_eq!(texts(&prompt), ["Yes", "No"]);
}

/// Claude 问用户的选择题：选项中间夹着分隔线，底部的提示折成了两行。
#[test]
fn claude_question() {
    let screen = "────────\n ☐ 午饭\n\n午饭吃什么？\n\n❯ 1. 面条\n  2. 米饭\n────────\n  3. Chat about this\n\nEnter to select · ↑/↓ to navigate · Esc to\ncancel\n";
    let prompt = parse(screen).unwrap();
    assert_eq!(prompt.question, ["☐ 午饭", "午饭吃什么？"]);
    assert_eq!(texts(&prompt), ["面条", "米饭", "Chat about this"]);
}

/// 终端窄时长的选项折成两行：缩进到正文那一列的下一行接到这一项后面。
#[test]
fn wrapped_options_are_joined() {
    let screen = "\
 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and don't ask again for
      cargo test
   3. No, and tell Claude what to do
      differently (esc)";
    let prompt = parse(screen).unwrap();
    assert_eq!(
        texts(&prompt),
        ["Yes", "Yes, and don't ask again for cargo test", "No, and tell Claude what to do differently"]
    );
}

/// Codex 问能不能跑命令：每项后面的 `(y)`、`(a)` 是快捷键，去掉。
#[test]
fn codex_approval() {
    let screen = "  Would you like to run the following command?

  $ cargo test -p runode-protocol

› 1. Yes, proceed (y)
  2. Yes, and don't ask again for this command (a)
  3. No, and tell Codex what to do differently (esc)

  Press enter to confirm or esc to cancel";
    let prompt = parse(screen).unwrap();
    assert_eq!(prompt.question, ["Would you like to run the following command?", "$ cargo test -p runode-protocol"]);
    assert_eq!(
        texts(&prompt),
        ["Yes, proceed", "Yes, and don't ask again for this command", "No, and tell Codex what to do differently"]
    );
    assert!(prompt.options[0].selected);
}

/// Codex 把两个选项排在同一行时只认出一项，不算。
#[test]
fn codex_inline_options_are_not_recognized() {
    assert_eq!(
        parse("• Ran ls\n\n  Allow command?\n  › 1. Yes  2. No\n  Press enter to confirm or esc to cancel\n"),
        None
    );
}

/// Gemini 把提问画在框里，框下面还有一行状态栏：竖线去掉，默认项是 `●`。
#[test]
fn gemini_box() {
    let screen = "\
╭──────────────────────────────────────────╮
│ ?  Shell  cargo test -p runode-protocol  │
│                                          │
│ Allow execution of: 'cargo'?             │
│                                          │
│ ● 1. Yes, allow once                     │
│   2. Yes, allow always ...               │
│   3. No, suggest changes (esc)           │
│                                          │
╰──────────────────────────────────────────╯
~/dev/runode (main*)     no sandbox     gemini-2.5-pro (98% context left)";
    let prompt = parse(screen).unwrap();
    assert_eq!(prompt.question, ["?  Shell  cargo test -p runode-protocol", "Allow execution of: 'cargo'?"]);
    assert_eq!(texts(&prompt), ["Yes, allow once", "Yes, allow always ...", "No, suggest changes"]);
    assert_eq!(selected(&prompt), [true, false, false]);
}

/// 没有问题、只有选项也认：GitHub Copilot 的提问。
#[test]
fn options_without_question() {
    let prompt = parse("❯ 1. Yes\n  2. No\n↑↓ to navigate · Enter to select · Esc to cancel\n").unwrap();
    assert!(prompt.question.is_empty());
    assert_eq!(texts(&prompt), ["Yes", "No"]);
}

/// 括号里的字长了不是按键提示，留着。
#[test]
fn long_parentheses_stay() {
    let prompt = parse("Pick one\n❯ 1. Yes (recommended)\n  2. No\n").unwrap();
    assert_eq!(texts(&prompt), ["Yes (recommended)", "No"]);
}

/// agent 输出里一段普通的编号列表，下面还有好几行字：不贴着底部，不算提问。
#[test]
fn numbered_list_in_output_is_not_a_prompt() {
    let screen = "\
● 我打算这样改：
  1. 改读帧的地方
  2. 正文长度加上限
  3. 补测试

● 先从第一步开始。
  读帧的地方在 remote.rs。
  改完再跑一遍测试。";
    assert_eq!(parse(screen), None);
}

/// 没有编号的提问认不出。
#[test]
fn question_without_numbers_is_not_recognized() {
    assert_eq!(parse("Run this command?\n  Run (once) (y)\n  Skip (esc or n)\n"), None);
    assert_eq!(parse("Run this command?\n  ▶ Approve once\n    Reject\n  ↑↓ choose · ↵ confirm\n"), None);
}

/// 编号断开、不从 1 起、只有一项，都不算。
#[test]
fn broken_numbering_is_not_recognized() {
    assert_eq!(parse("Pick\n1. a\n3. b\n"), None);
    assert_eq!(parse("Pick\n2. a\n3. b\n"), None);
    assert_eq!(parse("Pick\n1. a\n2. b\n\n这里插了一段字\n3. c\n"), None);
    assert_eq!(parse("Do you trust this folder?\n  › 1. Yes, continue\n"), None);
    assert_eq!(parse(""), None);
}

/// 按一个数字键选不到的（十项以上）认不出。
#[test]
fn too_many_options_are_not_recognized() {
    let screen = (1..=10).map(|n| format!("  {n}. option {n}")).collect::<Vec<_>>().join("\n");
    assert_eq!(parse(&screen), None);
}

/// 编号后面要有空白：`1.5 GB` 这种数字不是选项。
#[test]
fn numbers_in_text_are_not_options() {
    assert_eq!(parse("Download size\n1.5 GB\n2.0 GB\n"), None);
}

/// 三项时首尾两项的编号、原文和默认项照屏幕；光标停在后面的项也认。
#[test]
fn first_and_last_options() {
    let three = parse(" Proceed?\n ❯ 1. Yes\n   2. Yes, always\n   3. No (esc)\n").unwrap();
    let (first, last) = (&three.options[0], &three.options[2]);
    assert_eq!((first.number, first.text.as_str(), first.selected), (1, "Yes", true));
    assert_eq!((last.number, last.text.as_str(), last.selected), (3, "No", false));
    let two = parse(" Proceed?\n   1. Yes\n ❯ 2. No\n").unwrap();
    assert_eq!(numbers(&two), [1, 2]);
    assert_eq!(selected(&two), [false, true]);
}

/// Claude Code 画在框里的权限框：框线去掉，问题带出命令，折下来的路径接到选项后面。
#[test]
fn claude_boxed_permission() {
    let screen = "\
● 先跑一遍测试。

╭───────────────────────────────────────────────────────────╮
│ Bash command                                              │
│                                                           │
│   cargo test -p runode-agent                              │
│   Run the agent crate's tests                             │
│                                                           │
│ Do you want to proceed?                                   │
│ ❯ 1. Yes                                                  │
│   2. Yes, and don't ask again for cargo test commands in  │
│      /Users/me/dev/runode                                 │
│   3. No, and tell Claude what to do differently (esc)     │
╰───────────────────────────────────────────────────────────╯
";
    let prompt = parse(screen).unwrap();
    assert_eq!(
        prompt.question,
        ["Bash command", "cargo test -p runode-agent", "Run the agent crate's tests", "Do you want to proceed?"]
    );
    assert_eq!(
        texts(&prompt),
        [
            "Yes",
            "Yes, and don't ask again for cargo test commands in /Users/me/dev/runode",
            "No, and tell Claude what to do differently",
        ]
    );
    assert_eq!(selected(&prompt), [true, false, false]);
}

/// Codex 的审批：问题里带着理由和命令，快捷键提示去掉。
#[test]
fn codex_approval_with_reason() {
    let screen = "
  Would you like to run the following command?

  Reason: Run the agent crate's tests

  $ cargo test -p runode-agent

› 1. Yes, proceed (y)
  2. Yes, and don't ask again for commands that start with `cargo test` (p)
  3. No, and tell Codex what to do differently (esc)

  Press enter to confirm or esc to cancel
";
    let prompt = parse(screen).unwrap();
    assert_eq!(
        prompt.question,
        [
            "Would you like to run the following command?",
            "Reason: Run the agent crate's tests",
            "$ cargo test -p runode-agent",
        ]
    );
    assert_eq!(
        texts(&prompt),
        [
            "Yes, proceed",
            "Yes, and don't ask again for commands that start with `cargo test`",
            "No, and tell Codex what to do differently",
        ]
    );
    assert_eq!(numbers(&prompt), [1, 2, 3]);
}

/// Gemini 的四项确认框，框下面有转圈的提示和状态栏两行。
#[test]
fn gemini_four_options_with_status_lines() {
    let screen = "\
╭──────────────────────────────────────────────────────────────╮
│ ?  Shell cargo test -p runode-agent (Run the agent tests)    │
│                                                              │
│   cargo test -p runode-agent                                 │
│                                                              │
│ Allow execution of: 'cargo'?                                 │
│                                                              │
│ ● 1. Yes, allow once                                         │
│   2. Yes, allow always ...                                   │
│   3. Modify with external editor                             │
│   4. No, suggest changes (esc)                               │
│                                                              │
╰──────────────────────────────────────────────────────────────╯
⠏ Waiting for user confirmation...
~/dev/runode (main*)   no sandbox (see /docs)   gemini-2.5-pro (97% context left)";
    let prompt = parse(screen).unwrap();
    assert_eq!(
        prompt.question,
        [
            "?  Shell cargo test -p runode-agent (Run the agent tests)",
            "cargo test -p runode-agent",
            "Allow execution of: 'cargo'?",
        ]
    );
    assert_eq!(
        texts(&prompt),
        ["Yes, allow once", "Yes, allow always ...", "Modify with external editor", "No, suggest changes"]
    );
    assert_eq!(selected(&prompt), [true, false, false, false]);
}

/// 底部有输入框时取框上面的输出：跳过空行，框线和缩进去掉。
#[test]
fn excerpt_takes_the_lines_above_the_input_box() {
    let screen = "\
● 测试都过了。
  ⎿  42 passed

● 改好了，要我提交吗？

╭──────────────────────────────╮
│ >                            │
╰──────────────────────────────╯
  ? for shortcuts
";
    assert_eq!(excerpt(screen, 2), ["⎿  42 passed", "● 改好了，要我提交吗？"]);
    assert_eq!(excerpt(screen, 10), ["● 测试都过了。", "⎿  42 passed", "● 改好了，要我提交吗？"]);
}

/// 没有输入框时从最底下取：跳过空行和制表线，框两边的竖线去掉。
#[test]
fn excerpt_without_input_box() {
    let screen = "done\n────────\n│ line a │\n\n│ line b │\n\n";
    assert_eq!(excerpt(screen, 2), ["line a", "line b"]);
    assert_eq!(excerpt(screen, 5), ["done", "line a", "line b"]);
}

/// 框下面的字多于 `MAX_TRAILING_LINES` 行时那是输出里的框，不是输入框，照常从最底下取。
#[test]
fn excerpt_ignores_a_box_far_from_the_bottom() {
    let screen = "\
╭──────────╮
│ Welcome  │
╰──────────╯
one
two
three";
    assert_eq!(excerpt(screen, 2), ["two", "three"]);
}

#[test]
fn excerpt_of_nothing_is_empty() {
    assert!(excerpt("", 2).is_empty());
    assert!(excerpt("some output", 0).is_empty());
    assert!(excerpt("\n\n────\n", 2).is_empty());
}
