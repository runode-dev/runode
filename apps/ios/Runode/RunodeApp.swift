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

/// 把真的依赖组装起来：Keychain 里的设备私钥、Application Support 里的 Mac 列表、Bonjour 发现、
/// 每台 Mac 一条 `HostConnection`。
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
        let identity = ClientIdentity(
            build: buildIdentifier(info: Bundle.main.infoDictionary ?? [:]), deviceName: UIDevice.current.name)
        return AppDependencies(
            store: store, keyStore: keyStore, pairing: RemotePairing(keyStore: keyStore, discovery: discovery),
            makeLink: { machine in
                HostConnection(
                    machine: machine, keyStore: keyStore, machines: store, discovery: discovery, identity: identity)
            },
            deviceName: identity.deviceName)
    }

    /// 启动后要做的事；演示模式下直接打开演示的终端，调试用的启动参数见 `DebugLaunch`。
    static func prepare(_ app: AppModel) async {
        await app.machineList.load()
        #if DEBUG
            DemoComposition.openIfRequested(app)
            DebugLaunch.apply(app)
        #endif
    }

    /// 报给宿主的构建标识：`ios-<版本>+<构建号>`。宿主只拿它判断能不能用快照，iOS 的永远不能。
    nonisolated static func buildIdentifier(info: [String: Any]) -> String {
        let version = info["CFBundleShortVersionString"] as? String ?? "0"
        let build = info["CFBundleVersion"] as? String ?? "0"
        return "ios-\(version)+\(build)"
    }
}
