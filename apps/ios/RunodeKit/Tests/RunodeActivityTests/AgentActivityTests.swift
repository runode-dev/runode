import Foundation
import RunodeActivity
import Testing

/// 宿主（Rust）`runode_protocol::push` 那批照规格写出的推送样例：Live Activity 的 attributes、
/// content-state 和 APNs payload。
enum PushSamples {
    static let directory: URL = URL(filePath: #filePath)
        .deletingLastPathComponent()  // RunodeActivityTests
        .deletingLastPathComponent()  // Tests
        .deletingLastPathComponent()  // RunodeKit
        .deletingLastPathComponent()  // ios
        .deletingLastPathComponent()  // apps
        .deletingLastPathComponent()  // 仓库根
        .appending(path: "crates/protocol/tests/fixtures/push")

    static func data(_ name: String) throws -> Data {
        try Data(contentsOf: directory.appending(path: name))
    }

    /// APNs payload 里 `aps` 下面的一项，重新编成 JSON。
    static func aps(_ name: String, _ key: String) throws -> Data {
        let root = try #require(JSONSerialization.jsonObject(with: data(name)) as? [String: Any])
        let aps = try #require(root["aps"] as? [String: Any])
        return try JSONSerialization.data(withJSONObject: #require(aps[key]))
    }
}

/// 两段 JSON 解出来一样（不管键的先后和空白）。
private func sameJSON(_ a: Data, _ b: Data) throws -> Bool {
    let left = try JSONSerialization.jsonObject(with: a) as? NSObject
    let right = try JSONSerialization.jsonObject(with: b) as? NSObject
    return left == right
}

@Suite struct AgentActivityTests {
    @Test func attributesMatchTheHostSamples() throws {
        let sample = try PushSamples.data("attributes.json")
        let attributes = try JSONDecoder().decode(AgentActivityAttributes.self, from: sample)
        #expect(
            attributes
                == AgentActivityAttributes(
                    machine: "6F9619FF-8B86-D011-B42D-00C04FC964FF", machineName: "Ethan 的 MacBook Pro",
                    session: "0123456789abcdef0011223344556677"))
        #expect(try sameJSON(JSONEncoder().encode(attributes), sample))
    }

    @Test func contentStateMatchesTheHostSamples() throws {
        let sample = try PushSamples.data("content_state.json")
        let state = try JSONDecoder().decode(AgentActivityAttributes.ContentState.self, from: sample)
        #expect(state.title == "修 bug")
        #expect(state.agent == "Claude Code")
        #expect(state.lines == ["Do you want to proceed?", "❯ 1. Yes", "  2. No, and tell Claude what to do differently (esc)"])
        #expect(try sameJSON(JSONEncoder().encode(state), sample))
    }

    /// 起、更新、收起的 payload 里的内容和起的 payload 里的 attributes 都解得出来。
    @Test(arguments: ["apns_start.json", "apns_update.json", "apns_end.json"])
    func payloadsDecode(_ name: String) throws {
        let state = try JSONDecoder().decode(
            AgentActivityAttributes.ContentState.self, from: PushSamples.aps(name, "content-state"))
        #expect(state.agent == "Claude Code")
        if name == "apns_end.json" {
            #expect(state.lines.isEmpty)
        }
        if name == "apns_start.json" {
            let attributes = try JSONDecoder().decode(
                AgentActivityAttributes.self, from: PushSamples.aps(name, "attributes"))
            #expect(attributes.machineName == "Ethan 的 MacBook Pro")
        }
    }

    @Test func missingLinesAreEmpty() throws {
        let state = try JSONDecoder().decode(
            AgentActivityAttributes.ContentState.self, from: Data(#"{"title":"修 bug","agent":"Codex"}"#.utf8))
        #expect(state == AgentActivityAttributes.ContentState(title: "修 bug", agent: "Codex", lines: []))
    }

    @Test func deepLinkRoundTrips() throws {
        let attributes = AgentActivityAttributes(
            machine: "6F9619FF-8B86-D011-B42D-00C04FC964FF", machineName: "MacBook",
            session: "0123456789abcdef0011223344556677")
        let link = try #require(attributes.link)
        let url = try #require(link.url)
        #expect(
            url.absoluteString
                == "runode://session?machine=6F9619FF-8B86-D011-B42D-00C04FC964FF&session=0123456789abcdef0011223344556677")
        #expect(SessionLink(url: url) == link)
        // 电脑带回来的不是 UUID：不给链接，点卡片只打开 App。
        #expect(AgentActivityAttributes(machine: "x", machineName: "MacBook", session: "00").link == nil)
    }

    @Test(arguments: [
        "runode://pair?v=1&name=MacBook",
        "runode://session?session=0123456789abcdef0011223344556677",
        "runode://session?machine=not-a-uuid&session=0123456789abcdef0011223344556677",
        "runode://session?machine=6F9619FF-8B86-D011-B42D-00C04FC964FF&session=",
        "https://session?machine=6F9619FF-8B86-D011-B42D-00C04FC964FF&session=00",
    ])
    func otherLinksAreNotSessionLinks(_ text: String) throws {
        #expect(SessionLink(url: try #require(URL(string: text))) == nil)
    }

    @Test func hostAndSchemeIgnoreCase() throws {
        let url = try #require(
            URL(string: "RUNODE://Session?machine=6f9619ff-8b86-d011-b42d-00c04fc964ff&session=abc"))
        let link = try #require(SessionLink(url: url))
        #expect(link.machine == UUID(uuidString: "6F9619FF-8B86-D011-B42D-00C04FC964FF"))
        #expect(link.session == "abc")
    }
}
