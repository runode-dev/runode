import Foundation
import RunodeProtocol
import Testing

/// 宿主（Rust）`runode_protocol::push` 那批照规格写出的推送样例：电脑经 APNs 推来的等回答、任务做完的通知。
enum PushSamples {
    static let repository: URL = URL(filePath: #filePath)
        .deletingLastPathComponent()  // RunodeProtocolTests
        .deletingLastPathComponent()  // Tests
        .deletingLastPathComponent()  // RunodeKit
        .deletingLastPathComponent()  // ios
        .deletingLastPathComponent()  // apps
        .deletingLastPathComponent()  // 仓库根
    static let directory = repository.appending(path: "crates/protocol/tests/fixtures/push")

    static func data(_ name: String) throws -> Data {
        try Data(contentsOf: directory.appending(path: name))
    }

    /// 整条 APNs payload，系统交给 App 的 `userInfo` 就是它。
    static func payload(_ name: String) throws -> [AnyHashable: Any] {
        try #require(JSONSerialization.jsonObject(with: data(name)) as? [AnyHashable: Any], "\(name)")
    }
}

private let sampleMachine = UUID(uuidString: "6F9619FF-8B86-D011-B42D-00C04FC964FF")!
private let sampleSession = "0123456789abcdef0011223344556677"

@Suite struct PushTests {
    /// 点开通知时按 `aps` 外面的 `machine`、`session` 打开那个会话；同一台电脑的通知按 `thread-id` 归在一组。
    @Test(arguments: ["apns_notify.json", "apns_notify_unnamed.json", "apns_todo_done.json"])
    func notificationsCarryTheSession(_ name: String) throws {
        let payload = try PushSamples.payload(name)
        #expect(SessionLink(userInfo: payload) == SessionLink(machine: sampleMachine, session: sampleSession))
        let aps = try #require(payload["aps"] as? [String: Any])
        #expect(aps["thread-id"] as? String == sampleMachine.uuidString)
    }

    /// 通知的标题、副标题由手机按 loc-key 查 App 的 String Catalog 拼出来：样例里用到的键每种语言都要有，
    /// 占位符的个数和 loc-args 的个数一样。
    @Test(arguments: ["apns_notify.json", "apns_notify_unnamed.json", "apns_todo_done.json"])
    func alertKeysAreLocalized(_ name: String) throws {
        let aps = try #require(PushSamples.payload(name)["aps"] as? [String: Any])
        let alert = try #require(aps["alert"] as? [String: Any])
        let catalogURL = PushSamples.repository.appending(path: "apps/ios/Runode/Localizable.xcstrings")
        let catalog = try #require(
            JSONSerialization.jsonObject(with: Data(contentsOf: catalogURL)) as? [String: Any])
        let strings = try #require(catalog["strings"] as? [String: Any])
        for part in ["title", "subtitle"] {
            let key = try #require(alert["\(part)-loc-key"] as? String, "\(name) \(part)")
            let args = (alert["\(part)-loc-args"] as? [String])?.count ?? 0
            let entry = try #require(strings[key] as? [String: Any], "\(key) 不在 catalog 里")
            let localizations = try #require(entry["localizations"] as? [String: Any])
            for language in ["zh-Hans", "en", "zh-Hant"] {
                let value = try #require(
                    ((localizations[language] as? [String: Any])?["stringUnit"] as? [String: Any])?["value"]
                        as? String, "\(key) 缺 \(language)")
                let placeholders = value.matches(of: /%(\d+\$)?@/).count
                #expect(placeholders == args, "\(key) 的 \(language)：\(value)")
            }
        }
    }

    /// 官方 App 向中转服务换句柄的请求体和回话。
    @Test func relayRegistrationMatchesTheSamples() throws {
        let register = RelayRegister(
            env: .production, token: "80f0c8b3a4e2d1c0ffeeddccbbaa99887766554433221100aabbccddeeff0011",
            machine: sampleMachine.uuidString)
        #expect(try sameJSON(JSONEncoder().encode(register), PushSamples.data("relay_register.json")))
        let handle = try JSONDecoder().decode(RelayHandle.self, from: PushSamples.data("relay_handle.json"))
        #expect(handle.handle == "q2b9Xk0Lr7mVt3pZw8sYc1nH4dF6gJ-aE5uR_oTiBlM")
        let error = try JSONDecoder().decode(PushError.self, from: PushSamples.data("push_error.json"))
        #expect(error.reason == "BadDeviceToken")
    }

    @Test func otherNotificationsHaveNoSession() {
        #expect(SessionLink(userInfo: [:]) == nil)
        #expect(SessionLink(userInfo: ["machine": "not-a-uuid", "session": sampleSession]) == nil)
        #expect(SessionLink(userInfo: ["machine": sampleMachine.uuidString, "session": ""]) == nil)
        #expect(SessionLink(userInfo: ["machine": sampleMachine.uuidString]) == nil)
    }
}

@Suite struct SessionLinkTests {
    @Test func deepLinkRoundTrips() throws {
        let link = SessionLink(machine: sampleMachine, session: sampleSession)
        let url = try #require(link.url)
        #expect(
            url.absoluteString
                == "runode://session?machine=6F9619FF-8B86-D011-B42D-00C04FC964FF&session=0123456789abcdef0011223344556677")
        #expect(SessionLink(url: url) == link)
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
        #expect(link.machine == sampleMachine)
        #expect(link.session == "abc")
    }
}
