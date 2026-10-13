import Foundation
import Observation
import RunodeConnection
import RunodeProtocol

#if os(iOS)
    import UIKit
    import UserNotifications
#endif

/// 通知的系统接口：iOS 上是 UserNotifications 和 `UIApplication`（`SystemPushNotifications`），测试换成
/// 假的。等回答的通知由电脑经 APNs 推来，App 只要通知权限、拿 device token、在前台时清掉已经答完的。
@MainActor
public protocol PushNotifications: AnyObject {
    /// 要通知权限：没问过时弹系统的询问框，问过的直接给现在的结果。允许时为真。
    func requestAuthorization() async -> Bool
    /// 向 APNs 要 device token，拿到后从 `deviceTokens` 给出来。
    func registerForRemoteNotifications()
    /// device token：每拿到一次给一次，可能和上次一样。
    func deviceTokens() -> AsyncStream<Data>
    /// 向 APNs 要 device token 失败了，每次给出原因。
    func registrationFailures() -> AsyncStream<String>
    /// 通知中心里还留着的等回答通知。
    func delivered() async -> [DeliveredNotification]
    /// 从通知中心删掉这些（`DeliveredNotification.id`）。
    func removeDelivered(_ ids: [String])
}

/// 通知中心里的一条等回答通知。
public struct DeliveredNotification: Hashable, Sendable {
    /// 系统给的标识。
    public var id: String
    /// 它是哪台电脑上哪个会话的。
    public var link: SessionLink

    public init(id: String, link: SessionLink) {
        self.id = id
        self.link = link
    }
}

/// 登记推送时报给电脑的 App 身份：APNs 环境和 bundle id。
public struct PushIdentity: Hashable, Sendable {
    /// Info.plist 里记 APNs 环境的键，值来自 build setting `RUNODE_APS_ENVIRONMENT`。
    public static let environmentKey = "RunodeAPSEnvironment"

    public var environment: ApnsEnv
    public var bundle: String

    public init(environment: ApnsEnv, bundle: String) {
        self.environment = environment
        self.bundle = bundle
    }

    /// 从 App 的 Info.plist 读；没写环境、写的不认识、没有 bundle id 时为空，那就不登记推送。
    public init?(info: [String: Any], bundle: String?) {
        guard let raw = info[Self.environmentKey] as? String,
            let environment = ApnsEnv(rawValue: raw.trimmingCharacters(in: .whitespaces).lowercased()),
            let bundle, !bundle.isEmpty
        else { return nil }
        self.init(environment: environment, bundle: bundle)
    }
}

/// 向各台电脑登记推送：电脑上有会话的 agent 停下来等回答时，经 APNs 用这里交出的 device token 给手机
/// 推一条通知，手机不用连着。
///
/// 打开着提醒时，每台电脑连上（`ready`）都要一次通知权限：第一次弹系统的询问框，之后直接给结果，用户在
/// 系统设置里改了也就跟着知道了。权限有了结果才登记，免得用缓存的 token 抢在前面。允许了才向 APNs 要
/// device token；拿到 token、系统允许时对连着的电脑登记，token 变了再登记一次。系统不允许时注销（`token`
/// 为空的同一条消息）：电脑记着登记，不注销的话它会接着把问题内容推给 APNs。设置里关掉提醒时同样对连着的
/// 电脑注销，之后连上的也注销；再打开时重新登记。
///
/// 电脑回 `Done` 算办好了；回带编号的 `Error` 记下原因，只在设置页里显示，不弹错误；`replyTimeout` 内
/// 没回话就不再等。
@Observable
@MainActor
public final class PushRegistration {
    /// 一台电脑上登记的结果。
    public enum Status: Hashable, Sendable {
        /// 电脑收下了这次登记（或注销）。
        case registered
        /// 电脑回了错误。
        case failed(String)
    }

    /// 各台电脑最近一次登记的结果；还没结果的没有。断开后留着，重新登记有了结果再换。
    public private(set) var statuses: [UUID: Status] = [:]
    /// 系统设置里允许本 App 发通知；还没问过、不知道（没有系统接口）时当作允许。
    public private(set) var systemAllowed = true
    /// 最近一次向 APNs 要 device token 失败的原因；之后拿到 token 时清掉。设置页据此提示。
    public private(set) var tokenError: String?

    /// 设置里打开了等回答提醒。
    public var enabled: Bool {
        didSet {
            guard enabled != oldValue else { return }
            if enabled {
                authorize(registering: Array(peers.keys))
            } else {
                registerAll()
            }
        }
    }

    /// 等电脑回话最多等多久。
    @ObservationIgnored let replyTimeout: Duration
    @ObservationIgnored private let system: (any PushNotifications)?
    @ObservationIgnored private let identity: PushIdentity?
    /// 现在的 device token（十六进制）；还没拿到时为空。
    @ObservationIgnored private(set) var token: String?
    /// 连着、能发消息的电脑。
    @ObservationIgnored private var peers: [UUID: Peer] = [:]
    /// 每台电脑在等回话的那次登记；新登记一次就换掉旧的。
    @ObservationIgnored private var pending: [UUID: Pending] = [:]
    @ObservationIgnored private var attempts = 0

    private struct Peer {
        var name: String
        var link: any HostLink
    }

    private struct Pending {
        var attempt: Int
        /// 取到请求编号、发出去以后才有。
        var req: UInt32?
        var task: Task<Void, Never>
    }

    /// `system` 或 `identity` 为空时什么都不登记（测试、演示模式，以及 macOS 上）。
    public init(
        system: (any PushNotifications)?, identity: PushIdentity?, enabled: Bool,
        replyTimeout: Duration = .seconds(5)
    ) {
        self.system = system
        self.identity = identity
        self.enabled = enabled
        self.replyTimeout = replyTimeout
        guard let system else { return }
        let tokens = system.deviceTokens()
        let failures = system.registrationFailures()
        Task { [weak self] in
            for await data in tokens {
                guard let self else { return }
                self.tokenChanged(data)
            }
        }
        Task { [weak self] in
            for await reason in failures {
                guard let self else { return }
                self.tokenError = reason
            }
        }
    }

    /// 这台电脑连上了（`HostEvent.ready`）：登记一次；打开着提醒时先看系统还允不允许发通知，有了结果再登记。
    func connected(_ machine: MachineRecord, link: any HostLink) {
        peers[machine.id] = Peer(name: machine.name, link: link)
        if enabled {
            authorize(registering: [machine.id])
        } else {
            register(machine.id)
        }
    }

    /// 这台电脑断开了：不再等它的回话。
    func disconnected(_ id: UUID) {
        peers[id] = nil
        pending.removeValue(forKey: id)?.task.cancel()
    }

    /// 这台电脑在手机上改了名字：连着的话按新名字再登记一次，推送里带的电脑名跟着变。
    func renamed(_ machine: MachineRecord) {
        guard var peer = peers[machine.id], peer.name != machine.name else { return }
        peer.name = machine.name
        peers[machine.id] = peer
        register(machine.id)
    }

    /// 这台电脑要删掉了：连着的话先注销，等电脑回话（最多 `timeout`）再返回，免得删掉以后电脑上的 agent
    /// 等回答时还往这部手机推它的提醒，点开又找不到这台电脑。没连着时马上返回。
    func unregister(_ id: UUID, timeout: Duration) async {
        guard peers[id] != nil else { return }
        register(id, unregistering: true, timeout: timeout)
        // 回话到了（`handle`）、断开了（`disconnected`）或者超时，等回话的任务都会结束。
        await pending[id]?.task.value
    }

    /// 这台电脑删掉了：连它的结果一起忘掉。
    func forget(_ id: UUID) {
        disconnected(id)
        statuses[id] = nil
    }

    /// 这台电脑发来的消息：对上在等的那次登记的回话就记下结果。
    func handle(_ message: HostMsg, from id: UUID) {
        guard let req = pending[id]?.req else { return }
        switch message {
        case .done(req):
            pending.removeValue(forKey: id)?.task.cancel()
            statuses[id] = .registered
        case .error(.some(req), _, let text):
            pending.removeValue(forKey: id)?.task.cancel()
            statuses[id] = .failed(text)
        default:
            break
        }
    }

    private func tokenChanged(_ data: Data) {
        tokenError = nil
        let hex = data.map { String(format: "%02x", $0) }.joined()
        guard hex != token else { return }
        token = hex
        // 系统不允许时已经注销过了，新 token 用不上。
        guard enabled, systemAllowed else { return }
        registerAll()
    }

    /// 要通知权限，记下系统允不允许，允许时向 APNs 要 token；然后对 `ids` 这几台登记（不允许时是注销）。
    /// 允不允许变了时对连着的都重来一遍（token 没变时 `tokenChanged` 不会再登记）。
    private func authorize(registering ids: [UUID]) {
        guard let system, identity != nil else { return }
        Task { [weak self] in
            let allowed = await system.requestAuthorization()
            guard let self else { return }
            let changed = self.systemAllowed != allowed
            self.systemAllowed = allowed
            if allowed { system.registerForRemoteNotifications() }
            guard self.enabled else { return }
            if changed {
                self.registerAll()
            } else {
                for id in ids { self.register(id) }
            }
        }
    }

    private func registerAll() {
        for id in peers.keys {
            register(id)
        }
    }

    /// 对这台电脑登记（关着提醒、系统不允许发通知或者 `unregistering` 时注销）。打开着提醒却还没拿到 token 时
    /// 先不发，拿到了再发。`timeout` 为空时等 `replyTimeout`。
    private func register(_ id: UUID, unregistering: Bool = false, timeout: Duration? = nil) {
        guard let peer = peers[id], let identity, system != nil else { return }
        let token: String?
        if enabled && systemAllowed && !unregistering {
            guard let current = self.token else { return }
            token = current
        } else {
            token = nil
        }
        pending.removeValue(forKey: id)?.task.cancel()
        attempts += 1
        let attempt = attempts
        let timeout = timeout ?? replyTimeout
        let task = Task { [weak self] in
            let req = await peer.link.nextRequestId()
            guard let self, self.pending[id]?.attempt == attempt else { return }
            self.pending[id]?.req = req
            peer.link.send(
                .pushRegister(
                    req: req, token: token, env: identity.environment, bundle: identity.bundle,
                    machine: id.uuidString, machineName: peer.name))
            try? await Task.sleep(for: timeout)
            guard !Task.isCancelled, self.pending[id]?.attempt == attempt else { return }
            self.pending[id] = nil
        }
        pending[id] = Pending(attempt: attempt, req: nil, task: task)
    }
}

#if os(iOS)
    /// 用 UserNotifications 要权限、清通知，用 `UIApplication` 要 device token；也是通知中心的 delegate：
    /// App 在前台时照样弹横幅、响声音，点开通知时交给 `onOpen`。
    ///
    /// device token 由系统交给 App delegate，App delegate 转给 `received(token:)`。要在 App 启动完之前建好，
    /// 冷启动时点开的那条通知才交得到这里。
    @MainActor
    public final class SystemPushNotifications: NSObject, PushNotifications,
        @preconcurrency UNUserNotificationCenterDelegate
    {
        /// 点开了一条等回答的通知。
        public var onOpen: @MainActor (SessionLink) -> Void = { _ in }
        private let tokens: AsyncStream<Data>
        private let tokenSink: AsyncStream<Data>.Continuation
        private let failures: AsyncStream<String>
        private let failureSink: AsyncStream<String>.Continuation

        public override init() {
            (tokens, tokenSink) = AsyncStream.makeStream(of: Data.self)
            (failures, failureSink) = AsyncStream.makeStream(of: String.self)
            super.init()
            UNUserNotificationCenter.current().delegate = self
        }

        /// App delegate 拿到的 device token。
        public func received(token: Data) {
            tokenSink.yield(token)
        }

        /// App delegate 收到的要 device token 失败（`didFailToRegisterForRemoteNotificationsWithError`）。
        public func failed(error: any Error) {
            failureSink.yield(error.localizedDescription)
        }

        public func requestAuthorization() async -> Bool {
            (try? await UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound])) ?? false
        }

        public func registerForRemoteNotifications() {
            UIApplication.shared.registerForRemoteNotifications()
        }

        public func deviceTokens() -> AsyncStream<Data> {
            tokens
        }

        public func registrationFailures() -> AsyncStream<String> {
            failures
        }

        public func delivered() async -> [DeliveredNotification] {
            await Self.delivered()
        }

        public func removeDelivered(_ ids: [String]) {
            UNUserNotificationCenter.current().removeDeliveredNotifications(withIdentifiers: ids)
        }

        private nonisolated static func delivered() async -> [DeliveredNotification] {
            await UNUserNotificationCenter.current().deliveredNotifications().compactMap { notification in
                SessionLink(userInfo: notification.request.content.userInfo).map {
                    DeliveredNotification(id: notification.request.identifier, link: $0)
                }
            }
        }

        public func userNotificationCenter(
            _ center: UNUserNotificationCenter, willPresent notification: UNNotification
        ) async -> UNNotificationPresentationOptions {
            [.banner, .list, .sound]
        }

        // 跟着类在主线程上跑：系统交来的完成回调在方法返回的地方调，UIKit 收到后马上更新 App 快照，不在
        // 主线程上时断言失败，App 在后台时点开通知就闪退。
        public func userNotificationCenter(
            _ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse
        ) async {
            guard response.actionIdentifier == UNNotificationDefaultActionIdentifier,
                let link = SessionLink(userInfo: response.notification.request.content.userInfo)
            else { return }
            onOpen(link)
        }
    }
#endif
