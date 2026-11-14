package dev.runode.mobile.features.ui

import android.content.Context
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.click
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithContentDescription
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTouchInput
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import dev.runode.mobile.connection.HostEvent
import dev.runode.mobile.connection.LinkState
import dev.runode.mobile.connection.MemoryDeviceKeyStore
import dev.runode.mobile.connection.MemoryMachineStore
import dev.runode.mobile.features.R
import dev.runode.mobile.features.RunodeApp
import dev.runode.mobile.features.model.AppDependencies
import dev.runode.mobile.features.model.AppModel
import dev.runode.mobile.features.model.AppPreferences
import dev.runode.mobile.features.model.DefaultsStore
import dev.runode.mobile.features.model.FakeLink
import dev.runode.mobile.features.model.FakePairing
import dev.runode.mobile.features.model.FakeScreenTransport
import dev.runode.mobile.features.model.Route
import dev.runode.mobile.features.model.ScreenModel
import dev.runode.mobile.features.model.fakeScreenOpener
import dev.runode.mobile.features.model.machineRecord
import dev.runode.mobile.features.model.sessionB
import dev.runode.mobile.features.model.smallGrid
import dev.runode.mobile.features.ui.screen.RemoteDesktopPage
import dev.runode.mobile.protocol.ClientMsg
import dev.runode.mobile.protocol.GitFile
import dev.runode.mobile.protocol.GitFileDiff
import dev.runode.mobile.protocol.GitFileStatus
import dev.runode.mobile.protocol.GitHunk
import dev.runode.mobile.protocol.GitLine
import dev.runode.mobile.protocol.GitLineKind
import dev.runode.mobile.protocol.GitRequest
import dev.runode.mobile.protocol.GitStatus
import dev.runode.mobile.protocol.HostMsg
import dev.runode.mobile.protocol.PaneLayout
import dev.runode.mobile.protocol.SessionInfo
import dev.runode.mobile.protocol.SessionMeta
import dev.runode.mobile.protocol.TabLayout
import dev.runode.mobile.protocol.WindowLayout
import dev.runode.mobile.protocol.WorkspaceLayout
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertTrue
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Git 页、设置页、选目录和远程桌面接上假连接后的操作。 */
@RunWith(AndroidJUnit4::class)
class PageTests {
    @get:Rule val compose = createComposeRule()

    private val context = InstrumentationRegistry.getInstrumentation().targetContext
    private val machine = machineRecord(name = "Studio")
    private val link = FakeLink()
    private val preferences = DefaultsStore<AppPreferences>("preferences")
    private val shell = SessionInfo(sessionB, smallGrid, SessionMeta(fallbackTitle = "zsh", cwd = "/Users/ethan/dev", foregroundIsShell = true))

    private fun app(): AppModel = onMain {
        val app = AppModel(
            AppDependencies(
                MemoryMachineStore(listOf(machine)),
                MemoryDeviceKeyStore(),
                FakePairing { machineRecord(name = "Other", fingerprintByte = 2) },
                { link },
                "Test Phone",
                preferences = preferences,
            ),
        )
        app.machineList.load()
        app
    }

    /** 电脑连上，列出普通 shell，电脑上的 app 开着一个窗口。 */
    private fun connect(app: AppModel) = onMain {
        val list = assertNotNull(app.sessionList(machine.id))
        list.handle(HostEvent.State(LinkState.Connected("Studio", "192.168.1.20")))
        list.handle(HostEvent.Ready(1u))
        list.handle(HostEvent.Message(HostMsg.SessionList(listOf(shell))))
        val layout = assertNotNull(link.sent.filterIsInstance<ClientMsg.Layout>().lastOrNull())
        val pane = PaneLayout(1u, sessionB)
        val workspace = WorkspaceLayout(1u, name = "dev", dir = "/Users/ethan/dev", active = true, tabs = listOf(TabLayout(1u, true, listOf(pane))))
        list.handle(HostEvent.Message(HostMsg.Layout(layout.req, listOf(WindowLayout(1u, front = true, workspaces = listOf(workspace))))))
        assertTrue(list.hasDesktopWindow)
    }

    private fun lastGit(): ClientMsg.Git = assertNotNull(link.sent.filterIsInstance<ClientMsg.Git>().lastOrNull())

    @Test fun theGitPageShowsATreeAndATappedFileShowsItsDiff() {
        context.getSharedPreferences("git", Context.MODE_PRIVATE).edit().clear().commit()
        val app = app()
        connect(app)
        val server = GitFile(path = "crates/host/src/server.rs", status = GitFileStatus.Modified, added = 1u, removed = 1u)
        val notes = GitFile(path = "docs/old-notes.md", status = GitFileStatus.Deleted, removed = 41u)
        onMain {
            val git = assertNotNull(app.git(machine.id, sessionB))
            git.handle(HostEvent.State(LinkState.Connected("Studio", null)))
            git.handle(HostEvent.Ready(1u))
            val status = GitStatus(root = "/Users/ethan/dev", branch = "main", unstaged = listOf(server, notes))
            git.handle(HostEvent.Message(HostMsg.GitStatus(lastGit().req, sessionB, status)))
            app.path = listOf(Route.Machine(machine.id), Route.Git(machine.id, sessionB))
        }
        compose.setContent { RunodeApp(app) }
        compose.onNodeWithContentDescription(context.getString(R.string.git_actions)).performClick()
        compose.onNodeWithText(context.getString(R.string.git_view_tree)).performClick()
        compose.waitUntil(TIMEOUT_MS) {
            compose.onAllNodesWithContentDescription("Folder", substring = true).fetchSemanticsNodes().isNotEmpty()
        }

        link.clearSent()
        compose.onNodeWithContentDescription(server.path, substring = true).performClick()
        compose.waitUntil(TIMEOUT_MS) { link.sent.any { it is ClientMsg.Git && it.request == GitRequest.Diff(server.path, false) } }
        onMain {
            val git = assertNotNull(app.git(machine.id, sessionB))
            val lines = listOf(
                GitLine(GitLineKind.Removed, old = 10u, text = "let port = 7866;"),
                GitLine(GitLineKind.Added, new = 10u, text = "let port = 7867;"),
            )
            val diff = GitFileDiff(server, listOf(GitHunk("@@ -10 +10 @@ fn serve", lines)))
            git.handle(HostEvent.Message(HostMsg.GitDiff(lastGit().req, sessionB, diff)))
        }
        compose.waitUntil(TIMEOUT_MS) {
            compose.onAllNodesWithContentDescription("let port = 7867;", substring = true).fetchSemanticsNodes().isNotEmpty()
        }
    }

    @Test fun aSettingsSwitchIsSaved() {
        val app = app()
        val before = onMain { app.settings.preferences.bellHaptics }
        onMain { app.showingSettings = true }
        compose.setContent { RunodeApp(app) }
        compose.onNodeWithText(context.getString(R.string.settings_bell)).performClick()
        compose.waitForIdle()
        assertEquals(!before, onMain { app.settings.preferences.bellHaptics })
        assertEquals(!before, preferences.load()?.bellHaptics)
    }

    @Test fun deletingAComputerAsksFirst() {
        val app = app()
        onMain { app.showingSettings = true }
        compose.setContent { RunodeApp(app) }
        compose.onNodeWithText("Studio").performClick()
        compose.onNodeWithText(context.getString(R.string.machine_delete)).performClick()
        compose.onNodeWithText(context.getString(R.string.home_delete_title)).assertIsDisplayed()
        assertNotNull(onMain { app.machineList.machine(machine.id) })
        compose.onNodeWithText(context.getString(R.string.action_delete)).performClick()
        compose.waitUntil(TIMEOUT_MS) { onMain { app.machineList.machines.none { it.id == machine.id } } }
    }

    @Test fun choosingAFolderOpensAWorkspaceThere() {
        val app = app()
        connect(app)
        onMain { app.path = listOf(Route.Machine(machine.id)) }
        compose.setContent { RunodeApp(app) }
        compose.onNodeWithContentDescription(context.getString(R.string.sessions_new)).performClick()
        link.clearSent()
        compose.onNodeWithText(context.getString(R.string.new_workspace)).performClick()
        val list = onMain { assertNotNull(app.sessionList(machine.id)) }
        fun answer(path: String, dirs: List<String>) {
            compose.waitUntil(TIMEOUT_MS) { link.sent.any { it is ClientMsg.ListDirs } }
            val request = link.sent.filterIsInstance<ClientMsg.ListDirs>().last()
            link.clearSent()
            onMain { list.handle(HostEvent.Message(HostMsg.Dirs(request.req, path, dirs, truncated = false))) }
        }
        answer("/Users/ethan", listOf("Documents", "dev"))
        compose.onNodeWithText("dev").performClick()
        answer("/Users/ethan/dev", listOf("runode"))
        compose.waitUntil(TIMEOUT_MS) { onMain { list.directoryPicker?.path } == "/Users/ethan/dev" }
        compose.onNodeWithText(context.getString(R.string.picker_create)).performClick()
        compose.waitUntil(TIMEOUT_MS) { link.sent.any { it is ClientMsg.OpenWorkspace } }
        val open = assertIs<ClientMsg.OpenWorkspace>(link.sent.first { it is ClientMsg.OpenWorkspace })
        assertEquals("/Users/ethan/dev", open.dir)
    }

    @Test fun tappingTheRemoteScreenClicksThere() {
        val transport = FakeScreenTransport(machine.deviceId)
        val model = onMain { ScreenModel(machine, fakeScreenOpener(transport)) }
        compose.setContent { RemoteDesktopPage(model) {} }
        compose.waitUntil(TIMEOUT_MS) { onMain { model.phase } != ScreenModel.Phase.Connecting }
        assertEquals<ScreenModel.Phase>(ScreenModel.Phase.Live, onMain { model.phase })
        compose.onNodeWithContentDescription(context.getString(R.string.screen_computer_screen)).performTouchInput { click(center) }
        compose.waitUntil(TIMEOUT_MS) { transport.inputs("mouse_button").size >= 2 }
        val (down, up) = transport.inputs("mouse_button")
        assertEquals("left", down["button"]?.jsonPrimitive?.content)
        assertEquals("true", down["down"]?.jsonPrimitive?.content)
        assertEquals("false", up["down"]?.jsonPrimitive?.content)
        onMain { model.close() }
    }
}
