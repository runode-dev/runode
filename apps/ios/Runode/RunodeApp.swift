import RunodeConnection
import RunodeFeatures
import SwiftUI
import UIKit

@main
struct RunodeApp: App {
    @UIApplicationDelegateAdaptor private var delegate: AppDelegate

    var body: some Scene {
        WindowGroup {
            RootView(app: delegate.app)
                .task { await AppComposition.prepare(delegate.app) }
        }
    }
}

/// App delegate：系统把 APNs 的 device token 交到这里，转给通知的系统接口。整个 App 的状态也由它持有，
/// 点开通知时好打开那个会话；它在 App 启动完之前建好，冷启动时点开的那条通知也接得住。
final class AppDelegate: NSObject, UIApplicationDelegate {
    let notifications: SystemPushNotifications
    let app: AppModel

    override init() {
        let notifications = SystemPushNotifications()
        let app = AppModel(dependencies: AppComposition.dependencies(notifications: notifications))
        notifications.onOpen = { [weak app] link in app?.open(link) }
        notifications.shows = { [weak app] link in app?.showsNotification(for: link) ?? true }
        self.notifications = notifications
        self.app = app
        super.init()
    }

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        notifications.received(token: deviceToken)
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: any Error) {
        notifications.failed(error: error)
    }
}

/// 把真的依赖组装起来：Keychain 里的设备私钥、Application Support 里的电脑列表、Bonjour 发现、
/// 每台电脑一条 `HostConnection`，以及登记推送用的通知系统接口和 Info.plist 里的 APNs 环境。
@MainActor
enum AppComposition {
    static func dependencies(notifications: (any PushNotifications)?) -> AppDependencies {
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
        var dependencies = AppDependencies(
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
        dependencies.notifications = notifications
        dependencies.pushIdentity = PushIdentity(
            info: Bundle.main.infoDictionary ?? [:], bundle: Bundle.main.bundleIdentifier)
        return dependencies
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
