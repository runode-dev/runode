import Foundation

#if canImport(ActivityKit) && os(iOS)
    import ActivityKit
#endif

/// 灵动岛和锁屏上那个 Live Activity 的固定部分。整个 App 只有一个，汇总所有连着的电脑上带 agent 的
/// 会话，没有什么开了以后不变的东西，内容全在 `ContentState` 里。App 和小组件扩展各自链接这个 target，
/// 两边按同一个类型对上同一个 activity。
public struct AgentActivityAttributes: Hashable, Sendable, Codable {
    public typealias ContentState = AgentActivityContent

    public init() {}
}

#if canImport(ActivityKit) && os(iOS)
    extension AgentActivityAttributes: ActivityAttributes {}
#endif

/// agent 的状态，只留 Live Activity 要分的三种；宿主新加的、不认识的状态不进来。
public enum AgentActivityState: String, Hashable, Sendable, Codable, CaseIterable {
    /// 停下来等用户回答。
    case blocked
    case working
    case idle

    /// 条目排序的先后：等回答的最要紧，其次在干活的。
    var rank: Int {
        switch self {
        case .blocked: 0
        case .working: 1
        case .idle: 2
        }
    }
}

/// Live Activity 每次更新带过去的内容：三种状态各几个、最要紧的几条，以及连接是不是停了。
/// 系统限制一次更新的内容不能太大（几 KB），所以条目有上限、文字截短。
public struct AgentActivityContent: Hashable, Sendable, Codable {
    /// 一个带 agent 的会话。
    public struct Entry: Hashable, Sendable, Codable, Identifiable {
        /// 哪台电脑上的哪个会话，界面上区分条目用。
        public var id: String
        /// 电脑的名字。
        public var machine: String
        /// 会话的标题。
        public var title: String
        /// agent 给人看的名字（「Claude Code」）。
        public var agent: String
        public var state: AgentActivityState
        /// 在干活时代替通用图标画的字：这个 agent 自己转圈里的一帧，和 App、桌面上的一样（Claude 的
        /// `✻`）。Live Activity 里不能逐帧动，只画一帧。
        public var spinner: String?
        /// 那个字的颜色（0xRRGGBB）；为空时用前景色。
        public var spinnerColor: UInt32?

        public init(
            id: String, machine: String, title: String, agent: String, state: AgentActivityState,
            spinner: String? = nil, spinnerColor: UInt32? = nil
        ) {
            self.id = id
            self.machine = machine
            self.title = title
            self.agent = agent
            self.state = state
            self.spinner = spinner
            self.spinnerColor = spinnerColor
        }
    }

    /// 最多带几条。
    public static let entryLimit = 4
    /// 电脑名、标题最多带几个字，免得一次更新的内容超出系统的限制。
    public static let textLimit = 48

    public var blocked: Int
    public var working: Int
    public var idle: Int
    /// 等回答的在前、再是在干活的、最后是空闲的，同一种按电脑的先后和电脑上的先后；最多 `entryLimit` 条。
    public var entries: [Entry]
    /// App 进了后台、连接停了：显示的是停之前最后的样子，回到 App 才会刷新。
    public var paused: Bool
    /// 整体是在干活（`overall` 为 `working`）时，从什么时候起一直是：灵动岛上按它显示系统自己走的计时。
    /// 宿主不报状态变化的时刻，这是 App 头一次看到的时刻，由 `AgentActivityModel` 填。
    public var workingSince: Date?

    public init(
        blocked: Int = 0, working: Int = 0, idle: Int = 0, entries: [Entry] = [], paused: Bool = false,
        workingSince: Date? = nil
    ) {
        self.blocked = blocked
        self.working = working
        self.idle = idle
        self.entries = entries
        self.paused = paused
        self.workingSince = workingSince
    }

    /// 从所有带 agent 的会话（按电脑的先后、电脑上的先后排好）算出：数各种状态的个数，按 `rank` 挑出
    /// 最要紧的 `limit` 条，电脑名和标题截到 `textLimit` 个字。
    public init(summarizing all: [Entry], limit: Int = entryLimit) {
        self.init()
        for entry in all {
            switch entry.state {
            case .blocked: blocked += 1
            case .working: working += 1
            case .idle: idle += 1
            }
        }
        // 先按原来的位置编号再排，同一种状态保持原来的先后。
        entries = all.enumerated()
            .sorted { ($0.element.state.rank, $0.offset) < ($1.element.state.rank, $1.offset) }
            .prefix(max(limit, 0))
            .map { _, entry in
                var entry = entry
                entry.machine = Self.clipped(entry.machine)
                entry.title = Self.clipped(entry.title)
                return entry
            }
    }

    /// 整体在干活时，排在最前面的那个在干活的会话：灵动岛上按它的转圈画整体的图标。
    public var leadingWorker: Entry? {
        overall == .working ? entries.first { $0.state == .working } : nil
    }

    /// 一个带 agent 的会话都没有。
    public var isEmpty: Bool {
        blocked + working + idle == 0
    }

    /// 整体的状态，灵动岛上那个图标按它画：有等回答的就是等回答，否则有在干活的就是干活，都没有是空闲。
    public var overall: AgentActivityState {
        if blocked > 0 { return .blocked }
        if working > 0 { return .working }
        return .idle
    }

    /// 灵动岛右边那个数：先看等回答的，没有时看在干活的，都没有时是空闲的个数。
    public var headline: Int {
        switch overall {
        case .blocked: blocked
        case .working: working
        case .idle: idle
        }
    }

    /// 同样的内容，标成连接已暂停。
    public var asPaused: AgentActivityContent {
        var content = self
        content.paused = true
        return content
    }

    private static func clipped(_ text: String) -> String {
        text.count > textLimit ? String(text.prefix(textLimit - 1)) + "…" : text
    }
}
