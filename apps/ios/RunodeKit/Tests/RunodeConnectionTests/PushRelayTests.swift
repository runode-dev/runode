import Foundation
import RunodeConnection
import Testing

@Suite struct PushRelayTests {
    @Test func baseComesFromTheInfoPlist() {
        let local = HTTPPushRelay(info: [HTTPPushRelay.infoKey: " https://relay.test:8443 "])
        #expect(local.base == URL(string: "https://relay.test:8443"))
        // 没写、build setting 没展开、不是 https：用官方的。
        for raw in [nil, "$(RUNODE_PUSH_RELAY)", "", "http://relay.test"] {
            let info: [String: Any] = raw.map { [HTTPPushRelay.infoKey: $0] } ?? [:]
            #expect(HTTPPushRelay(info: info).base == HTTPPushRelay.official.base, "\(raw ?? "nil")")
        }
    }
}
