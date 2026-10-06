import Foundation
import RunodeConnection
import RunodeProtocol
import Testing

@testable import RunodeFeatures

private let dark = TermSettings.default
private let light: TermSettings = {
    var settings = TermSettings.default
    settings.background = Rgb(hex: 0xFFFFFF)
    settings.foreground = Rgb(hex: 0x000000)
    return settings
}()

/// 两台电脑，各自一条假连接；主题记在内存里。
@MainActor
private struct Setup {
    let first = machineRecord(name: "一", fingerprintByte: 1)
    let second: MachineRecord = {
        var record = machineRecord(name: "二", fingerprintByte: 2)
        record.pairedAt += 1
        return record
    }()
    let themes: MemoryThemeStore
    let recents = MemoryRecentTerminalStore()
    let app: AppModel

    init(saved: AppTheme? = nil) async {
        themes = MemoryThemeStore(saved)
        let store = InMemoryMachineStore()
        await store.upsert(first)
        await store.upsert(second)
        let links = [first.id: FakeLink(), second.id: FakeLink()]
        let paired = second
        app = AppModel(
            dependencies: AppDependencies(
                store: store, keyStore: InMemoryKeyStore(), pairing: FakePairing { _ in paired },
                makeLink: { links[$0.id]! }, deviceName: "测试 iPhone", recents: recents, themes: themes))
        await app.machineList.load()
    }

    /// 这台电脑连上，回了一个会话只看状态的 `Attach`，带着 `settings` 这份主题。
    func connect(_ machine: MachineRecord, settings: TermSettings) throws {
        let list = try #require(app.sessionList(for: machine.id))
        list.handle(.ready(generation: 1))
        list.handle(
            .message(
                .attached(
                    .init(
                        id: sessionA, channel: 0, size: smallGrid, mode: .metaOnly, meta: SessionMeta(title: "zsh"),
                        settings: settings))))
    }
}

@MainActor
@Suite struct ThemeTests {
    @Test func darkThemesLiftTheCardsAndLightThemesDimThePage() {
        let darkTheme = AppTheme(dark)
        #expect(darkTheme.isDark)
        #expect(darkTheme.page == dark.background)
        #expect(darkTheme.card == dark.background.mixed(with: dark.foreground, by: 0.08))
        let lightTheme = AppTheme(light)
        #expect(!lightTheme.isDark)
        #expect(lightTheme.card == light.background)
        #expect(lightTheme.page == Rgb(hex: 0xF2F2F2))
    }

    @Test func isDarkUntilAMachineReportsATheme() async {
        let setup = await Setup()
        #expect(setup.app.theme == AppTheme(.default))
        #expect(setup.app.theme.isDark)
        #expect(setup.themes.load() == nil)
    }

    @Test func usesTheSavedThemeBeforeConnecting() async {
        let setup = await Setup(saved: AppTheme(light))
        #expect(setup.app.theme == AppTheme(light))
    }

    @Test func takesTheThemeFromTheFirstMachineAndSavesIt() async throws {
        let setup = await Setup(saved: AppTheme(light))
        try setup.connect(setup.second, settings: light)
        try setup.connect(setup.first, settings: dark)
        #expect(setup.app.theme == AppTheme(dark))
        #expect(setup.themes.load() == AppTheme(dark))
    }

    @Test func prefersTheMachineOfTheLastOpenedTerminal() async throws {
        let setup = await Setup()
        try setup.connect(setup.first, settings: dark)
        try setup.connect(setup.second, settings: light)
        setup.app.path = [.machine(setup.second.id), .terminal(machine: setup.second.id, session: sessionA)]
        #expect(setup.app.theme == AppTheme(light))
        #expect(setup.themes.load() == AppTheme(light))
    }

    @Test func followsThemeChangesOnTheMachine() async throws {
        let setup = await Setup()
        try setup.connect(setup.first, settings: dark)
        let list = try #require(setup.app.sessionList(for: setup.first.id))
        list.handle(.message(.themeApplied(id: sessionA, settings: light)))
        #expect(setup.app.theme == AppTheme(light))
        #expect(setup.themes.load() == AppTheme(light))
    }

    @Test func userDefaultsKeepsTheTheme() throws {
        let defaults = try #require(UserDefaults(suiteName: "ThemeTests"))
        defaults.removePersistentDomain(forName: "ThemeTests")
        #expect(UserDefaultsThemeStore(defaults: defaults).load() == nil)
        UserDefaultsThemeStore(defaults: defaults).save(AppTheme(light))
        #expect(UserDefaultsThemeStore(defaults: defaults).load() == AppTheme(light))
    }
}
