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
        #expect(prompt.question == ["午饭吃什么？"])
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

    /// Claude 的选项标签下面一行是说明：放得下却换了行的是原文的换行，留着；说明太长被折到下一行的接回去，
    /// 中文接中文不加空格。终端多宽按满宽的分隔线算。
    @Test func claudeOptionDescriptionsKeepTheirLineBreaks() throws {
        let screen = [
            String(repeating: "─", count: 74),
            " ☐ 回调方式",
            "",
            "回调式接 CI 失败用哪条路？",
            "",
            "❯ 1. 仓库 webhook+中转转发 (Recommended)",
            "     runode 用 gh api 给仓库建 webhook，URL 里带电脑的 iroh 公钥；经 iroh",
            "     转给那台电脑。",
            "  2. 官方 GitHub App",
            "     用户点一下就装好，不要 admin；中转得存「安装→电脑」的对应关系，变成",
            "     有状态的服务",
            "  3. Type something.",
        ].joined(separator: "\n")
        let prompt = try #require(parse(screen))
        #expect(
            texts(prompt) == [
                "仓库 webhook+中转转发 (Recommended)\nrunode 用 gh api 给仓库建 webhook，URL 里带电脑的 iroh 公钥；经 iroh 转给那台电脑。",
                "官方 GitHub App\n用户点一下就装好，不要 admin；中转得存「安装→电脑」的对应关系，变成有状态的服务",
                "Type something.",
            ])
    }

    /// 中文说明没有空格：下一行开头的字放得下就是原文的换行。分隔线和选项之间隔着凑满的几行问题，宽度照样按它算。
    @Test func chineseDescriptionsAndALongQuestionKeepLineBreaks() throws {
        let screen = [
            String(repeating: "─", count: 74),
            " Bash command",
            "   cargo test",
            "   跑测试",
            " Do you want to proceed?",
            " ❯ 1. Yes, and don't ask again",
            "      for cargo test",
            "   2. 官方 GitHub App",
            "      用户点一下就装好不需要管理员权限中转得存对应关系",
            "   3. No",
        ].joined(separator: "\n")
        let prompt = try #require(parse(screen))
        #expect(
            texts(prompt) == [
                "Yes, and don't ask again\nfor cargo test",
                "官方 GitHub App\n用户点一下就装好不需要管理员权限中转得存对应关系",
                "No",
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
    @Test func optionKeySendsTheNumber() throws {
        let prompt = try #require(parse(" Proceed?\n ❯ 1. Yes\n   2. No\n"))
        let key = prompt.key(for: prompt.options[1])
        #expect(key.key == "2")
        #expect(key.keys == ["2"])
        #expect(key.label == "2")
        #expect(key.accessibilityLabel.contains("No"))
    }

    /// 有预览的提问按了编号键再按回车；多选题按编号键是切换勾选，不跟着回车。
    @Test func optionKeyAddsEnterOnlyWhereTheNumberJustMovesTheCursor() throws {
        let preview = try #require(parse(claudePreview))
        #expect(preview.key(for: preview.options[1]).keys == ["2", "enter"])
        #expect(preview.key(for: preview.options[1]).accessibilityLabel.contains("Revert the change"))
        var checkboxes = try #require(parse(" Pick\n ❯ 1. [ ] A\n   2. [x] B\n"))
        checkboxes.needsEnter = true
        #expect(checkboxes.key(for: checkboxes.options[1]).keys == ["2"])
    }

    /// Claude 给选项配了代码预览：预览框画在选项右边，框下面一行说明，再下面是不带编号的「Chat about this」和
    /// 按键提示。框裁掉后照常认，按了编号键还要回车。
    private let claudePreview = """
        ───────────────────────────────────────────────────────────────────────────────────────────────────────
         ☐ Makefile

        │ Should I keep or revert the Makefile change that made the layers target use $(CARGO) instead of bare
        │ cargo?

        ❯ 1. Keep the change              ┌──────────────────────────────────────────┐
          2. Revert the change            │ layers: ## ...                           │
                                          │         $(CARGO) deny check bans         │
                                          │         python3 scripts/check-layers.py  │
                                          └──────────────────────────────────────────┘

                                          Notes: press n to add notes

        ───────────────────────────────────────────────────────────────────────────────────────────────────────
          Chat about this

        Enter to select · ↑/↓ to navigate · n to add notes · Esc to cancel

        """

    @Test func claudeQuestionWithPreviews() throws {
        let prompt = try #require(parse(claudePreview))
        #expect(
            prompt.question == [
                "Should I keep or revert the Makefile change that made the layers target use $(CARGO) instead of bare",
                "cargo?",
            ])
        #expect(texts(prompt) == ["Keep the change", "Revert the change"])
        #expect(prompt.options.map(\.selected) == [true, false])
        #expect(prompt.needsEnter)
    }

    /// 预览框跟着光标换内容，框比选项多几行时选项下面的框里也是预览，不算续行。
    @Test func claudePreviewTallerThanOptionsWithCursorMoved() throws {
        let screen = """
             Which one?

              1. Alpha            ┌──────────┐
            ❯ 2. Beta             │ beta()   │
              3. Gamma            │   .x()   │
                                  │   .y()   │
                                  └──────────┘

                                  Notes: press n to add notes

              Chat about this

            Enter to select · Esc to cancel

            """
        let prompt = try #require(parse(screen))
        #expect(prompt.question == ["Which one?"])
        #expect(texts(prompt) == ["Alpha", "Beta", "Gamma"])
        #expect(prompt.options.map(\.selected) == [false, true, false])
        #expect(prompt.needsEnter)
    }

    /// 没有预览的提问按编号键就选定。
    @Test func plainPromptNeedsNoEnter() throws {
        let prompt = try #require(parse(" Do you want to proceed?\n ❯ 1. Yes\n   2. No (esc)\n"))
        #expect(!prompt.needsEnter)
    }

    /// 选项比预览那一栏高时，框下面的说明画在某个选项右边，裁掉，不混进选项原文。
    @Test func claudePreviewShorterThanOptions() throws {
        let screen = """
             Which one?

            ❯ 1. Alpha            ┌──────────┐
              2. Beta             │ a()      │
              3. Gamma            └──────────┘
              4. Delta
              5. Echo             Notes: press n to add notes

            Enter to select · Esc to cancel

            """
        let prompt = try #require(parse(screen))
        #expect(texts(prompt) == ["Alpha", "Beta", "Gamma", "Delta", "Echo"])
        #expect(prompt.needsEnter)
    }

    /// 上面的输出里有编号项旁边画着框，下面是没有预览的提问：不算预览，选项也不裁。
    @Test func boxBesideANumberedLineAboveAPlainPrompt() throws {
        let screen = """
             1. Client   ┌────────┐
                         │ server │
                         └────────┘

             Do you want to proceed?
             ❯ 1. Yes
               2. Yes, and don't ask again for this command
               3. No, and tell Claude what to do differently (esc)

            """
        let prompt = try #require(parse(screen))
        #expect(prompt.question == ["Do you want to proceed?"])
        #expect(texts(prompt).count == 3)
        #expect(!prompt.needsEnter)
    }

    /// 预览里的示意图自己画着框：选项右边靠下的内框不当成预览框。
    @Test func claudePreviewWithABoxInside() throws {
        let screen = """
            ❯ 1. Sidebar          ┌────────────────────┐
              2. Tabs             │  ┌──────┐          │
              3. Single           │  │ nav  │  main    │
                                  │  └──────┘          │
                                  └────────────────────┘

                                  Notes: press n to add notes

              Chat about this

            Enter to select · Esc to cancel

            """
        let prompt = try #require(parse(screen))
        #expect(texts(prompt) == ["Sidebar", "Tabs", "Single"])
        #expect(prompt.needsEnter)
    }

    /// 列表卡片并排放第一项和最后一项，编号和原文照旧；只有两项时就是这两项。
    @Test func compactOptionsAreTheFirstAndTheLast() throws {
        let three = try #require(parse(" Proceed?\n ❯ 1. Yes\n   2. Yes, always\n   3. No (esc)\n"))
        #expect(three.compactOptions.map(\.number) == [1, 3])
        #expect(three.compactOptions.map(\.text) == ["Yes", "No"])
        #expect(three.compactOptions.map(\.selected) == [true, false])
        #expect(three.compactOptions.map { three.key(for: $0).key } == ["1", "3"])
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

    /// 一次问几个问题时问题上面的标签条不算问题。
    @Test func questionTabBarIsNotQuestion() throws {
        let screen = "────────\n←  ☐ 引导步骤  ✔ Submit  →\n\n新用户引导要包含哪些步骤？\n\n❯ 1. [ ] 选主题\n  2. [ ] 装 skill\n"
        let prompt = try #require(parse(screen))
        #expect(prompt.question == ["新用户引导要包含哪些步骤？"])
    }

    /// Claude Code 的多选题：选项以复选框开头，勾没勾和去掉复选框后的原文分得出来，后面不带复选框的几项照旧；
    /// 光标停的那一项不算默认项，读屏的原文不带复选框，比是不是同一个提问时仍带着复选框。
    @Test func claudeMultiSelect() throws {
        let screen = """
             ☐ 引导步骤
             新用户引导先做哪一步？
             ❯ 1. [ ] 首页双 tab 切换
               2. [✔] SuperAgent 按钮
               3. [x] 搜索按钮
               4. Type something.
               5. Chat about this

            """
        let prompt = try #require(parse(screen))
        #expect(prompt.isMultiSelect)
        #expect(prompt.options.map { $0.checkbox?.checked } == [false, true, true, nil, nil])
        #expect(prompt.options.map { $0.checkbox?.text } == ["首页双 tab 切换", "SuperAgent 按钮", "搜索按钮", nil, nil])
        #expect(
            prompt.options.map(\.displayText) == [
                "首页双 tab 切换", "SuperAgent 按钮", "搜索按钮", "Type something.", "Chat about this",
            ])
        #expect(prompt.options.first?.selected == true)
        #expect(prompt.options.allSatisfy { !prompt.isDefault($0) })
        #expect(prompt.options.first.map { prompt.key(for: $0).accessibilityLabel.contains("[") } == false)
        #expect(prompt.asked.options.first == "[ ] 首页双 tab 切换")
        #expect(prompt.question == ["新用户引导先做哪一步？"])

        let toggled = try #require(parse(screen.replacingOccurrences(of: "[ ] 首页", with: "[✔] 首页")))
        #expect(toggled.asked != prompt.asked)
    }

    /// 单选题、方括号里不是勾的字都不算复选框；单选题光标停的那一项是默认项。
    @Test func notACheckbox() throws {
        let prompt = try #require(parse(" Do you want to proceed?\n ❯ 1. Yes\n   2. [beta] No\n"))
        #expect(!prompt.isMultiSelect)
        #expect(prompt.options.allSatisfy { $0.checkbox == nil })
        #expect(prompt.options.map(\.displayText) == ["Yes", "[beta] No"])
        #expect(prompt.options.map(prompt.isDefault) == [true, false])
        #expect(AgentPrompt.Option(number: 1, text: "[?] Maybe", selected: false).checkbox == nil)
        #expect(AgentPrompt.Option(number: 1, text: "[ ]", selected: false).checkbox?.text == "")
    }

    /// 原文第一行是标签，换行后面的是说明，几行说明并成一行；复选框不算进标签。
    @Test func labelAndDetail() {
        let described = AgentPrompt.Option(number: 1, text: "[x] --run CMD\n先跑命令\n再起 agent", selected: false)
        #expect(described.label == "--run CMD")
        #expect(described.detail == "先跑命令 再起 agent")
        let plain = AgentPrompt.Option(number: 2, text: "Type something.", selected: false)
        #expect(plain.label == "Type something.")
        #expect(plain.detail == nil)
    }
}
