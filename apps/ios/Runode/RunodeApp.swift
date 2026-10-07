import RunodeConnection
import RunodeFeatures
import SwiftUI
import UIKit

@main
struct RunodeApp: App {
    @State private var app = AppModel(dependencies: AppComposition.dependencies())

    var body: some Scene {
        WindowGroup {
            RootView(app: app)
                .task { await AppComposition.prepare(app) }
        }
    }
}

/// 把真的依赖组装起来：Keychain 里的设备私钥、Application Support 里的电脑列表、Bonjour 发现、
/// 每台电脑一条 `HostConnection`。
@MainActor
enum AppComposition {
    static func dependencies() -> AppDependencies {
        #if DEBUG
            if let demo = DemoComposition.dependenciesIfRequested() {
                return demo
            }
        #endif
        let keyStore = KeychainDeviceKeyStore()
        let store = FileMachineStore(url: FileMachineStore.defaultURL())
        let discovery = BonjourDiscovery()
        let preferences = DefaultsStore<AppPreferences>("preferences")
        let systemName = UIDevice.current.name
        return AppDependencies(
            store: store, keyStore: keyStore, pairing: RemotePairing(keyStore: keyStore, discovery: discovery),
            // 设备名按建连接时存着的设置取；之后改了由 `AppModel` 转给已有的连接。
            makeLink: { machine in
                HostConnection(
                    machine: machine, keyStore: keyStore, machines: store, discovery: discovery,
                    identity: ClientIdentity(
                        build: buildIdentifier, deviceName: preferences.load()?.deviceName ?? systemName))
            },
            deviceName: systemName, recents: DefaultsStore("recentTerminal"), preferences: preferences,
            themes: DefaultsStore("theme"))
    }

    /// 启动后要做的事；演示模式下直接打开演示的终端。
    static func prepare(_ app: AppModel) async {
        await app.machineList.load()
        #if DEBUG
            DemoComposition.openIfRequested(app)
        #endif
    }

    /// 报给宿主的构建标识，固定不变。宿主只拿它和自己的比，一样才给快照；桌面端的是
    /// `<版本>.<提交号>`，和这个永远对不上，所以手机一律用 VT 重放。
    nonisolated static let buildIdentifier = "dev.runode.mobile"
}
