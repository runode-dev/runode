import Foundation
import Observation
import RunodeActivity
import RunodeConnection
import RunodeProtocol

#if os(iOS)
    import ActivityKit
#endif

/// 灵动岛和锁屏上那些 Live Activity 的系统接口：iOS 上是 ActivityKit（`SystemLiveActivities`），测试换成
/// 假的。Live Activity 由电脑经推送起、更新和收起，App 只拿 push-to-start token、看系统开关、在前台时
/// 收起过时的。
@MainActor
public protocol LiveActivities: AnyObject {
    /// push-to-start token：有的话先给现在的，之后每次变了再给。
    func pushToStartTokens() -> AsyncStream<Data>
    /// 系统设置里允许本 App 显示 Live Activity。
    var areEnabled: Bool { get }
    /// `areEnabled` 之后的每次变化。
    func enablementUpdates() -> AsyncStream<Bool>
    /// 本地正在显示（包括已过时）的那些的 attributes。
    var shown: [AgentActivityAttributes] { get }
    /// 马上收起这台电脑（`AgentActivityAttributes.machine`）上这个会话的。
    func end(machine: String, session: String) async
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

/// 向各台电脑登记推送：电脑上有会话的 agent 停下来等回答时，经 APNs 用这里交出的 push-to-start token
/// 在手机上起一个 Live Activity，之后由电脑更新、收起，手机不用连着。
///
/// 一启动就开始等 push-to-start token（被推送在后台唤醒时也一样）；每台电脑连上（`ready`）时登记一次，
/// token 变了对连着的电脑再登记一次。设置里关掉提醒时对连着的电脑注销（`token` 为空的同一条消息），
/// 之后连上的也注销；再打开时重新登记。
///
/// 电脑回 `Done` 算办好了；回带编号的 `Error` 记下原因；`replyTimeout` 内什么都没回（老版本的 runode
/// 不认识这条消息，回不带编号的 `Error` 或者不回）就当它不支持推送。这些都只在设置页里显示，不弹错误。
@Observable
@MainActor
public final class PushRegistration {
    /// 一台电脑上登记的结果。
    public enum Status: Hashable, Sendable {
        /// 电脑收下了这次登记（或注销）。
        case registered
        /// 电脑上的 runode 太旧，不认识登记推送。
        case unsupported
        /// 电脑回了错误。
        case failed(String)
    }

    /// 各台电脑最近一次登记的结果；还没结果的没有。断开后留着，重新登记有了结果再换。
    public private(set) var statuses: [UUID: Status] = [:]
    /// 系统设置里允许本 App 显示 Live Activity；不知道（没有系统接口）时当作允许。
    public private(set) var systemAllowed = true

    /// 设置里打开了等回答提醒。
    public var enabled: Bool {
        didSet {
            guard enabled != oldValue else { return }
            for id in peers.keys {
                register(id)
            }
        }
    }

    /// 等电脑回话最多等多久。
    @ObservationIgnored let replyTimeout: Duration
    @ObservationIgnored private let system: (any LiveActivities)?
    @ObservationIgnored private let identity: PushIdentity?
    /// 现在的 push-to-start token（十六进制）；还没拿到时为空。
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
        system: (any LiveActivities)?, identity: PushIdentity?, enabled: Bool, replyTimeout: Duration = .seconds(5)
    ) {
        self.system = system
        self.identity = identity
        self.enabled = enabled
        self.replyTimeout = replyTimeout
        guard let system else { return }
        systemAllowed = system.areEnabled
        let tokens = system.pushToStartTokens()
        let enablement = system.enablementUpdates()
        Task { [weak self] in
            for await data in tokens {
                guard let self else { return }
                self.tokenChanged(data)
            }
        }
        Task { [weak self] in
            for await allowed in enablement {
                guard let self else { return }
                self.systemAllowed = allowed
            }
        }
    }

    /// 这台电脑连上了（`HostEvent.ready`）：登记一次。
    func connected(_ machine: MachineRecord, link: any HostLink) {
        peers[machine.id] = Peer(name: machine.name, link: link)
        register(machine.id)
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
        let hex = data.map { String(format: "%02x", $0) }.joined()
        guard hex != token else { return }
        token = hex
        guard enabled else { return }
        for id in peers.keys {
            register(id)
        }
    }

    /// 对这台电脑登记（关着提醒时注销）。打开着提醒却还没拿到 token 时先不发，拿到了再发。
    private func register(_ id: UUID) {
        guard let peer = peers[id], let identity, system != nil else { return }
        let token: String?
        if enabled {
            guard let current = self.token else { return }
            token = current
        } else {
            token = nil
        }
        pending.removeValue(forKey: id)?.task.cancel()
        attempts += 1
        let attempt = attempts
        let timeout = replyTimeout
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
            self.statuses[id] = .unsupported
        }
        pending[id] = Pending(attempt: attempt, req: nil, task: task)
    }
}

#if os(iOS)
    /// 用 ActivityKit 拿 push-to-start token、看系统开关、收起本地的 Live Activity。小组件扩展按
    /// `AgentActivityAttributes` 画它们。
    ///
    /// `Activity` 不是 `Sendable`，对它的调用放在不隔离的静态函数里，每次现取，不在主线程上留着。
    public final class SystemLiveActivities: LiveActivities {
        public init() {}

        public func pushToStartTokens() -> AsyncStream<Data> {
            Self.pushToStartTokens()
        }

        public var areEnabled: Bool {
            ActivityAuthorizationInfo().areActivitiesEnabled
        }

        public func enablementUpdates() -> AsyncStream<Bool> {
            Self.enablementUpdates()
        }

        public var shown: [AgentActivityAttributes] {
            Self.showing().map(\.attributes)
        }

        public func end(machine: String, session: String) async {
            await Self.end(machine: machine, session: session)
        }

        private nonisolated static func pushToStartTokens() -> AsyncStream<Data> {
            AsyncStream { continuation in
                let task = Task {
                    if let current = Activity<AgentActivityAttributes>.pushToStartToken {
                        continuation.yield(current)
                    }
                    for await data in Activity<AgentActivityAttributes>.pushToStartTokenUpdates {
                        continuation.yield(data)
                    }
                    continuation.finish()
                }
                continuation.onTermination = { _ in task.cancel() }
            }
        }

        private nonisolated static func enablementUpdates() -> AsyncStream<Bool> {
            AsyncStream { continuation in
                let task = Task {
                    for await allowed in ActivityAuthorizationInfo().activityEnablementUpdates {
                        continuation.yield(allowed)
                    }
                    continuation.finish()
                }
                continuation.onTermination = { _ in task.cancel() }
            }
        }

        private nonisolated static func showing() -> [Activity<AgentActivityAttributes>] {
            Activity<AgentActivityAttributes>.activities.filter {
                $0.activityState == .active || $0.activityState == .stale
            }
        }

        private nonisolated static func end(machine: String, session: String) async {
            for activity in showing()
            where activity.attributes.machine == machine && activity.attributes.session == session {
                await activity.end(nil, dismissalPolicy: .immediate)
            }
        }
    }
#endif
