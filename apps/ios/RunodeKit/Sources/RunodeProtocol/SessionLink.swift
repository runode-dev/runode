import Foundation

/// 要打开的某台电脑上的某个会话。两处来的：点开电脑推来的等回答通知（`init(userInfo:)`），以及系统交来的
/// 深链接 `runode://session?machine=<UUID>&session=<会话编号>`（`init(url:)`）。
public struct SessionLink: Hashable, Sendable {
    /// 链接里 `runode://` 后面的那一段。
    public static let host = "session"

    /// 手机登记推送时给这台电脑编的 UUID（`MachineRecord.id`）。
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

    /// 从电脑推来的等回答通知的 `userInfo` 读：宿主的 `push::notify_payload` 在 `aps` 外面带着 `machine`
    /// （登记时报的 UUID）和 `session`。不是这种通知、`machine` 不是 UUID、`session` 为空时为空。
    public init?(userInfo: [AnyHashable: Any]) {
        guard let machine = (userInfo["machine"] as? String).flatMap(UUID.init(uuidString:)),
            let session = userInfo["session"] as? String, !session.isEmpty
        else { return nil }
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
