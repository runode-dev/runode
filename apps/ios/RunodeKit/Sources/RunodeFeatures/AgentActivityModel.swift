import Foundation
import RunodeActivity
import RunodeProtocol

#if os(iOS)
    import ActivityKit
#endif

extension AgentActivityContent {
    /// 从各台电脑的会话列表算出 Live Activity 的内容：`machines` 按电脑的先后，只给连着的电脑；结束了的
    /// 会话、没有 agent 的会话、agent 状态不认识的会话不算。
    public init(machines: [(id: UUID, name: String, sessions: [SessionInfo])]) {
        let entries = machines.flatMap { machine in
            machine.sessions.compactMap { session -> Entry? in
                guard !session.exited, let agent = session.meta.agent else { return nil }
                let state: AgentActivityState
                switch agent.state {
                case .blocked: state = .blocked
                case .working: state = .working
                case .idle: state = .idle
                case .unknown: return nil
                }
                return Entry(
                    id: "\(machine.id)/\(session.id)", machine: machine.name, title: Presentation.sessionTitle(session),
                    agent: agent.kind.displayName, state: state)
            }
        }
        self.init(summarizing: entries)
    }
}

/// 灵动岛和锁屏上那个 Live Activity 的系统接口：iOS 上是 ActivityKit（`SystemAgentActivity`），测试换成假的。
@MainActor
public protocol AgentActivityDriver: AnyObject {
    /// 系统允许本 App 显示 Live Activity，用户能在系统设置里关掉。
    var isAllowed: Bool { get }
    /// 有本 App 的 Live Activity 在显示，包括 App 上次运行时留下的。
    var isShowing: Bool { get }
    /// 新开一个。只有 App 在前台时开得成，开不成时抛错。
    func start(_ content: AgentActivityContent) throws
    /// 更新在显示的那个；不止一个时留一个、其余结束。`staleDate` 之后系统把内容当成过时的。
    func update(_ content: AgentActivityContent, staleDate: Date?) async
    /// 结束本 App 所有的 Live Activity，马上从灵动岛和锁屏上拿掉。
    func end() async
}

/// 管那个 Live Activity：设置里打开着、系统允许、有带 agent 的会话时显示；内容变了才更新；没有 agent 了
/// 就结束。只能在前台新开，在后台只更新已经开着的。连接停了（`suspend`）时改成「已暂停」，显示停之前
/// 最后的样子，回到前台（`resume`）后再跟着实时的内容走。
///
/// 对系统的调用一个接一个做（`settle` 等它们做完），每次做之前按最新的状态重新算该怎么办，来得太勤的
/// 变化合并成一次。
@MainActor
public final class AgentActivityModel {
    /// 用户在设置里打开了「在灵动岛显示 agent 状态」。关掉时马上结束。
    public var enabled: Bool {
        didSet {
            if enabled != oldValue { apply() }
        }
    }

    /// 上次送出去、现在在显示的内容；没有在显示时为空。
    public private(set) var shown: AgentActivityContent?

    private let driver: (any AgentActivityDriver)?
    private let now: () -> Date
    private var foreground = true
    /// 连着的电脑上实时的内容。
    private var live = AgentActivityContent()
    /// 各台电脑都连上并收到了列表，或者确定连不上：这时 `live` 是空的才说明真没有 agent 了。刚启动、
    /// 刚回到前台还在连时为假，那时不因为 `live` 是空的就结束，免得上次留下的、暂停着的那个刚回来就
    /// 被拿掉又重开。
    private var settled = false
    /// 连接停了的时刻；连着时为空。
    private var suspendedAt: Date?
    private var dirty = false
    private var worker: Task<Void, Never>?

    /// `driver` 为空时什么都不显示（测试、演示模式，以及 macOS 上）。
    public init(driver: (any AgentActivityDriver)?, enabled: Bool, now: @escaping () -> Date = { .now }) {
        self.driver = driver
        self.enabled = enabled
        self.now = now
    }

    /// 各台电脑的会话列表变了。
    public func refresh(_ content: AgentActivityContent, settled: Bool) {
        live = content
        self.settled = settled
        apply()
    }

    /// App 回到前台或进了后台。
    public func setForeground(_ isForeground: Bool) {
        foreground = isForeground
        apply()
    }

    /// 连接停了：改成「已暂停」，并标上过时的时刻。
    public func suspend() {
        guard suspendedAt == nil else { return }
        suspendedAt = now()
        apply()
    }

    /// 连接又开了：恢复跟着实时的内容走。
    public func resume() {
        guard suspendedAt != nil else { return }
        suspendedAt = nil
        apply()
    }

    /// 等对系统的调用都做完。
    public func settle() async {
        while let worker {
            await worker.value
        }
    }

    /// 该把 Live Activity 弄成什么样。
    private enum Target {
        case end
        /// 不动它：连接停了而什么都没显示，或者还在连、不知道有没有 agent。
        case keep
        case show(AgentActivityContent, staleDate: Date?)
    }

    private var target: Target {
        guard enabled, let driver, driver.isAllowed else { return .end }
        if let suspendedAt {
            return shown.map { .show($0.asPaused, staleDate: suspendedAt) } ?? .keep
        }
        if live.isEmpty { return settled ? .end : .keep }
        return .show(live, staleDate: nil)
    }

    private func apply() {
        dirty = true
        guard worker == nil else { return }
        worker = Task { [weak self] in
            while let model = self, model.dirty {
                model.dirty = false
                await model.reconcile()
            }
            self?.worker = nil
        }
    }

    private func reconcile() async {
        guard let driver else { return }
        switch target {
        case .keep:
            break
        case .end:
            if driver.isShowing || shown != nil {
                await driver.end()
            }
            shown = nil
        case .show(let content, let staleDate):
            if driver.isShowing {
                guard content != shown else { return }
                await driver.update(content, staleDate: staleDate)
                shown = content
            } else if foreground {
                // 没在显示（第一次，或者被系统、用户拿掉了）：前台时新开一个。
                do {
                    try driver.start(content)
                    shown = content
                } catch {
                    shown = nil
                }
            } else {
                shown = nil
            }
        }
    }
}

#if os(iOS)
    /// 用 ActivityKit 显示那个 Live Activity。小组件扩展按 `AgentActivityAttributes` 画它。
    ///
    /// `Activity` 不是 `Sendable`，对它的异步调用放在不隔离的静态函数里，每次现取，不在主线程上留着。
    public final class SystemAgentActivity: AgentActivityDriver {
        public init() {}

        public var isAllowed: Bool {
            ActivityAuthorizationInfo().areActivitiesEnabled
        }

        public var isShowing: Bool {
            !Self.showing().isEmpty
        }

        public func start(_ content: AgentActivityContent) throws {
            _ = try Activity.request(
                attributes: AgentActivityAttributes(), content: ActivityContent(state: content, staleDate: nil))
        }

        public func update(_ content: AgentActivityContent, staleDate: Date?) async {
            await Self.update(content, staleDate: staleDate)
        }

        public func end() async {
            await Self.endAll()
        }

        /// 本 App 还在显示的 Live Activity。
        private nonisolated static func showing() -> [Activity<AgentActivityAttributes>] {
            Activity<AgentActivityAttributes>.activities.filter {
                $0.activityState == .active || $0.activityState == .stale
            }
        }

        private nonisolated static func update(_ content: AgentActivityContent, staleDate: Date?) async {
            let activities = showing()
            guard let first = activities.first else { return }
            // 上次运行留下了不止一个：接着用第一个，其余结束，不叠着显示。
            for extra in activities.dropFirst() {
                await extra.end(nil, dismissalPolicy: .immediate)
            }
            await first.update(ActivityContent(state: content, staleDate: staleDate))
        }

        private nonisolated static func endAll() async {
            for activity in Activity<AgentActivityAttributes>.activities {
                await activity.end(nil, dismissalPolicy: .immediate)
            }
        }
    }
#endif
