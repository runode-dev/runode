package dev.runode.mobile.protocol

import kotlin.math.roundToInt
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long
import kotlin.test.assertEquals
import kotlin.test.assertFails
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlin.test.fail
import org.junit.Test

private val sampleId = SessionId("0123456789abcdef0011223344556677")!!
private val sampleSize = GridSize(cols = 80u, rows = 24u, cellWidthPx = 16u, cellHeightPx = 32u)

class AgentKindTests {
    /** 每种 agent 的显示名、转圈和颜色和宿主的一样。 */
    @Test
    fun matchesTheHost() {
        val agents = Samples.messagesRoot.getValue("agents").jsonArray
        assertTrue(agents.isNotEmpty())
        for (element in agents) {
            val sample = element.jsonObject
            val label = sample.getValue("kind").jsonPrimitive.content
            val kind = AgentKind(label)
            assertEquals(sample.getValue("display_name").jsonPrimitive.content, kind.displayName, label)
            assertEquals(
                sample.getValue("spinner_frames").jsonArray.map { it.jsonPrimitive.content },
                kind.spinner.frames,
                label,
            )
            assertEquals(
                sample.getValue("spinner_frame_ms").jsonPrimitive.int,
                (kind.spinner.frameTime * 1000).roundToInt(),
                label,
            )
            val color = kind.spinnerColor?.let { (it.r.toLong() shl 16) or (it.g.toLong() shl 8) or it.b.toLong() }
            val expected = sample.getValue("spinner_color").takeUnless { it is JsonNull }?.jsonPrimitive?.long
            assertEquals(expected, color, label)
        }
    }
}

class ClientMessageTests {
    private fun encoded(message: ClientMsg) = encodeToJson(ClientMsg.serializer(), message)

    @Test
    fun matchesTheHost() {
        val cases = mapOf(
            "hello" to ClientMsg.Hello(
                protocol = 4u,
                build = "ios-0.1.0+1",
                client = ClientKind.Mobile,
                caps = Caps(snapshot = false, vtReplay = true, shellReport = true),
                session = null,
                device = "Ethan 的 iPhone",
            ),
            "list" to ClientMsg.ListSessions,
            "layout_request" to ClientMsg.Layout(req = 0u),
            "open" to ClientMsg.Open(req = 7u, placement = Placement.Tab, near = null, cwd = null, focus = false),
            "open_workspace" to ClientMsg.OpenWorkspace(req = 8u, dir = "/Users/ethan/dev/中文", focus = false),
            "open_workspace_named" to
                ClientMsg.OpenWorkspace(req = 8u, dir = "/Users/ethan/dev/中文", focus = false, name = "后端"),
            "rename_workspace" to ClientMsg.RenameWorkspace(req = 9u, window = 1u, workspace = 2u, name = "前端"),
            "list_dirs" to ClientMsg.ListDirs(req = 5u, path = "/Users/ethan"),
            "list_dirs_home" to ClientMsg.ListDirs(req = 5u, path = null),
            "list_tasks" to ClientMsg.ListTasks(req = 5u, dir = "/Users/ethan/dev/中文"),
            "find_commands" to ClientMsg.FindCommands(req = 6u, path = null, programs = listOf("claude", "codex")),
            "find_commands_path" to ClientMsg.FindCommands(
                req = 6u,
                path = "/Users/ethan/.local/bin:/usr/bin",
                programs = listOf("claude"),
            ),
            "spawn" to ClientMsg.Spawn(
                req = 3u,
                size = sampleSize,
                cwd = null,
                integration = IntegrationMode.Detect,
                start = true,
            ),
            "attach_vt" to ClientMsg.Attach(id = sampleId, size = null, mode = AttachMode.VtReplay),
            "attach_meta" to ClientMsg.Attach(id = sampleId, size = null, mode = AttachMode.MetaOnly),
            "attach_size" to ClientMsg.Attach(id = sampleId, size = sampleSize, mode = AttachMode.VtReplay),
            "detach" to ClientMsg.Detach(id = sampleId),
            "resize" to ClientMsg.Resize(id = sampleId, size = sampleSize),
            "focus" to ClientMsg.Focus(id = sampleId, focused = true),
            "kill" to ClientMsg.Kill(id = sampleId),
            "read_screen" to ClientMsg.ReadScreen(id = sampleId, lines = 8u),
            "read_screen_command" to ClientMsg.ReadScreen(id = sampleId, lines = null, command = 1u),
            "send_keys" to ClientMsg.SendKeys(req = 5u, id = sampleId, keys = listOf("1", "enter", "esc", "up")),
            "paste" to ClientMsg.Paste(req = 6u, id = sampleId, text = "继续，用方案 2\n"),
            "push_register" to ClientMsg.PushRegister(
                req = 10u,
                token = "80f0c8b3a4e2d1c0ffeeddccbbaa99887766554433221100aabbccddeeff0011",
                env = ApnsEnv.Production,
                bundle = "cn.barey.runode",
                machine = "6F9619FF-8B86-D011-B42D-00C04FC964FF",
                machineName = "Ethan 的 MacBook Pro",
            ),
            "push_unregister" to ClientMsg.PushRegister(
                req = 11u,
                token = null,
                env = ApnsEnv.Development,
                bundle = "cn.barey.runode",
                machine = "6F9619FF-8B86-D011-B42D-00C04FC964FF",
                machineName = "Ethan 的 MacBook Pro",
            ),
            "push_register_fcm" to ClientMsg.PushRegister(
                req = 12u,
                token = "q2b9Xk0Lr7mVt3pZw8sYc1nH4dF6gJ-aE5uR_oTiBlM",
                env = ApnsEnv.Production,
                bundle = "dev.runode.mobile",
                machine = "6F9619FF-8B86-D011-B42D-00C04FC964FF",
                machineName = "Ethan 的 MacBook Pro",
                service = PushService.Fcm,
            ),
            "keep_awake" to ClientMsg.KeepAwake,
            "unpair" to ClientMsg.Unpair(req = 9u),
        )
        for ((name, message) in cases) {
            assertTrue(sameJson(encoded(message), Samples.message(name)), "$name: ${encoded(message)}")
        }
    }

    @Test
    fun settingsMatchTheHost() {
        assertTrue(
            sameJson(encodeToJson(TermSettings.serializer(), TermSettings.default), Samples.message("settings_default")),
        )
        val custom = TermSettings.default.copy(
            cursorColor = TerminalColor.Rgb(Rgb(1u, 2u, 3u)),
            cursorText = TerminalColor.CellBackground,
            cursorBlink = false,
            cursorStyle = CursorStyle.BlockHollow,
            optionAsAlt = OptionAsAlt.Left,
            palette = listOf(
                TermSettings.PaletteEntry(index = 1u, color = Rgb(255u, 0u, 0u)),
                TermSettings.PaletteEntry(index = 200u, color = Rgb(0u, 0u, 255u)),
            ),
        )
        assertTrue(sameJson(encodeToJson(TermSettings.serializer(), custom), Samples.message("settings_custom")))
        assertEquals(custom, decode(TermSettings.serializer(), Samples.message("settings_custom")))
    }

    @Test
    fun integrationModeMatchesTheHost() {
        assertTrue(
            sameJson(
                encodeToJson(IntegrationMode.serializer(), IntegrationMode.Force(Shell.Zsh)),
                Samples.message("integration_force"),
            ),
        )
    }

    /**
     * 手机能发的消息种类就是这些。宿主对手机发的 `shutdown`、交接、`ui_reply`、`set_options`、`set_theme`
     * 只回错误，这里一个都不能有。给 [ClientMsg] 加种类时下面的 `when` 编不过，逼着回来对一遍这张表。
     */
    @Test
    fun mobileSendsOnlyAllowedKinds() {
        val samples = listOf(
            ClientMsg.Hello(4u, "b", ClientKind.Mobile, Caps(false, true, true), null, null),
            ClientMsg.ListSessions,
            ClientMsg.Layout(0u),
            ClientMsg.Open(1u, Placement.Tab, sampleId, "/tmp", false),
            ClientMsg.OpenWorkspace(1u, "/tmp", false),
            ClientMsg.RenameWorkspace(1u, 1u, 1u, "a"),
            ClientMsg.ListDirs(1u, null),
            ClientMsg.ListTasks(1u, "/tmp"),
            ClientMsg.FindCommands(1u, null, listOf("claude")),
            ClientMsg.Spawn(1u, sampleSize, null, IntegrationMode.Detect, true),
            ClientMsg.Attach(sampleId, null, AttachMode.VtReplay),
            ClientMsg.Detach(sampleId),
            ClientMsg.Resize(sampleId, sampleSize),
            ClientMsg.Focus(sampleId, true),
            ClientMsg.Kill(sampleId),
            ClientMsg.ReadScreen(sampleId, 3u),
            ClientMsg.SendKeys(1u, sampleId, listOf("enter")),
            ClientMsg.Paste(2u, sampleId, "y"),
            ClientMsg.Git(3u, sampleId, GitRequest.Status),
            ClientMsg.PushRegister(4u, "00", ApnsEnv.Production, "b", "m", "n"),
            ClientMsg.Unpair(5u),
            ClientMsg.KeepAwake,
        )
        fun covered(message: ClientMsg): Boolean = when (message) {
            is ClientMsg.Hello, ClientMsg.ListSessions, is ClientMsg.Layout, is ClientMsg.Open,
            is ClientMsg.OpenWorkspace, is ClientMsg.RenameWorkspace, is ClientMsg.ListDirs, is ClientMsg.ListTasks,
            is ClientMsg.FindCommands, is ClientMsg.Spawn, is ClientMsg.Attach, is ClientMsg.Detach,
            is ClientMsg.Resize, is ClientMsg.Focus, is ClientMsg.Kill, is ClientMsg.ReadScreen,
            is ClientMsg.SendKeys, is ClientMsg.Paste, is ClientMsg.Git, is ClientMsg.PushRegister,
            is ClientMsg.Unpair, ClientMsg.KeepAwake,
            -> true
        }
        val forbidden = setOf(
            "shutdown", "handoff", "handoff_ready", "handoff_abort", "handoff_done", "ui_reply", "set_options",
            "set_theme",
        )
        for (message in samples.filter(::covered)) {
            val type = encoded(message).jsonObject.getValue("type").jsonPrimitive.content
            assertFalse(type in forbidden, type)
        }
    }

    @Test
    fun helloGoesInAControlFrameOnChannelZero() {
        val frame = Frame.control(ClientMsg.ListSessions)
        assertEquals(FrameKind.Control, frame.kind)
        assertEquals(0u, frame.channel)
        assertEquals("""{"type":"list_sessions"}""", frame.payload.decodeToString())
    }
}

class HostMessageTests {
    private fun decode(name: String): HostMsg = decode(HostMsg.serializer(), Samples.message(name))

    private fun decodeText(json: String): HostMsg = decode(HostMsg.serializer(), json)

    /** 宿主发来的各条样例都认得，不会读成 `Unknown`：宿主改了消息的类型名时这里先发现。 */
    @Test
    fun knowsEveryHostMessage() {
        val names = listOf(
            "welcome", "incompatible", "session_list", "spawned", "attached", "snapshot_end", "resized",
            "theme_applied", "meta", "meta_shell", "command_finished", "resync", "exited", "exited_none", "bell",
            "opened", "done", "screen_text", "layout", "layout_changed", "dirs", "tasks", "commands", "git_status",
            "git_status_none", "git_diff", "git_branches", "error", "error_not_found", "error_backed_up",
            "error_no_session", "goodbye", "goodbye_handoff", "size_owner",
        )
        for (name in names) {
            val message = decode(name)
            if (message is HostMsg.Unknown) fail("$name decoded as unknown ${message.type}")
        }
    }

    @Test
    fun welcomeAndIncompatible() {
        assertEquals(HostMsg.Welcome(keepAwake = true, findsCommands = true), decode("welcome"))
        // 协议里 `keep_awake`、`shells` 可以缺省，缺了当作不认得。
        assertEquals(
            HostMsg.Welcome(keepAwake = false, findsCommands = false),
            decodeText("""{"type":"welcome","protocol":4}"""),
        )
        assertEquals(HostMsg.Incompatible(protocol = 5u, build = "x", reason = "old"), decode("incompatible"))
    }

    /** shell 报告的 PATH 第一份在 `Attached` 里，变了在 `Meta` 里带；没变的 `Meta` 不带。 */
    @Test
    fun shellReportsCarryThePath() {
        val attached = assertIs<HostMsg.Attached>(decode("attached"))
        assertEquals(ShellReport(path = "/usr/bin:/bin"), attached.shell)
        assertEquals(
            HostMsg.Meta(sampleId, SessionMeta(), ShellReport(path = "/Users/ethan/.local/bin:/usr/bin")),
            decode("meta_shell"),
        )
        assertEquals(HostMsg.Meta(sampleId, SessionMeta(), null), decode("meta"))
    }

    @Test
    fun commandsKeepTheirOrder() {
        assertEquals(HostMsg.Commands(req = 6u, found = listOf("claude.cmd", null)), decode("commands"))
    }

    @Test
    fun sessionListCarriesMeta() {
        val sessions = assertIs<HostMsg.SessionList>(decode("session_list")).sessions
        val session = sessions.first()
        assertEquals(sampleId, session.id)
        assertEquals(sampleSize, session.size)
        assertEquals("修 bug", session.meta.title)
        assertEquals("/Users/ethan/中文", session.meta.cwd)
        assertEquals(Agent(kind = AgentKind("claude"), state = AgentState.Blocked), session.meta.agent)
        assertEquals(1_790_000_000_000L, session.meta.agentSinceMs)
        assertEquals(2u, session.clients)
        assertTrue(session.claimed)
        assertEquals("Ethan 的 MacBook", session.sizeOwner)
    }

    @Test
    fun attachedCarriesTheTheme() {
        val attached = assertIs<HostMsg.Attached>(decode("attached"))
        assertEquals(7u, attached.channel)
        assertEquals(AttachMode.VtReplay, attached.mode)
        assertEquals(TermSettings.default, attached.settings)
        assertEquals("修 bug", attached.meta.displayTitle)
        assertEquals(1_790_000_000_000L, attached.meta.agentSinceMs)
    }

    @Test
    fun shellsAreTitledByTheirProgramName() {
        val fish = SessionMeta(title = "/p/t/proj", foregroundIsShell = true, foreground = "fish")
        assertEquals("fish", fish.displayTitle)
        val pwsh = SessionMeta(title = "C:\\src", foregroundIsShell = true, foreground = "pwsh.EXE")
        assertEquals("pwsh", pwsh.displayTitle)
        val vim = SessionMeta(title = "a.txt - VIM", foregroundIsShell = false, foreground = "vim")
        assertEquals("a.txt - VIM", vim.displayTitle)
    }

    @Test
    fun streamMarkers() {
        assertEquals(HostMsg.SnapshotEnd(sampleId), decode("snapshot_end"))
        assertEquals(HostMsg.Resized(sampleId, sampleSize), decode("resized"))
        val settings = assertIs<HostMsg.ThemeApplied>(decode("theme_applied")).settings
        assertEquals(CursorStyle.BlockHollow, settings.cursorStyle)
        assertEquals(TerminalColor.CellBackground, settings.cursorText)
        assertEquals(HostMsg.Resync(sampleId, "slow"), decode("resync"))
        assertEquals(HostMsg.Exited(sampleId, 1), decode("exited"))
        assertEquals(HostMsg.Exited(sampleId, null), decode("exited_none"))
        assertEquals(HostMsg.Bell(sampleId), decode("bell"))
        assertEquals(HostMsg.Meta(sampleId, SessionMeta()), decode("meta"))
        assertEquals(HostMsg.SizeOwner(sampleId, mine = false, owner = "Ethan 的 MacBook"), decode("size_owner"))
    }

    /** 布局按窗口、工作区、标签、分屏一层层读出来，带分屏在标签里的位置和浮着的标记；没有目录的工作区读成空。 */
    @Test
    fun layoutDescribesWorkspaces() {
        val layout = assertIs<HostMsg.Layout>(decode("layout"))
        assertEquals(0u, layout.req)
        val window = layout.windows.first()
        assertTrue(window.index == 1u && window.front)
        assertEquals(listOf("runode", "blog"), window.workspaces.map { it.name })
        assertEquals(listOf("/Users/ethan/dev/runode", null), window.workspaces.map { it.dir })
        val runode = window.workspaces[0]
        assertTrue(runode.active)
        assertEquals(listOf(false, true), runode.tabs.map { it.active })
        val a = SessionId("11111111111111111111111111111111")!!
        val b = SessionId("22222222222222222222222222222222")!!
        val c = SessionId("33333333333333333333333333333333")!!
        assertEquals(listOf(sampleId, a, b, c), runode.sessions)
        assertEquals(
            listOf(
                PaneRect(x = 0u, y = 0u, width = 500u, height = 1000u),
                PaneRect(x = 500u, y = 0u, width = 500u, height = 1000u),
                PaneRect(x = 200u, y = 150u, width = 600u, height = 650u),
            ),
            runode.tabs[1].panes.map { it.rect },
        )
        assertEquals(listOf(false, false, true), runode.tabs[1].panes.map { it.floating })
        // 开新标签挨着当前标签里有焦点的分屏。
        assertEquals(b, runode.anchor)
        assertNull(window.workspaces[1].anchor)
        assertEquals(HostMsg.LayoutChanged, decode("layout_changed"))
    }

    /** 命令按来源分组读出来；没有说明的读成空。 */
    @Test
    fun tasksAreGroupedBySource() {
        val tasks = assertIs<HostMsg.Tasks>(decode("tasks"))
        assertEquals(6u, tasks.req)
        assertEquals("/Users/ethan/dev/app/web", tasks.dir)
        assertEquals(
            listOf(TaskSource.Kind.Custom, TaskSource.Kind.Makefile, TaskSource.Kind.PackageJson),
            tasks.sources.map { it.kind },
        )
        val custom = tasks.sources[0]
        assertEquals("/Users/ethan/dev/app", custom.project)
        assertEquals(
            listOf(TaskEntry(name = "lint", command = "cargo clippy # slow", description = "cargo clippy # slow")),
            custom.tasks,
        )
        val sources = tasks.sources.drop(1)
        assertNull(sources[0].project)
        assertEquals(
            listOf(
                TaskEntry(name = "build", command = "make -C .. build", description = "编译全部"),
                TaskEntry(name = "test", command = "make -C .. test"),
            ),
            sources[0].tasks,
        )
        assertEquals("/Users/ethan/dev/app/Makefile", sources[0].file)
        assertTrue(!sources[0].truncated && sources[1].truncated)
        assertEquals(listOf(TaskEntry(name = "dev", command = "pnpm run dev", description = "vite")), sources[1].tasks)
    }

    @Test
    fun repliesAndGoodbyes() {
        assertEquals(HostMsg.Done(9u), decode("done"))
        assertEquals(HostMsg.Spawned(3u, sampleId), decode("spawned"))
        assertEquals(HostMsg.Opened(7u, sampleId), decode("opened"))
        assertEquals(
            HostMsg.Dirs(req = 5u, path = "/Users/ethan", dirs = listOf(".config", "dev", "中文"), truncated = true),
            decode("dirs"),
        )
        assertEquals(HostMsg.Error(req = null, id = sampleId, message = "no session"), decode("error"))
        // 宿主标了原因的带着原因，界面按它给出界面语言的说法。
        val notFound = assertIs<HostMsg.Error>(decode("error_not_found"))
        assertEquals(3u, notFound.req)
        assertNull(notFound.id)
        assertEquals(HostErrorReason(kind = "not_found", path = "/root/nope"), notFound.reason)
        // 宿主实际发的两句：措辞一改，重新生成样例后这里就对不上。
        val backedUp = assertIs<HostMsg.Error>(decode("error_backed_up"))
        val noSession = assertIs<HostMsg.Error>(decode("error_no_session"))
        assertTrue(backedUp.message.startsWith(HostMsg.backedUpPrefix))
        assertTrue(noSession.message.startsWith(HostMsg.noSessionPrefix))
        assertEquals(HostMsg.Goodbye(GoodbyeReason.Error("bye")), decode("goodbye"))
        assertEquals(HostMsg.Goodbye(GoodbyeReason.Handoff), decode("goodbye_handoff"))
        assertEquals(HostMsg.ScreenText(sampleId, "a\n", truncated = false), decode("screen_text"))
        assertEquals(HostMsg.CommandFinished(sampleId), decode("command_finished"))
    }

    /** 读屏带了 `req` 时写出去，回话里的 `req` 读回来；不带时不写这个键。 */
    @Test
    fun screenReadsCarryTheirReq() {
        assertEquals(7, encodeToJson(ClientMsg.serializer(), ClientMsg.ReadScreen(sampleId, 3u, req = 7u)).jsonObject["req"]?.jsonPrimitive?.int)
        assertFalse("req" in encodeToJson(ClientMsg.serializer(), ClientMsg.ReadScreen(sampleId, 3u)).jsonObject)
        assertEquals(
            HostMsg.ScreenText(sampleId, "a", truncated = false, req = 7u),
            decodeText("""{"type":"screen_text","id":"${sampleId.rawValue}","text":"a","req":7}"""),
        )
    }

    @Test
    fun unknownKindsAndValuesAreTolerated() {
        assertEquals(HostMsg.Unknown("from_the_future"), decodeText("""{"type":"from_the_future","x":1}"""))
        assertEquals(
            HostMsg.Goodbye(GoodbyeReason.Unknown("reboot")),
            decodeText("""{"type":"goodbye","reason":{"kind":"reboot"}}"""),
        )
        // 宿主已经不发的原因也读成 `Unknown`。
        assertEquals(
            HostMsg.Goodbye(GoodbyeReason.Unknown("idle")),
            decodeText("""{"type":"goodbye","reason":{"kind":"idle"}}"""),
        )
        // 认识的种类多了不认识的字段：忽略。
        assertEquals(
            HostMsg.Bell(sampleId),
            decodeText("""{"type":"bell","id":"0123456789abcdef0011223344556677","extra":true}"""),
        )
        // 不认识的命令来源读成 `Unknown`，缺的 `truncated` 读成假。
        val tasks = assertIs<HostMsg.Tasks>(
            decodeText(
                """{"type":"tasks","req":1,"dir":"/a","sources":[{"kind":"justfile","file":"/a/justfile","tasks":[]}]}""",
            ),
        )
        assertEquals(listOf(TaskSource.Kind.Unknown), tasks.sources.map { it.kind })
        assertFalse(tasks.sources[0].truncated)
        // 新的 agent 状态不让整条消息失败。
        val meta = decodeText(
            """{"type":"meta","id":"0123456789abcdef0011223344556677","meta":{"agent":{"kind":"newbot","state":"thinking"}}}""",
        )
        assertEquals(
            HostMsg.Meta(sampleId, SessionMeta(agent = Agent(AgentKind("newbot"), AgentState.Unknown("thinking")))),
            meta,
        )
    }

    @Test
    fun knownKindsMissingRequiredFieldsFail() {
        assertFails { decodeText("""{"type":"bell"}""") }
    }

    @Test
    fun sessionIdsAreValidated() {
        assertEquals("0123456789abcdef0011223344556677", SessionId("0123456789ABCDEF0011223344556677")?.rawValue)
        assertNull(SessionId("xyz"))
        assertFails { decodeText("""{"type":"bell","id":"short"}""") }
    }
}
