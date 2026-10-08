import Foundation

#if canImport(ActivityKit) && os(iOS)
    import ActivityKit
#endif

/// agent 停下来等回答时，会话所在的电脑经 APNs 推送弹出的 Live Activity（灵动岛、锁屏），一个会话一个。
/// 由电脑用 push-to-start token 起、往广播频道里推更新和收起，App 自己不起。这是开了以后不变的部分。
///
/// 线上格式以宿主的 `push::ActivityAttributes`、`push::ActivityContent` 为准：APNs 按类型名
/// （`push::ATTRIBUTES_TYPE`）找到这个类型，用 Swift 默认的 `Codable` 解，键名就是属性名。App 和小组件
/// 扩展各自链接这个 target，两边按同一个类型对上同一个 activity。
public struct AgentActivityAttributes: Hashable, Sendable, Codable {
    /// 手机登记推送时给这台电脑报的 UUID（`MachineRecord.id`），电脑原样带回。
    public var machine: String
    /// 登记时报的电脑名。
    public var machineName: String
    /// 会话编号，32 个十六进制数字。
    public var session: String

    public init(machine: String, machineName: String, session: String) {
        self.machine = machine
        self.machineName = machineName
        self.session = session
    }

    /// 每次推送带的内容。
    public struct ContentState: Hashable, Sendable, Codable {
        /// 会话的标题。
        public var title: String
        /// agent 给人看的名字（「Claude Code」）。
        public var agent: String
        /// 屏幕底部的问题和选项，最多几行；电脑没带屏幕文字时为空。
        public var lines: [String]

        public init(title: String, agent: String, lines: [String] = []) {
            self.title = title
            self.agent = agent
            self.lines = lines
        }

        // 电脑那边总会写 `lines`，缺了也当成没有，不让整条推送解不出来。
        public init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            title = try container.decode(String.self, forKey: .title)
            agent = try container.decode(String.self, forKey: .agent)
            lines = try container.decodeIfPresent([String].self, forKey: .lines) ?? []
        }
    }

    /// 点卡片时打开的那个会话；`machine` 不是 UUID 时为空。
    public var link: SessionLink? {
        UUID(uuidString: machine).map { SessionLink(machine: $0, session: session) }
    }
}

#if canImport(ActivityKit) && os(iOS)
    extension AgentActivityAttributes: ActivityAttributes {}
#endif

/// 打开某台电脑上某个会话的深链接：`runode://session?machine=<UUID>&session=<会话编号>`。Live Activity
/// 的卡片点开时由系统交给 App。
public struct SessionLink: Hashable, Sendable {
    /// 链接里 `runode://` 后面的那一段。
    public static let host = "session"

    public var machine: UUID
    /// 会话编号，原样带着；对不对由打开它的一方核对。
    public var session: String

    public init(machine: UUID, session: String) {
        self.machine = machine
        self.session = session
    }

    /// 认出这种链接；别的链接、缺参数、`machine` 不是 UUID 时为空。
    public init?(url: URL) {
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
            components.scheme?.lowercased() == "runode", components.host?.lowercased() == Self.host
        else { return nil }
        let items = components.queryItems ?? []
        func value(_ name: String) -> String? {
            items.first { $0.name == name }?.value.flatMap { $0.isEmpty ? nil : $0 }
        }
        guard let machine = value("machine").flatMap(UUID.init(uuidString:)), let session = value("session") else {
            return nil
        }
        self.init(machine: machine, session: session)
    }

    public var url: URL? {
        var components = URLComponents()
        components.scheme = "runode"
        components.host = Self.host
        components.queryItems = [
            URLQueryItem(name: "machine", value: machine.uuidString), URLQueryItem(name: "session", value: session),
        ]
        return components.url
    }
}
