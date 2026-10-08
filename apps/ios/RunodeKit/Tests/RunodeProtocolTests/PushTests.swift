import Foundation
import Testing

/// 宿主（Rust）`runode_protocol::push` 那批照规格写出的推送样例：Live Activity 的 attributes、
/// content-state 和 APNs payload。小组件那边的 Live Activity 类型用 Swift 默认的 `Codable` 解它们，这里
/// 用同样写法的两个类型核对键名：读得出来、写回去和样例一样。
enum PushSamples {
    static let directory: URL = URL(filePath: #filePath)
        .deletingLastPathComponent()  // RunodeProtocolTests
        .deletingLastPathComponent()  // Tests
        .deletingLastPathComponent()  // RunodeKit
        .deletingLastPathComponent()  // ios
        .deletingLastPathComponent()  // apps
        .deletingLastPathComponent()  // 仓库根
        .appending(path: "crates/protocol/tests/fixtures/push")

    static func data(_ name: String) throws -> Data {
        try Data(contentsOf: directory.appending(path: name))
    }
}

/// 和 Rust 的 `ActivityAttributes` 对得上的默认 `Codable` 写法。
private struct Attributes: Hashable, Codable {
    var machine: String
    var machineName: String
    var session: String
}

/// 和 Rust 的 `ActivityContent` 对得上的默认 `Codable` 写法。
private struct ContentState: Hashable, Codable {
    var title: String
    var agent: String
    var lines: [String]
}

@Suite struct PushTests {
    @Test func attributesAndContentUseDefaultCodableKeys() throws {
        let attributes = try PushSamples.data("attributes.json")
        let decoded = try JSONDecoder().decode(Attributes.self, from: attributes)
        #expect(decoded.machineName == "Ethan 的 MacBook Pro")
        #expect(decoded.session == "0123456789abcdef0011223344556677")
        #expect(try sameJSON(JSONEncoder().encode(decoded), attributes))

        let content = try PushSamples.data("content_state.json")
        let state = try JSONDecoder().decode(ContentState.self, from: content)
        #expect(state.agent == "Claude Code")
        #expect(state.lines.count == 3)
        #expect(try sameJSON(JSONEncoder().encode(state), content))
    }

    /// APNs payload 里的 attributes 和 content-state 也是这个样子，起 Live Activity 时按类型名找类型；
    /// 起来的 activity 订阅广播频道（`input-push-channel`），不要 update token。
    @Test(arguments: ["apns_start.json", "apns_update.json", "apns_end.json"])
    func payloadsCarryTheSameShapes(_ name: String) throws {
        let root = try #require(
            JSONSerialization.jsonObject(with: PushSamples.data(name)) as? [String: Any], "\(name)")
        let aps = try #require(root["aps"] as? [String: Any], "\(name)")
        let state = try JSONSerialization.data(withJSONObject: #require(aps["content-state"]))
        _ = try JSONDecoder().decode(ContentState.self, from: state)
        if name == "apns_start.json" {
            #expect(aps["attributes-type"] as? String == "AgentActivityAttributes")
            let attributes = try JSONSerialization.data(withJSONObject: #require(aps["attributes"]))
            _ = try JSONDecoder().decode(Attributes.self, from: attributes)
            #expect(aps["input-push-channel"] is String)
            #expect(aps["input-push-token"] == nil)
        }
    }
}
