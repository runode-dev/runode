package dev.runode.mobile.features.model

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

private fun parse(screen: String): AgentPrompt? = AgentPrompt.parse(screen.split('\n'))

/** 一行一个拼成屏幕文字。 */
private fun screen(vararg lines: String): String = lines.joinToString("\n")

private fun texts(prompt: AgentPrompt?): List<String> = prompt?.options?.map { it.text } ?: emptyList()

class AgentPromptTests {
    /** Claude Code 问能不能跑命令：问题取选项上面两行，末尾的 `(esc)` 去掉，带 `❯` 的是默认项。 */
    @Test fun claudePermission() {
        val prompt = assertNotNull(
            parse(
                screen(
                    " Bash command",
                    "",
                    "   rm -rf build",
                    "   Remove the build directory",
                    "",
                    " Do you want to proceed?",
                    " ❯ 1. Yes",
                    "   2. Yes, and don't ask again for rm commands",
                    "   3. No, and tell Claude what to do differently (esc)",
                    "",
                ),
            ),
        )
        assertEquals(listOf("Remove the build directory", "Do you want to proceed?"), prompt.question)
        assertEquals(
            listOf("Yes", "Yes, and don't ask again for rm commands", "No, and tell Claude what to do differently"),
            texts(prompt),
        )
        assertEquals(listOf(1, 2, 3), prompt.options.map { it.number })
        assertEquals(listOf(true, false, false), prompt.options.map { it.selected })
    }

    /** 演示模式里那个会话的屏幕：去掉转义序列以后的样子。问题遇到上面的分隔线就停。 */
    @Test fun claudeDemoScreen() {
        val lines = listOf(
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
        )
        val prompt = assertNotNull(AgentPrompt.parse(lines))
        assertEquals(listOf("Run the protocol crate's tests", "Do you want to proceed?"), prompt.question)
        assertEquals(3, texts(prompt).size)
        assertEquals(true, prompt.options.first().selected)
    }

    /** 选项下面隔着空行的按键提示不影响。 */
    @Test fun claudeWithKeyHint() {
        val prompt = assertNotNull(
            parse("────────\n Edit file\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n\n Esc to cancel · Tab to amend\n"),
        )
        assertEquals(listOf("Edit file", "Do you want to proceed?"), prompt.question)
        assertEquals(listOf("Yes", "No"), texts(prompt))
    }

    /** Claude 问用户的选择题：选项中间夹着分隔线，底部的提示折成了两行。 */
    @Test fun claudeQuestion() {
        val prompt = assertNotNull(
            parse(
                "────────\n ☐ 午饭\n\n午饭吃什么？\n\n❯ 1. 面条\n  2. 米饭\n────────\n  3. Chat about this\n\n" +
                    "Enter to select · ↑/↓ to navigate · Esc to\ncancel\n",
            ),
        )
        assertEquals(listOf("☐ 午饭", "午饭吃什么？"), prompt.question)
        assertEquals(listOf("面条", "米饭", "Chat about this"), texts(prompt))
    }

    /** 手机上终端窄，长的选项折成两行：缩进到正文那一列的下一行接到这一项后面。 */
    @Test fun wrappedOptionsAreJoined() {
        val prompt = assertNotNull(
            parse(
                screen(
                    " Do you want to proceed?",
                    " ❯ 1. Yes",
                    "   2. Yes, and don't ask again for",
                    "      cargo test",
                    "   3. No, and tell Claude what to do",
                    "      differently (esc)",
                ),
            ),
        )
        assertEquals(
            listOf("Yes", "Yes, and don't ask again for cargo test", "No, and tell Claude what to do differently"),
            texts(prompt),
        )
    }

    /** Codex 问能不能跑命令：每项后面的 `(y)`、`(a)` 是快捷键，去掉。 */
    @Test fun codexApproval() {
        val prompt = assertNotNull(
            parse(
                screen(
                    "  Would you like to run the following command?",
                    "",
                    "  $ cargo test -p runode-protocol",
                    "",
                    "› 1. Yes, proceed (y)",
                    "  2. Yes, and don't ask again for this command (a)",
                    "  3. No, and tell Codex what to do differently (esc)",
                    "",
                    "  Press enter to confirm or esc to cancel",
                ),
            ),
        )
        assertEquals(listOf("Would you like to run the following command?", "$ cargo test -p runode-protocol"), prompt.question)
        assertEquals(
            listOf("Yes, proceed", "Yes, and don't ask again for this command", "No, and tell Codex what to do differently"),
            texts(prompt),
        )
        assertTrue(prompt.options[0].selected)
    }

    /** Codex 把两个选项排在同一行时只认出一项，不算。 */
    @Test fun codexInlineOptionsAreNotRecognized() {
        assertNull(parse("• Ran ls\n\n  Allow command?\n  › 1. Yes  2. No\n  Press enter to confirm or esc to cancel\n"))
    }

    /** Gemini 把提问画在框里，框下面还有一行状态栏：竖线去掉，默认项是 `●`。 */
    @Test fun geminiBox() {
        val prompt = assertNotNull(
            parse(
                screen(
                    "╭──────────────────────────────────────────╮",
                    "│ ?  Shell  cargo test -p runode-protocol  │",
                    "│                                          │",
                    "│ Allow execution of: 'cargo'?             │",
                    "│                                          │",
                    "│ ● 1. Yes, allow once                     │",
                    "│   2. Yes, allow always ...               │",
                    "│   3. No, suggest changes (esc)           │",
                    "│                                          │",
                    "╰──────────────────────────────────────────╯",
                    "~/dev/runode (main*)     no sandbox     gemini-2.5-pro (98% context left)",
                ),
            ),
        )
        assertEquals(listOf("?  Shell  cargo test -p runode-protocol", "Allow execution of: 'cargo'?"), prompt.question)
        assertEquals(listOf("Yes, allow once", "Yes, allow always ...", "No, suggest changes"), texts(prompt))
        assertEquals(listOf(true, false, false), prompt.options.map { it.selected })
    }

    /** 没有问题、只有选项也认：GitHub Copilot 的提问。 */
    @Test fun optionsWithoutQuestion() {
        val prompt = assertNotNull(parse("❯ 1. Yes\n  2. No\n↑↓ to navigate · Enter to select · Esc to cancel\n"))
        assertTrue(prompt.question.isEmpty())
        assertEquals(listOf("Yes", "No"), texts(prompt))
    }

    /** 括号里的字长了不是按键提示，留着。 */
    @Test fun longParenthesesStay() {
        val prompt = assertNotNull(parse("Pick one\n❯ 1. Yes (recommended)\n  2. No\n"))
        assertEquals(listOf("Yes (recommended)", "No"), texts(prompt))
    }

    /** agent 输出里一段普通的编号列表，下面还有好几行字：不贴着底部，不算提问。 */
    @Test fun numberedListInOutputIsNotAPrompt() {
        val output = screen(
            "● 我打算这样改：",
            "  1. 改读帧的地方",
            "  2. 正文长度加上限",
            "  3. 补测试",
            "",
            "● 先从第一步开始。",
            "  读帧的地方在 remote.rs。",
            "  改完再跑一遍测试。",
        )
        assertNull(parse(output))
    }

    /** 没有编号的提问认不出，退回通用的键。 */
    @Test fun questionWithoutNumbersIsNotRecognized() {
        assertNull(parse("Run this command?\n  Run (once) (y)\n  Skip (esc or n)\n"))
        assertNull(parse("Run this command?\n  ▶ Approve once\n    Reject\n  ↑↓ choose · ↵ confirm\n"))
    }

    /** 编号断开、不从 1 起、只有一项，都不算。 */
    @Test fun brokenNumberingIsNotRecognized() {
        assertNull(parse("Pick\n1. a\n3. b\n"))
        assertNull(parse("Pick\n2. a\n3. b\n"))
        assertNull(parse("Pick\n1. a\n2. b\n\n这里插了一段字\n3. c\n"))
        assertNull(parse("Do you trust this folder?\n  › 1. Yes, continue\n"))
        assertNull(parse(""))
    }

    /** 按一个数字键选不到的（十项以上）认不出。 */
    @Test fun tooManyOptionsAreNotRecognized() {
        assertNull(parse((1..10).joinToString("\n") { "  $it. option $it" }))
    }

    /** 编号后面要有空白：`1.5 GB` 这种数字不是选项。 */
    @Test fun numbersInTextAreNotOptions() {
        assertNull(parse("Download size\n1.5 GB\n2.0 GB\n"))
    }

    /** 是不是同一个提问只看问题和各项原文：光标挪了、状态栏的秒数变了还是它，问题或选项换了就不是。 */
    @Test fun askedIgnoresTheCursorAndTheLinesAround() {
        val first = assertNotNull(parse(" Proceed?\n ❯ 1. Yes\n   2. No (esc)\n\n 12s elapsed\n"))
        val moved = assertNotNull(parse(" Proceed?\n   1. Yes\n ❯ 2. No (esc)\n\n 13s elapsed\n"))
        val other = assertNotNull(parse(" Push to main?\n ❯ 1. Yes\n   2. No (esc)\n"))
        val reworded = assertNotNull(parse(" Proceed?\n ❯ 1. Yes\n   2. No, stop (esc)\n"))
        assertEquals(first.asked, moved.asked)
        assertNotEquals(first.asked, other.asked)
        assertNotEquals(first.asked, reworded.asked)
    }

    /** 卡片上并排的选项：两项时就是这两项，多于两项时是第一项和最后一项。 */
    @Test fun compactOptionsAreTheFirstAndTheLast() {
        fun numbers(count: Int) =
            AgentPrompt(emptyList(), (1..count).map { AgentPrompt.Option(it, "o$it", it == 1) }).compactOptions.map { it.number }
        assertEquals(listOf(1, 2), numbers(2))
        assertEquals(listOf(1, 3), numbers(3))
        assertEquals(listOf(1, 5), numbers(5))
    }

    /** 选项的键就是编号，读屏带着原文。 */
    @Test fun optionKeySendsTheNumber() {
        val option = AgentPrompt.Option(2, "No", false)
        assertEquals("2", option.key.key)
        assertEquals("2", option.key.label)
        assertTrue(option.key.accessibilityLabel.english!!.contains("No"))
    }
}
