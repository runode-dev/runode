import Foundation
import RunodeConnection
import RunodeProtocol
import Testing

@testable import RunodeFeatures

@MainActor
@Suite struct SettingsTests {
    let machine = machineRecord()
    let link = FakeLink()
    let store = DefaultsStore<AppPreferences>("preferences", defaults: .temporary())

    func app() async -> AppModel {
        let machines = MemoryMachineStore()
        await machines.upsert(machine)
        let link = self.link
        let app = AppModel(
            dependencies: AppDependencies(
                store: machines, keyStore: MemoryDeviceKeyStore(), pairing: FakePairing { _ in machineRecord() },
                makeLink: { _ in link }, deviceName: "iPhone", preferences: store))
        await app.machineList.load()
        return app
    }

    @Test func changesAreSavedRightAway() async {
        let app = await app()
        app.settings.preferences.defaultSize = .followMachine
        app.settings.preferences.bellHaptics = false
        app.settings.setFontSize(40)
        #expect(store.load()?.defaultSize == .followMachine)
        #expect(store.load()?.bellHaptics == false)
        // 超出范围的字号拉回范围里。
        #expect(store.load()?.fontSize == AppPreferences.fontSizes.upperBound)
    }

    @Test func oldArchivesKeepTheirDefaults() throws {
        let decoded = try JSONDecoder().decode(AppPreferences.self, from: Data(#"{"fontSize":15}"#.utf8))
        #expect(decoded.fontSize == 15)
        #expect(decoded.defaultSize == .automatic)
        #expect(decoded.bellHaptics)
        #expect(decoded.deviceName == nil)
        #expect(decoded.alertsBlockedAgents)
    }

    /// 以前叫 `showsAgentActivity` 的开关：关过的照旧关着，新键在时以新键为准；写回去只有新键。
    @Test func theOldActivitySwitchCarriesOver() throws {
        let off = try JSONDecoder().decode(AppPreferences.self, from: Data(#"{"showsAgentActivity":false}"#.utf8))
        #expect(!off.alertsBlockedAgents)
        let on = try JSONDecoder().decode(AppPreferences.self, from: Data(#"{"showsAgentActivity":true}"#.utf8))
        #expect(on.alertsBlockedAgents)
        let both = try JSONDecoder().decode(
            AppPreferences.self, from: Data(#"{"showsAgentActivity":false,"alertsBlockedAgents":true}"#.utf8))
        #expect(both.alertsBlockedAgents)
        let written = try JSONSerialization.jsonObject(with: JSONEncoder().encode(off)) as? [String: Any]
        #expect(written?["alertsBlockedAgents"] as? Bool == false)
        #expect(written?["showsAgentActivity"] == nil)
    }

    @Test func newTerminalsStartWithTheDefaultSize() async throws {
        let app = await app()
        app.settings.preferences.defaultSize = .fitPhone
        let terminal = try #require(app.terminal(machine: machine.id, session: sessionA))
        #expect(terminal.sizePreference == .fitPhone)
        #expect(terminal.fitsPhone)
    }

    @Test func renamingTheDeviceReachesEveryConnection() async throws {
        let app = await app()
        #expect(app.settings.deviceName == "iPhone")
        app.settings.setDeviceName("  书房的 iPhone ")
        #expect(app.settings.deviceName == "书房的 iPhone")
        #expect(await eventually { link.deviceNames == ["书房的 iPhone"] })
        // 清空或者填回系统的名字，就是不再自己起名字。
        app.settings.setDeviceName("iPhone")
        #expect(try #require(store.load()).deviceName == nil)
        #expect(await eventually { link.deviceNames == ["书房的 iPhone", "iPhone"] })
        // 没变时不再转给连接。
        app.settings.setDeviceName("")
        try? await Task.sleep(for: .milliseconds(20))
        #expect(link.deviceNames.count == 2)
    }

    @Test func pairingFromSettingsWaitsForTheSheetToClose() async {
        let app = await app()
        app.showingSettings = true
        app.pairsAfterSettings = true
        app.showingSettings = false
        #expect(app.pairing == nil)
        app.settingsDismissed()
        #expect(app.pairing != nil)
        #expect(!app.pairsAfterSettings)
    }
}
