import Foundation
import RunodeConnection
import Testing

@Suite struct PushRelayTests {
    @Test func baseComesFromTheInfoPlist() {
        let local = HTTPPushRelay(info: [HTTPPushRelay.infoKey: " https://relay.test:8443 "])
        #expect(local?.base == URL(string: "https://relay.test:8443"))
        // 没写、build setting 没展开、不是 https：不经中转。
        for raw in [nil, "$(RUNODE_PUSH_RELAY)", "", "http://relay.test", "https://:8443"] {
            let info: [String: Any] = raw.map { [HTTPPushRelay.infoKey: $0] } ?? [:]
            #expect(HTTPPushRelay(info: info) == nil, "\(raw ?? "nil")")
        }
    }
}
