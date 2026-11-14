package dev.runode.mobile.features.model

import dev.runode.mobile.connection.HostEvent
import dev.runode.mobile.connection.MachineRecord
import dev.runode.mobile.connection.MemoryDeviceKeyStore
import dev.runode.mobile.connection.MemoryMachineStore
import dev.runode.mobile.protocol.AgentKind
import dev.runode.mobile.protocol.AttachMode
import dev.runode.mobile.protocol.HostMsg
import dev.runode.mobile.protocol.Rgb
import dev.runode.mobile.protocol.SessionMeta
import dev.runode.mobile.protocol.TermSettings
import java.io.File
import kotlin.math.abs
import kotlin.math.ceil
import kotlin.math.floor
import kotlin.math.min
import kotlin.math.pow
import kotlin.math.sign
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlin.time.Duration.Companion.milliseconds
import kotlin.time.TestTimeSource
import kotlinx.coroutines.delay
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import org.junit.Rule

class SettingsTests {
    @get:Rule val main = MainDispatcherRule()

    private val machine = machineRecord()
    private val link = FakeLink()
    private val store = DefaultsStore<AppPreferences>("preferences")

    private suspend fun app(): AppModel {
        val machines = MemoryMachineStore()
        machines.upsert(machine)
        val app = AppModel(
            AppDependencies(
                store = machines,
                keyStore = MemoryDeviceKeyStore(),
                pairing = FakePairing { machineRecord() },
                makeLink = { link },
                deviceName = "Pixel",
                preferences = store,
            ),
        )
        app.machineList.load()
        return app
    }

    @Test fun changesAreSavedRightAway() = runTest {
        val app = app()
        app.settings.preferences = app.settings.preferences.copy(defaultSize = SizePreference.FollowMachine)
        app.settings.preferences = app.settings.preferences.copy(bellHaptics = false)
        app.settings.setFontSize(40.0)
        assertEquals(SizePreference.FollowMachine, store.load()?.defaultSize)
        assertEquals(false, store.load()?.bellHaptics)
        // 超出范围的字号拉回范围里。
        assertEquals(AppPreferences.fontSizes.endInclusive, store.load()?.fontSize)
    }

    @Test fun oldArchivesKeepTheirDefaults() {
        val defaults = MemoryDefaults()
        defaults.setString("preferences", """{"fontSize":15}""")
        val decoded = assertNotNull(DefaultsStore<AppPreferences>("preferences", defaults).load())
        assertEquals(15.0, decoded.fontSize)
        assertEquals(SizePreference.Automatic, decoded.defaultSize)
        assertTrue(decoded.bellHaptics)
        assertNull(decoded.deviceName)
        assertTrue(decoded.alertsBlockedAgents)
    }

    @Test fun newTerminalsStartWithTheDefaultSize() = runTest {
        val app = app()
        app.settings.preferences = app.settings.preferences.copy(defaultSize = SizePreference.FitPhone)
        val terminal = assertNotNull(app.terminal(machine.id, sessionA))
        assertEquals(SizePreference.FitPhone, terminal.sizePreference)
        assertTrue(terminal.fitsPhone)
        terminal.close()
    }

    @Test fun renamingTheDeviceReachesEveryConnection() = runTest {
        val app = app()
        assertEquals("Pixel", app.settings.deviceName)
        app.settings.setDeviceName("  书房的 Pixel ")
        assertEquals("书房的 Pixel", app.settings.deviceName)
        assertTrue(eventually { link.deviceNames == listOf("书房的 Pixel") })
        // 清空或者填回系统的名字，就是不再自己起名字。
        app.settings.setDeviceName("Pixel")
        assertNull(assertNotNull(store.load()).deviceName)
        assertTrue(eventually { link.deviceNames == listOf("书房的 Pixel", "Pixel") })
        // 没变时不再转给连接。
        app.settings.setDeviceName("")
        delay(20)
        assertEquals(2, link.deviceNames.size)
    }

    @Test fun pairingFromSettingsWaitsForTheSheetToClose() = runTest {
        val app = app()
        app.showingSettings = true
        app.pairsAfterSettings = true
        app.showingSettings = false
        assertNull(app.pairing)
        app.settingsDismissed()
        assertNotNull(app.pairing)
        assertFalse(app.pairsAfterSettings)
    }
}

private val dark = TermSettings.default
private val light = TermSettings.default.copy(background = Rgb(0xFFFFFFu), foreground = Rgb(0x000000u))

/** 两台电脑，各自一条假连接；主题只记在内存里。 */
private class ThemeSetup(
    val first: MachineRecord,
    val second: MachineRecord,
    val themes: DefaultsStore<AppTheme>,
    val app: AppModel,
) {
    /** 这台电脑连上，回了一个会话只看状态的 Attach，带着 [settings] 这份主题。 */
    fun connect(machine: MachineRecord, settings: TermSettings) {
        val list = assertNotNull(app.sessionList(machine.id))
        list.handle(HostEvent.Ready(1u))
        list.handle(
            HostEvent.Message(
                HostMsg.Attached(sessionA, 0u, smallGrid, AttachMode.MetaOnly, SessionMeta(title = "zsh"), settings),
            ),
        )
    }
}

private suspend fun themeSetup(saved: AppTheme? = null): ThemeSetup {
    val first = machineRecord(name = "一", fingerprintByte = 1)
    val second = machineRecord(name = "二", fingerprintByte = 2).let { it.copy(pairedAt = it.pairedAt.plusSeconds(1)) }
    val themes = DefaultsStore<AppTheme>("theme")
    if (saved != null) themes.save(saved)
    val store = MemoryMachineStore()
    store.upsert(first)
    store.upsert(second)
    val links = mapOf(first.id to FakeLink(), second.id to FakeLink())
    val app = AppModel(
        AppDependencies(
            store = store,
            keyStore = MemoryDeviceKeyStore(),
            pairing = FakePairing { second },
            makeLink = { links.getValue(it.id) },
            deviceName = "测试手机",
            themes = themes,
        ),
    )
    app.machineList.load()
    return ThemeSetup(first, second, themes, app)
}

class ThemeTests {
    @get:Rule val main = MainDispatcherRule()

    @Test fun darkThemesLiftTheCardsAndLightThemesDimThePage() {
        val darkTheme = AppTheme(dark)
        assertTrue(darkTheme.isDark)
        assertEquals(dark.background, darkTheme.page)
        assertEquals(dark.background.mixed(dark.foreground, 0.05), darkTheme.card)
        val lightTheme = AppTheme(light)
        assertFalse(lightTheme.isDark)
        assertEquals(light.background, lightTheme.card)
        assertEquals(Rgb(0xF2F2F2u), lightTheme.page)
    }

    /** 深色下由暗到亮是底槽、页面、卡片、凸起的块、细线、粗一点的线；浅色下卡片最亮。 */
    @Test fun surfacesStayOrdered() {
        fun luminance(rgb: Rgb) = 0.2126 * rgb.r.toDouble() + 0.7152 * rgb.g.toDouble() + 0.0722 * rgb.b.toDouble()
        val darkTheme = AppTheme(dark)
        val ordered = listOf(
            darkTheme.well, darkTheme.page, darkTheme.card, darkTheme.raised, darkTheme.hairline, darkTheme.strongHairline,
        ).map(::luminance)
        assertEquals(ordered.sorted(), ordered)
        assertEquals(ordered.size, ordered.toSet().size)
        val lightTheme = AppTheme(light)
        val others = listOf(
            lightTheme.page, lightTheme.raised, lightTheme.well, lightTheme.hairline, lightTheme.strongHairline,
            lightTheme.stackEdge,
        )
        assertTrue(others.all { luminance(it) < luminance(lightTheme.card) })
    }

    @Test fun statusPaletteFollowsDarkness() {
        assertEquals(StatusPalette.dark, AppTheme(dark).status)
        assertEquals(StatusPalette.light, AppTheme(light).status)
        assertNotEquals(StatusPalette.dark, StatusPalette.light)
    }

    @Test fun isDarkUntilAMachineReportsATheme() = runTest {
        val setup = themeSetup()
        assertEquals(AppTheme(TermSettings.default), setup.app.theme)
        assertTrue(setup.app.theme.isDark)
        assertNull(setup.themes.load())
    }

    @Test fun usesTheSavedThemeBeforeConnecting() = runTest {
        val setup = themeSetup(saved = AppTheme(light))
        assertEquals(AppTheme(light), setup.app.theme)
    }

    @Test fun takesTheThemeFromTheFirstMachineAndSavesIt() = runTest {
        val setup = themeSetup(saved = AppTheme(light))
        setup.connect(setup.second, light)
        setup.connect(setup.first, dark)
        assertEquals(AppTheme(dark), setup.app.theme)
        assertEquals(AppTheme(dark), setup.themes.load())
    }

    @Test fun prefersTheMachineOfTheLastOpenedTerminal() = runTest {
        val setup = themeSetup()
        setup.connect(setup.first, dark)
        setup.connect(setup.second, light)
        setup.app.path = listOf(Route.Machine(setup.second.id), Route.Terminal(setup.second.id, sessionA))
        assertEquals(AppTheme(light), setup.app.theme)
        assertEquals(AppTheme(light), setup.themes.load())
        setup.app.path = emptyList()
    }

    @Test fun followsThemeChangesOnTheMachine() = runTest {
        val setup = themeSetup()
        setup.connect(setup.first, dark)
        val list = assertNotNull(setup.app.sessionList(setup.first.id))
        list.handle(HostEvent.Message(HostMsg.ThemeApplied(sessionA, light)))
        assertEquals(AppTheme(light), setup.app.theme)
        assertEquals(AppTheme(light), setup.themes.load())
    }

    @Test fun defaultsKeepTheTheme() {
        val defaults = MemoryDefaults()
        assertNull(DefaultsStore<AppTheme>("theme", defaults).load())
        DefaultsStore<AppTheme>("theme", defaults).save(AppTheme(light))
        assertEquals(AppTheme(light), DefaultsStore<AppTheme>("theme", defaults).load())
    }
}

class AgentLauncherTests {
    @Serializable
    private class Root(val launchers: List<Launcher>) {
        @Serializable
        class Launcher(val label: String, val programs: List<String>, val kind: String)
    }

    /** 菜单里的各家和宿主那边一样：取自 `runode_agent` 的测试 `launcher_fixture` 生成的样例。 */
    @Test fun matchesTheHostLaunchers() {
        val fixture = File(System.getProperty("runode.repo"), "crates/agent/tests/fixtures/launchers.json")
        val launchers = Json { ignoreUnknownKeys = true }.decodeFromString<Root>(fixture.readText()).launchers
        assertTrue(launchers.isNotEmpty())
        assertEquals(launchers.map { it.label }, AgentLauncher.all.map { it.label })
        assertEquals(launchers.map { it.programs }, AgentLauncher.all.map { it.programs })
        assertEquals(launchers.map { it.kind }, AgentLauncher.all.map { it.kind.label })
    }

    /** 每家列一个：先试的程序找不到时换同一家的下一个；起它时打宿主找到的文件名。 */
    @Test fun listsTheFirstProgramFoundForEachAgent() {
        val programs = AgentLauncher.programs
        assertEquals(programs.size, programs.toSet().size)
        val found = MutableList<String?>(programs.size) { null }
        found[programs.indexOf("codex")] = "codex.cmd"
        found[programs.indexOf("opencode")] = "opencode"
        assertEquals(
            listOf(
                InstalledAgent("Codex", AgentKind("codex"), "codex.cmd"),
                InstalledAgent("OpenCode", AgentKind("open_code"), "opencode"),
            ),
            AgentLauncher.installed(found),
        )
        found[programs.indexOf("opencode2")] = "opencode2"
        assertEquals(listOf("codex.cmd", "opencode2"), AgentLauncher.installed(found).map { it.command })
        assertTrue(AgentLauncher.installed(emptyList()).isEmpty())
    }
}

/**
 * Claude Code 2.1 在 runode 里处理一个滚轮事件的那段（base 为 1、开着加速、不是 wheelFlood、不走 decay），照它的
 * JS 逐行抄的，含 [ClaudeWheelPacer] 要躲开的 wheelMode。返回这个事件滚几行。
 */
private class ClaudeReference {
    var time = -10_000.0
    var mult = 1.0
    var dir = 0
    var pendingFlip = false
    var wheelMode = false
    var enteredWheelMode = false
    var burstCount = 0

    fun wheel(k: Int, m: Double): Int {
        if (wheelMode && m - time > 1500) {
            wheelMode = false
            burstCount = 0
            mult = 1.0
        }
        if (pendingFlip) {
            pendingFlip = false
            if (k != dir || m - time > 200) {
                dir = k
                time = m
                mult = 1.0
                return 1
            }
            wheelMode = true
            enteredWheelMode = true
        }
        val pe = m - time
        if (k != dir && dir != 0) {
            pendingFlip = true
            time = m
            return 0
        }
        dir = k
        time = m
        if (wheelMode) {
            if (pe < 5) {
                burstCount += 1
                if (burstCount >= 5) {
                    wheelMode = false
                    burstCount = 0
                    mult = 1.0
                } else {
                    return 1
                }
            } else {
                burstCount = 0
            }
        }
        if (wheelMode) {
            val decay = 0.5.pow(pe / 150)
            mult = min(min(15.0, 1 + (mult - 1) * decay + 15 * decay), mult + 3)
            return maxOf(1, floor(mult).toInt())
        }
        mult = if (pe > 40) 1.0 else min(6.0, mult + 0.3)
        return maxOf(1, floor(mult).toInt())
    }
}

class ClaudeWheelPacerTests {
    /**
     * 拖动（每 8 毫秒报一次）：慢慢往上、快速往上、来回抖、再往下甩。Claude Code 最后滚的总行数和手指要的一样，中途也
     * 从不进 wheelMode。
     */
    @Test fun claudeScrollsWhatTheFingerAskedFor() {
        val clock = TestTimeSource()
        val start = clock.markNow()
        val pacer = ClaudeWheelPacer()
        val claude = ClaudeReference()
        var asked = 0
        var scrolled = 0
        var retryAt: Int? = null
        for (ms in 0 until 3000) {
            var due = retryAt?.let { ms >= it } ?: false
            if (ms % 8 == 0 && ms < 2000) {
                val step = ms / 8
                val lines = when {
                    ms < 400 -> if (step % 4 == 0) -1 else 0
                    ms < 900 -> -3
                    ms < 1200 -> if (step % 3 == 0) (if (step % 2 == 0) 1 else -1) else 0
                    else -> 2
                }
                pacer.add(lines)
                asked += lines
                due = true
            }
            if (!due) continue
            val (events, retry) = pacer.take(start + ms.milliseconds)
            repeat(abs(events)) {
                scrolled += events.sign * claude.wheel(events.sign, ms.toDouble())
            }
            retryAt = retry?.let { ms + ceil(it / 1.milliseconds).toInt() }
        }
        assertNull(retryAt)
        assertEquals(asked, scrolled)
        assertFalse(claude.enteredWheelMode)
    }
}
