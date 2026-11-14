import Foundation
import Testing

@testable import RunodeFeatures

private func parse(_ screen: String) -> AgentPrompt? {
    AgentPrompt.parse(screen.split(separator: "\n", omittingEmptySubsequences: false).map(String.init))
}

private func texts(_ prompt: AgentPrompt?) -> [String] {
    prompt?.options.map(\.text) ?? []
}

@Suite struct AgentPromptTests {
    /// Claude Code 问能不能跑命令：问题取选项上面两行，末尾的 `(esc)` 去掉，带 `❯` 的是默认项。
    @Test func claudePermission() throws {
        let screen = """
             Bash command

               rm -rf build
               Remove the build directory

             Do you want to proceed?
             ❯ 1. Yes
               2. Yes, and don't ask again for rm commands
               3. No, and tell Claude what to do differently (esc)

            """
        let prompt = try #require(parse(screen))
        #expect(prompt.question == ["Remove the build directory", "Do you want to proceed?"])
        #expect(
            texts(prompt) == [
                "Yes", "Yes, and don't ask again for rm commands", "No, and tell Claude what to do differently",
            ])
        #expect(prompt.options.map(\.number) == [1, 2, 3])
        #expect(prompt.options.map(\.selected) == [true, false, false])
    }

    /// 演示模式里那个会话的屏幕：去掉转义序列以后的样子。问题遇到上面的分隔线就停。
    @Test func claudeDemoScreen() throws {
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
        let prompt = try #require(AgentPrompt.parse(screen))
        #expect(prompt.question == ["Run the protocol crate's tests", "Do you want to proceed?"])
        #expect(texts(prompt).count == 3)
        #expect(prompt.options.first?.selected == true)
    }

    /// 选项下面隔着空行的按键提示不影响。
    @Test func claudeWithKeyHint() throws {
        let prompt = try #require(
            parse("────────\n Edit file\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n\n Esc to cancel · Tab to amend\n"))
        #expect(prompt.question == ["Edit file", "Do you want to proceed?"])
        #expect(texts(prompt) == ["Yes", "No"])
    }

    /// Claude 问用户的选择题：选项中间夹着分隔线，底部的提示折成了两行。
    @Test func claudeQuestion() throws {
        let screen =
            "────────\n ☐ 午饭\n\n午饭吃什么？\n\n❯ 1. 面条\n  2. 米饭\n────────\n  3. Chat about this\n\nEnter to select · ↑/↓ to navigate · Esc to\ncancel\n"
        let prompt = try #require(parse(screen))
        #expect(prompt.question == ["☐ 午饭", "午饭吃什么？"])
        #expect(texts(prompt) == ["面条", "米饭", "Chat about this"])
    }

    /// 手机上终端窄，长的选项折成两行：缩进到正文那一列的下一行接到这一项后面。
    @Test func wrappedOptionsAreJoined() throws {
        let screen = """
             Do you want to proceed?
             ❯ 1. Yes
               2. Yes, and don't ask again for
                  cargo test
               3. No, and tell Claude what to do
                  differently (esc)
            """
        let prompt = try #require(parse(screen))
        #expect(
            texts(prompt) == [
                "Yes", "Yes, and don't ask again for cargo test", "No, and tell Claude what to do differently",
            ])
    }

    /// Codex 问能不能跑命令：每项后面的 `(y)`、`(a)` 是快捷键，去掉。
    @Test func codexApproval() throws {
        let screen = """
              Would you like to run the following command?

              $ cargo test -p runode-protocol

            › 1. Yes, proceed (y)
              2. Yes, and don't ask again for this command (a)
              3. No, and tell Codex what to do differently (esc)

              Press enter to confirm or esc to cancel
            """
        let prompt = try #require(parse(screen))
        #expect(prompt.question == ["Would you like to run the following command?", "$ cargo test -p runode-protocol"])
        #expect(
            texts(prompt) == [
                "Yes, proceed", "Yes, and don't ask again for this command", "No, and tell Codex what to do differently",
            ])
        #expect(prompt.options[0].selected)
    }

    /// Codex 把两个选项排在同一行时只认出一项，不算。
    @Test func codexInlineOptionsAreNotRecognized() {
        #expect(parse("• Ran ls\n\n  Allow command?\n  › 1. Yes  2. No\n  Press enter to confirm or esc to cancel\n") == nil)
    }

    /// Gemini 把提问画在框里，框下面还有一行状态栏：竖线去掉，默认项是 `●`。
    @Test func geminiBox() throws {
        let screen = """
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
            ~/dev/runode (main*)     no sandbox     gemini-2.5-pro (98% context left)
            """
        let prompt = try #require(parse(screen))
        #expect(prompt.question == ["?  Shell  cargo test -p runode-protocol", "Allow execution of: 'cargo'?"])
        #expect(texts(prompt) == ["Yes, allow once", "Yes, allow always ...", "No, suggest changes"])
        #expect(prompt.options.map(\.selected) == [true, false, false])
    }

    /// 没有问题、只有选项也认：GitHub Copilot 的提问。
    @Test func optionsWithoutQuestion() throws {
        let prompt = try #require(parse("❯ 1. Yes\n  2. No\n↑↓ to navigate · Enter to select · Esc to cancel\n"))
        #expect(prompt.question.isEmpty)
        #expect(texts(prompt) == ["Yes", "No"])
    }

    /// 括号里的字长了不是按键提示，留着。
    @Test func longParenthesesStay() throws {
        let prompt = try #require(parse("Pick one\n❯ 1. Yes (recommended)\n  2. No\n"))
        #expect(texts(prompt) == ["Yes (recommended)", "No"])
    }

    /// agent 输出里一段普通的编号列表，下面还有好几行字：不贴着底部，不算提问。
    @Test func numberedListInOutputIsNotAPrompt() {
        let screen = """
            ● 我打算这样改：
              1. 改读帧的地方
              2. 正文长度加上限
              3. 补测试

            ● 先从第一步开始。
              读帧的地方在 remote.rs。
              改完再跑一遍测试。
            """
        #expect(parse(screen) == nil)
    }

    /// 没有编号的提问认不出，退回通用的键。
    @Test func questionWithoutNumbersIsNotRecognized() {
        #expect(parse("Run this command?\n  Run (once) (y)\n  Skip (esc or n)\n") == nil)
        #expect(parse("Run this command?\n  ▶ Approve once\n    Reject\n  ↑↓ choose · ↵ confirm\n") == nil)
    }

    /// 编号断开、不从 1 起、只有一项，都不算。
    @Test func brokenNumberingIsNotRecognized() {
        #expect(parse("Pick\n1. a\n3. b\n") == nil)
        #expect(parse("Pick\n2. a\n3. b\n") == nil)
        #expect(parse("Pick\n1. a\n2. b\n\n这里插了一段字\n3. c\n") == nil)
        #expect(parse("Do you trust this folder?\n  › 1. Yes, continue\n") == nil)
        #expect(parse("") == nil)
    }

    /// 按一个数字键选不到的（十项以上）认不出。
    @Test func tooManyOptionsAreNotRecognized() {
        let screen = (1...10).map { "  \($0). option \($0)" }.joined(separator: "\n")
        #expect(parse(screen) == nil)
    }

    /// 编号后面要有空白：`1.5 GB` 这种数字不是选项。
    @Test func numbersInTextAreNotOptions() {
        #expect(parse("Download size\n1.5 GB\n2.0 GB\n") == nil)
    }

    /// 选项的键就是编号，读屏带着原文。
    @Test func optionKeySendsTheNumber() {
        let option = AgentPrompt.Option(number: 2, text: "No", selected: false)
        #expect(option.key.key == "2")
        #expect(option.key.label == "2")
        #expect(option.key.accessibilityLabel.contains("No"))
    }

    /// 列表卡片并排放第一项和最后一项，编号和原文照旧；只有两项时就是这两项。
    @Test func compactOptionsAreTheFirstAndTheLast() throws {
        let three = try #require(parse(" Proceed?\n ❯ 1. Yes\n   2. Yes, always\n   3. No (esc)\n"))
        #expect(three.compactOptions.map(\.number) == [1, 3])
        #expect(three.compactOptions.map(\.text) == ["Yes", "No"])
        #expect(three.compactOptions.map(\.selected) == [true, false])
        #expect(three.compactOptions.map(\.key.key) == ["1", "3"])
        let two = try #require(parse(" Proceed?\n   1. Yes\n ❯ 2. No\n"))
        #expect(two.compactOptions == two.options)
    }

    /// 光标停在哪一项、选项下面的字变没变都不影响是不是同一个提问；问题或选项的原文变了才不是。
    @Test func askedIgnoresTheCursorAndTheLinesAround() throws {
        let first = try #require(parse(" Proceed?\n ❯ 1. Yes\n   2. No (esc)\n\n 12s elapsed\n"))
        let moved = try #require(parse(" Proceed?\n   1. Yes\n ❯ 2. No (esc)\n\n 13s elapsed\n"))
        let other = try #require(parse(" Push to main?\n ❯ 1. Yes\n   2. No (esc)\n"))
        let reworded = try #require(parse(" Proceed?\n ❯ 1. Yes\n   2. No, stop (esc)\n"))
        #expect(first.asked == moved.asked)
        #expect(first.asked != other.asked)
        #expect(first.asked != reworded.asked)
    }
}
