package dev.runode.mobile.features.ui

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import dev.runode.mobile.connection.HostEvent
import dev.runode.mobile.connection.LinkState
import dev.runode.mobile.connection.MemoryDeviceKeyStore
import dev.runode.mobile.connection.MemoryMachineStore
import dev.runode.mobile.connection.Pairing
import dev.runode.mobile.features.R
import dev.runode.mobile.features.RunodeApp
import dev.runode.mobile.features.model.AppDependencies
import dev.runode.mobile.features.model.AppModel
import dev.runode.mobile.features.model.FakeLink
import dev.runode.mobile.features.model.FakePairing
import dev.runode.mobile.features.model.GatedPairing
import dev.runode.mobile.features.model.PairingModel
import dev.runode.mobile.features.model.Route
import dev.runode.mobile.features.model.UiText
import dev.runode.mobile.features.model.machineRecord
import dev.runode.mobile.features.model.sessionA
import dev.runode.mobile.features.model.sessionB
import dev.runode.mobile.features.model.smallGrid
import dev.runode.mobile.protocol.Agent
import dev.runode.mobile.protocol.AgentKind
import dev.runode.mobile.protocol.AgentState
import dev.runode.mobile.protocol.ClientMsg
import dev.runode.mobile.protocol.HostMsg
import dev.runode.mobile.protocol.SessionInfo
import dev.runode.mobile.protocol.SessionMeta
import java.time.Instant
import java.util.Base64
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.runBlocking
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** 视图模型只在主线程上用。 */
internal fun <T> onMain(block: suspend CoroutineScope.() -> T): T = runBlocking(Dispatchers.Main, block)

internal const val TIMEOUT_MS = 5_000L

/** 一台假电脑上的会话：等你回答的 agent（[sessionA]）和普通的 shell（[sessionB]）。 */
private val waiting = SessionInfo(
    sessionA,
    smallGrid,
    SessionMeta(title = "Fix the build", agent = Agent(AgentKind("claude"), AgentState.Blocked), cwd = "/Users/ethan/dev"),
)
private val shell = SessionInfo(sessionB, smallGrid, SessionMeta(fallbackTitle = "zsh", cwd = "/Users/ethan/dev", foregroundIsShell = true))

/** 等你回答的会话屏幕上的提问。 */
private const val question = " Bash command\n\n   rm -rf build\n   Remove the build directory\n\n Do you want to proceed?\n" +
    " ❯ 1. Yes\n   2. Yes, and don't ask again for rm commands\n   3. No, and tell Claude what to do differently (esc)\n"

/** 首页、会话列表、终端页和配对页接上假连接、假配对后的样子和操作。 */
@RunWith(AndroidJUnit4::class)
class AppScreensTests {
    @get:Rule val compose = createComposeRule()

    private val context = InstrumentationRegistry.getInstrumentation().targetContext
    private val machine = machineRecord(name = "Studio")
    private val link = FakeLink()

    private fun app(pairing: Pairing = FakePairing { machineRecord(name = "Other", fingerprintByte = 2) }): AppModel = onMain {
        val app = AppModel(AppDependencies(MemoryMachineStore(listOf(machine)), MemoryDeviceKeyStore(), pairing, { link }, "Test Phone"))
        app.machineList.load()
        app
    }

    /** 电脑连上，列出两个会话，读回等回答的那个会话的屏幕。 */
    private fun connect(app: AppModel) = onMain {
        val list = assertNotNull(app.sessionList(machine.id))
        list.handle(HostEvent.State(LinkState.Connected("Studio", "192.168.1.20")))
        list.handle(HostEvent.Ready(1u))
        list.handle(HostEvent.Message(HostMsg.SessionList(listOf(waiting, shell))))
        list.handle(HostEvent.Message(HostMsg.ScreenText(sessionA, question, truncated = false)))
        assertNotNull(list.prompts[sessionA])
    }

    private fun pairingLink(): String {
        val encoder = Base64.getUrlEncoder().withoutPadding()
        val fingerprint = encoder.encodeToString(ByteArray(32) { 9 })
        val secret = encoder.encodeToString(ByteArray(32) { 3 })
        val expires = Instant.now().plusSeconds(3600).epochSecond
        return "runode://pair?v=1&name=Studio&fp=$fingerprint&secret=$secret&port=7866&addr=192.168.1.30&exp=$expires"
    }

    @Test fun homeShowsTheWaitingCardWithItsOptions() {
        val app = app()
        connect(app)
        compose.setContent { RunodeApp(app) }
        compose.onNodeWithText("Fix the build").assertIsDisplayed()
        compose.onNodeWithText("Yes, and don't ask again for rm commands").assertIsDisplayed()
        compose.onNodeWithContentDescription(context.getString(R.string.draft_placeholder)).assertExists()
    }

    @Test fun tappingAnOptionSendsItsNumber() {
        val app = app()
        connect(app)
        compose.setContent { RunodeApp(app) }
        val key = onMain { assertNotNull(app.sessionList(machine.id)?.prompts?.get(sessionA)).options.first().key.key }
        link.clearSent()
        compose.onNodeWithText("Yes").performClick()
        compose.waitUntil(TIMEOUT_MS) {
            link.sent.any { it is ClientMsg.SendKeys && it.id == sessionA && it.keys == listOf(key) }
        }
    }

    @Test fun aSessionInTheListOpensItsTerminal() {
        val app = app()
        connect(app)
        onMain { app.path = listOf(Route.Machine(machine.id)) }
        compose.setContent { RunodeApp(app) }
        compose.onNodeWithText("zsh").performClick()
        compose.waitForIdle()
        assertEquals(Route.Terminal(machine.id, sessionB), onMain { app.path.last() })
        compose.onNodeWithContentDescription(context.getString(R.string.terminal_options)).assertIsDisplayed()
    }

    @Test fun aPastedLinkStartsPairingAndTheResultStaysOnScreen() {
        val gate = Channel<Unit>(Channel.UNLIMITED)
        val pairing = GatedPairing(gate)
        val app = app(pairing)
        onMain {
            app.startPairing()
            // 测试里不开相机。
            assertNotNull(app.pairing).scannerUnavailable = UiText.Raw("No camera")
        }
        compose.setContent { RunodeApp(app) }
        compose.onNodeWithText(context.getString(R.string.pairing_paste_link)).performClick()
        compose.onNodeWithContentDescription(context.getString(R.string.pairing_link_field)).performTextInput(pairingLink())
        compose.onNodeWithText(context.getString(R.string.pairing_pair)).performClick()
        compose.waitUntil(TIMEOUT_MS) { onMain { app.pairing?.phase is PairingModel.Phase.Pairing } }
        compose.onNodeWithText(context.getString(R.string.pairing_in_progress, "Studio")).assertIsDisplayed()

        gate.trySend(Unit)
        compose.waitUntil(TIMEOUT_MS) { onMain { app.pairing?.phase is PairingModel.Phase.Paired } }
        compose.onNodeWithText(context.getString(R.string.pairing_done_detail, pairing.machine.name)).assertIsDisplayed()
        // 配好了停在这一页，由用户决定去看会话还是再配一台。
        assertNotNull(onMain { app.pairing })
        compose.onNodeWithText(context.getString(R.string.pairing_view_sessions)).performClick()
        compose.waitForIdle()
        onMain {
            assertNull(app.pairing)
            assertEquals(listOf<Route>(Route.Machine(pairing.machine.id)), app.path)
        }
        assertTrue(onMain { app.machineList.machine(pairing.machine.id) != null })
    }

    @Test fun theFailedPairingOffersToScanAgain() {
        val app = app(FakePairing { throw dev.runode.mobile.connection.LinkFailure.ConnectionFailed("unreachable") })
        onMain {
            app.startPairing()
            assertNotNull(app.pairing).scannerUnavailable = UiText.Raw("No camera")
        }
        compose.setContent { RunodeApp(app) }
        onMain { assertNotNull(app.pairing).linkText = pairingLink() }
        onMain { assertNotNull(app.pairing).submitLink() }
        compose.waitUntil(TIMEOUT_MS) { onMain { app.pairing?.phase is PairingModel.Phase.Failed } }
        compose.onNodeWithText(context.getString(R.string.pairing_scan_again)).performClick()
        compose.waitForIdle()
        assertIs<PairingModel.Phase.Idle>(onMain { assertNotNull(app.pairing).phase })
    }
}
